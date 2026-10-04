#!/usr/bin/env python3
"""Edit a `rec export --tighten --plan-out` plan with TypeSafe Jev judgments.

For each settled or end screen, one request asks two independent questions:
  read_need   (Score) how much of the newly appeared text a viewer of this demo needs to read
  undoes_prev (Noul)  whether the next input undoes the input that produced this screen

read_need sets the screen's hold through HOLD_TABLE. A screen whose next input undoes the input
that produced it, with undoes_prev above UNDO_THRESHOLD, is cut along with that input and its
undo, so the video goes from the screen before the mistake straight to the corrected one.

Usage: jev_tighten.py plan.json [--purpose TEXT] [--out plan.jev.json] [--answers jev_answers.json]

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


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("plan")
    ap.add_argument("--purpose", help="what the video is about; defaults to the plan's purpose")
    ap.add_argument("--out", help="edited plan path (default: <plan>.jev.json)")
    ap.add_argument("--answers", help="raw answers path (default: jev_answers.json beside --out)")
    args = ap.parse_args()

    plan = json.load(open(args.plan))
    purpose = args.purpose or plan.get("purpose") or ""
    if not purpose:
        sys.exit("jev_tighten: give --purpose or fill the plan's purpose")
    plan["purpose"] = purpose
    out_path = args.out or re.sub(r"\.json$", "", args.plan) + ".jev.json"
    answers_path = args.answers or os.path.join(os.path.dirname(out_path) or ".", "jev_answers.json")

    key = api_key()
    todo = screens(plan)
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        replies = list(pool.map(lambda t: ask(key, state_for(purpose, *t)), todo))

    by_id = {s["id"]: s for s in plan["segments"]}
    policy_out_len = {s["id"]: s["out_len"] for s in plan["segments"]}
    records = []
    drops = set()
    ids = [s["id"] for s in plan["segments"]]
    quiet_ids = [s["id"] for s in plan["segments"] if "screen_text" in s]
    for (seg, prev, nxt), (reply, latency) in zip(todo, replies):
        read = reply["answers"]["read_need"]
        undo = reply["answers"]["undoes_prev"]
        usage = reply.get("usage", {})
        hold = hold_for(read["score"], seg.get("words") or 0)
        if seg["kind"] == "end":
            hold = max(hold, END_MIN)
        flagged = nxt is not None and undo["noul"] >= UNDO_THRESHOLD
        dropped = []
        if flagged:
            # Everything after the screen before the mistake, up to the corrected screen.
            first = quiet_ids[quiet_ids.index(seg["id"]) - 1] + 1 if prev else 0
            dropped = [i for i in ids if first <= i < nxt["id"]]
            drops.update(dropped)
        print(
            f"segment {seg['id']:>3} {seg['kind']:<7} read_need {read['score']:.2f} "
            f"(conf {read['confidence']:.2f}) undoes_prev {undo['noul']:.2f} hold {hold:.2f}s "
            f"{'DROP ' + str(dropped) + ' ' if dropped else ''}"
            f"tokens {usage.get('input_tokens')}/{usage.get('output_tokens')} {latency * 1000:.0f}ms",
            file=sys.stderr,
        )
        by_id[seg["id"]]["out_len"] = hold
        records.append(
            {
                "id": seg["id"],
                "kind": seg["kind"],
                "policy_out_len": policy_out_len[seg["id"]],
                "jev_out_len": hold,
                "read_need": read,
                "undoes_prev": undo,
                "undo_flagged": flagged,
                "drops": dropped,
                "model": reply.get("model"),
                "usage": usage,
                "latency_s": round(latency, 3),
            }
        )
    for i in drops:
        by_id[i]["drop"] = True

    json.dump(plan, open(out_path, "w"), indent=2, ensure_ascii=False)
    tokens_in = sum(r["usage"].get("input_tokens", 0) for r in records)
    tokens_out = sum(r["usage"].get("output_tokens", 0) for r in records)
    summary = {
        "calls": len(records),
        "input_tokens": tokens_in,
        "output_tokens": tokens_out,
        "cost_usd": tokens_in / 1e6 * PRICE_PER_MTOK_USD,
        "latency_s_total": round(sum(r["latency_s"] for r in records), 3),
        "latency_s_max": max((r["latency_s"] for r in records), default=0),
        "dropped_segments": sorted(drops),
    }
    json.dump(
        {
            "plan": args.plan,
            "purpose": purpose,
            "questions": QUESTIONS,
            "hold_table": HOLD_TABLE,
            "end_min": END_MIN,
            "undo_threshold": UNDO_THRESHOLD,
            "summary": summary,
            "screens": records,
        },
        open(answers_path, "w"),
        indent=2,
        ensure_ascii=False,
    )
    print(json.dumps({"plan": out_path, "answers": answers_path, **summary}))


if __name__ == "__main__":
    main()
