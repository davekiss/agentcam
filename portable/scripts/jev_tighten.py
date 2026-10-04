#!/usr/bin/env python3
"""Edit a `rec export --tighten --plan-out` plan with TypeSafe Jev judgments.

For each settled or end screen, one request asks two independent questions:
  read_need   (Score) how much of the newly appeared text a viewer of this demo needs to read
  undoes_prev (Noul)  whether the next input undoes the input that produced this screen

read_need sets the screen's hold through HOLD_TABLE. A screen whose next input undoes the input
that produced it, with undoes_prev above UNDO_THRESHOLD, is cut along with that input and its
undo. When the screen after the undo only restores the screen before the mistake (same_screen),
it is cut too, so the video goes from the screen before the mistake straight to the next real
input; --no-restore-rule keeps it. --hold-scale multiplies every non-end hold.

Raw answers are saved to --answers. --from-answers reapplies the policy to saved answers without
calling Jev, so variants of one take differ only by policy.

Usage: jev_tighten.py plan.json [--purpose TEXT] [--out plan.jev.json] [--answers jev_answers.json]
                      [--from-answers jev_answers.json] [--no-restore-rule] [--hold-scale 1.0]

The API key comes from TYPESAFE_API_KEY, else from the repo's .env.local. It is never printed.
"""

import argparse
import concurrent.futures
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

ENDPOINT = "https://api.typesafe.ai/v1/systemone"
MODEL = "jev-latest"
ENV_FILE = "/Users/davekiss/Code/Projects/rec/.env.local"
PRICE_PER_MTOK_USD = 0.042  # docs.typesafe.ai/models: input tokens only, output is free.

# One row per read_need level: (base seconds, seconds per unread word, cap seconds).
# The hold interpolates each column between the two levels around the score, then takes
# min(base + per_word * words, cap). Every column rises with the level, so the hold is
# monotone in both the score and the amount of new text.
HOLD_TABLE = [
    (1.0, 0.00, 1.0),  # 0 nothing new to read
    (1.0, 0.02, 1.5),  # 1 boilerplate or startup chrome
    (1.2, 0.08, 3.0),  # 2 transitional confirmation
    (2.0, 0.20, 6.0),  # 3 a choice the viewer should notice
    (4.0, 0.25, 10.0),  # 4 the payoff
]
END_MIN = 2.0  # The final screen holds at least this long, as in rec's policy.
UNDO_THRESHOLD = 0.8
HOLD_SCALE = 1.0  # Multiplies every non-end hold. End screens keep the END_MIN floor unscaled.

QUESTIONS = {
    "read_need": {
        "type": "score",
        "instructions": (
            "A viewer is watching a short screen-recording demo whose subject is `video_purpose`. "
            "`this_screen` just appeared after `input_that_produced_this_screen`; "
            "`newly_appeared_text` is the text on it the viewer has not seen before, and "
            "`previous_screen` is what was on screen before. How much of the newly appeared "
            "text does the viewer need to read to follow the demo?"
        ),
        "criteria": [
            "Nothing worth reading: no new text, or the new text is only the echo of what was "
            "just typed, a partly typed command, or an empty prompt.",
            "Boilerplate the viewer can skip: a startup banner, version or account line, status "
            "bar, keyboard hints, or tool chrome unrelated to the demo's subject.",
            "A transitional confirmation the viewer only glances at: a short acknowledgement "
            "that a step ran, a prompt coming back, a loading or progress line.",
            "A choice or prompt the viewer should notice: a menu, question, list of options, or "
            "setup output that the next step responds to.",
            "The key result or payoff the demo exists to show: the output that demonstrates "
            "`video_purpose` actually working.",
        ],
    },
    "undoes_prev": {
        "type": "noul",
        "instructions": (
            "Does `next_input` undo or reverse the effect of `input_that_produced_this_screen`, "
            "so that `screen_after_next_input` puts things back the way they were in "
            "`previous_screen`?"
        ),
        "criteria": {
            "true": "The input that produced this screen was a mistake or an accidental action, "
            "and the next input cancels, clears, deletes, or reverses it: for example clearing "
            "a mistyped command with ctrl+u, toggling a setting back off, or pressing Escape "
            "to back out of something opened by mistake.",
            "false": "The next input continues the demo: it confirms, answers, runs, or builds "
            "on this screen, even when it changes the screen a lot.",
        },
    },
}


def api_key():
    key = os.environ.get("TYPESAFE_API_KEY")
    if key:
        return key
    try:
        with open(ENV_FILE) as f:
            for line in f:
                name, sep, value = line.strip().partition("=")
                if sep and name.strip().removeprefix("export ").strip() == "TYPESAFE_API_KEY":
                    return value.strip().strip("'\"")
    except OSError:
        pass
    sys.exit(f"jev_tighten: TYPESAFE_API_KEY is not set and not found in {ENV_FILE}")


def ask(key, state):
    body = json.dumps({"state": state, "model": MODEL, "questions": QUESTIONS}).encode()
    req = urllib.request.Request(
        ENDPOINT,
        data=body,
        method="POST",
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
    )
    for attempt in range(6):
        start = time.monotonic()
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                reply = json.load(resp)
            return reply, time.monotonic() - start
        except urllib.error.HTTPError as e:
            if e.code in (429, 529) and attempt < 5:
                time.sleep(float(e.headers.get("retry-after") or 2**attempt))
                continue
            detail = e.read().decode(errors="replace")[:500]
            raise SystemExit(f"jev_tighten: HTTP {e.code}: {detail}")


def hold_for(score, words):
    lo = min(int(score), len(HOLD_TABLE) - 2)
    f = score - lo
    base, per_word, cap = (
        a + f * (b - a) for a, b in zip(HOLD_TABLE[lo], HOLD_TABLE[lo + 1])
    )
    return min(base + per_word * words, cap)


def tidy(screen):
    """Collapses runs of blank lines, which carry no meaning for the model."""
    return re.sub(r"\n\s*\n(\s*\n)+", "\n\n", screen or "")


def screens(plan):
    """Each settled or end segment with the context the questions need."""
    segs = plan["segments"]
    quiet = [s for s in segs if "screen_text" in s]
    out = []
    for i, seg in enumerate(quiet):
        if seg["kind"] not in ("settled", "end"):
            continue
        prev = quiet[i - 1] if i > 0 else None
        nxt = quiet[i + 1] if i + 1 < len(quiet) else None
        out.append((seg, prev, nxt))
    return out


def state_for(purpose, seg, prev, nxt):
    return {
        "video_purpose": purpose,
        "previous_screen": tidy(prev["screen_text"]) if prev else "",
        "input_that_produced_this_screen": seg["inputs"],
        "this_screen": tidy(seg["screen_text"]),
        "newly_appeared_text": seg["new_text"],
        "next_input": nxt["inputs"] if nxt else [],
        "screen_after_next_input": tidy(nxt["screen_text"]) if nxt else "",
    }


def normalize(screen):
    """Screen lines with trailing spaces stripped, blank runs collapsed, trailing blanks dropped."""
    out = []
    for line in (screen or "").split("\n"):
        line = line.rstrip()
        if line or (out and out[-1]):
            out.append(line)
    while out and not out[-1]:
        out.pop()
    return out


def same_screen(before, after, after_inputs):
    """Whether the quiet screen `after` only restores the quiet screen `before`.

    After normalize(), they are the same when identical, or when they share every line but the
    last and that shared part is not empty, so a cursor or status line may differ (less shows
    "Don't use line numbers  (press RETURN)" there after -N). The last line may not differ by
    text that `after_inputs` typed, so a screen already showing the retyped command is kept.
    """
    a, b = normalize(before), normalize(after)
    if a == b:
        return True
    if len(a) < 2 or a[:-1] != b[:-1]:
        return False
    typed = [i[len('type "') : -1] for i in after_inputs if i.startswith('type "')]
    return not any(t and t in b[-1] for t in typed)


def apply_policy(plan, answers, restore_rule=True, hold_scale=HOLD_SCALE):
    """Edits `plan` in place from Jev's answers keyed by segment id, and returns one record per screen.

    A screen flagged as undone drops everything after the screen before the mistake, up to the
    screen after the undo. With `restore_rule`, the screen after the undo is dropped too when it
    is a settled screen that only restores the screen before the mistake, so the video cuts from
    the pre-mistake screen to the next real input, whose preroll rec keeps.
    """
    by_id = {s["id"]: s for s in plan["segments"]}
    ids = [s["id"] for s in plan["segments"]]
    quiet_ids = [s["id"] for s in plan["segments"] if "screen_text" in s]
    records = []
    for seg, prev, nxt in screens(plan):
        read = answers[seg["id"]]["read_need"]
        undo = answers[seg["id"]]["undoes_prev"]
        hold = hold_for(read["score"], seg.get("words") or 0)
        hold = max(hold, END_MIN) if seg["kind"] == "end" else hold * hold_scale
        flagged = nxt is not None and undo["noul"] >= UNDO_THRESHOLD
        dropped = []
        restored = None
        if flagged:
            first = quiet_ids[quiet_ids.index(seg["id"]) - 1] + 1 if prev else 0
            dropped = [i for i in ids if first <= i < nxt["id"]]
            if (
                restore_rule
                and prev is not None
                and nxt["kind"] == "settled"
                and same_screen(prev["screen_text"], nxt["screen_text"], nxt["inputs"])
            ):
                restored = nxt["id"]
                dropped.append(restored)
        records.append(
            {
                "id": seg["id"],
                "kind": seg["kind"],
                "policy_out_len": seg["out_len"],
                "jev_out_len": hold,
                "read_need": read["score"],
                "undoes_prev": undo["noul"],
                "undo_flagged": flagged,
                "drops": dropped,
                "restored_dropped": restored,
            }
        )
    for r in records:
        by_id[r["id"]]["out_len"] = r["jev_out_len"]
        for i in r["drops"]:
            by_id[i]["drop"] = True
    return records


def fetch_answers(plan, purpose):
    key = api_key()
    todo = screens(plan)
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        replies = list(pool.map(lambda t: ask(key, state_for(purpose, *t)), todo))
    return [
        {
            "id": seg["id"],
            "kind": seg["kind"],
            "read_need": reply["answers"]["read_need"],
            "undoes_prev": reply["answers"]["undoes_prev"],
            "model": reply.get("model"),
            "usage": reply.get("usage", {}),
            "latency_s": round(latency, 3),
        }
        for (seg, _, _), (reply, latency) in zip(todo, replies)
    ]


def load_answers(path, plan, purpose):
    cached = json.load(open(path))
    if cached.get("questions") != QUESTIONS:
        sys.exit(f"jev_tighten: {path} answered different questions; rerun without --from-answers")
    if cached.get("purpose") != purpose:
        sys.exit(f"jev_tighten: {path} answered for a different purpose")
    if sorted(s["id"] for s in cached["screens"]) != sorted(seg["id"] for seg, _, _ in screens(plan)):
        sys.exit(f"jev_tighten: {path} does not cover this plan's screens")
    return cached["screens"]


def save_answers(path, plan_path, purpose, answers):
    tokens_in = sum(a["usage"].get("input_tokens", 0) for a in answers)
    summary = {
        "calls": len(answers),
        "input_tokens": tokens_in,
        "output_tokens": sum(a["usage"].get("output_tokens", 0) for a in answers),
        "cost_usd": tokens_in / 1e6 * PRICE_PER_MTOK_USD,
        "latency_s_total": round(sum(a["latency_s"] for a in answers), 3),
        "latency_s_max": max((a["latency_s"] for a in answers), default=0),
    }
    doc = {"plan": plan_path, "purpose": purpose, "questions": QUESTIONS, "summary": summary, "screens": answers}
    json.dump(doc, open(path, "w"), indent=2, ensure_ascii=False)
    return summary


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("plan")
    ap.add_argument("--purpose", help="what the video is about; defaults to the plan's, then the answers'")
    ap.add_argument("--out", help="edited plan path (default: <plan>.jev.json)")
    ap.add_argument("--answers", help="raw answers path (default: jev_answers.json beside --out)")
    ap.add_argument("--from-answers", help="reuse the raw answers in this file instead of calling Jev")
    ap.add_argument("--no-restore-rule", action="store_true", help="keep the screen an undo restores")
    ap.add_argument("--hold-scale", type=float, default=HOLD_SCALE, help="multiplier on every non-end hold")
    args = ap.parse_args()

    plan = json.load(open(args.plan))
    cached_purpose = json.load(open(args.from_answers)).get("purpose") if args.from_answers else None
    purpose = args.purpose or plan.get("purpose") or cached_purpose or ""
    if not purpose:
        sys.exit("jev_tighten: give --purpose or fill the plan's purpose")
    plan["purpose"] = purpose
    out_path = args.out or re.sub(r"\.json$", "", args.plan) + ".jev.json"

    summary = {}
    if args.from_answers:
        answers = load_answers(args.from_answers, plan, purpose)
    else:
        answers = fetch_answers(plan, purpose)
        answers_path = args.answers or os.path.join(os.path.dirname(out_path) or ".", "jev_answers.json")
        summary = {"answers": answers_path, **save_answers(answers_path, args.plan, purpose, answers)}

    restore_rule = not args.no_restore_rule
    records = apply_policy(plan, {a["id"]: a for a in answers}, restore_rule, args.hold_scale)
    for r in records:
        print(
            f"segment {r['id']:>3} {r['kind']:<7} read_need {r['read_need']:.2f} "
            f"undoes_prev {r['undoes_prev']:.2f} hold {r['jev_out_len']:.2f}s"
            + (f" DROP {r['drops']}" if r["drops"] else "")
            + (f" restored {r['restored_dropped']}" if r["restored_dropped"] is not None else ""),
            file=sys.stderr,
        )
    json.dump(plan, open(out_path, "w"), indent=2, ensure_ascii=False)
    print(
        json.dumps(
            {
                "plan": out_path,
                "restore_rule": restore_rule,
                "hold_scale": args.hold_scale,
                "dropped_segments": sorted(i for r in records for i in r["drops"]),
                "restored_dropped": [r["restored_dropped"] for r in records if r["restored_dropped"] is not None],
                **summary,
            }
        )
    )


if __name__ == "__main__":
    main()
