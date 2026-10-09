#!/usr/bin/env bash
# End-to-end check of the tty source: record, drive, observe, stop, export, verify the MP4s.
# Needs: a built agentcam (AGENTCAM=path, default target/release/agentcam), ffmpeg, ffprobe, python3.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
AGENTCAM=${AGENTCAM:-$here/../target/release/agentcam}
OUT=${OUT:-$(mktemp -d "${TMPDIR:-/tmp}/agentcam-smoke.XXXXXX")}
TAKE=""
export BASH_SILENCE_DEPRECATION_WARNING=1

fail() {
  echo "FAIL: $*" >&2
  if [ -n "$TAKE" ] && [ -f "$TAKE/recorder.log" ]; then
    echo "--- recorder.log" >&2
    tail -20 "$TAKE/recorder.log" >&2
  fi
  "$AGENTCAM" stop >/dev/null 2>&1 || true
  exit 1
}

run() {
  echo "+ agentcam $*" >&2
  local out
  out=$("$AGENTCAM" "$@" 2>/dev/null) || fail "agentcam $1 exited nonzero: $out"
  echo "$out" >&2
  printf '%s' "$out"
}

# json <python expression over `j`> reads stdin.
json() {
  python3 -c 'import json,sys; j=json.load(sys.stdin); v='"$1"'; print(v if not isinstance(v,(dict,list)) else json.dumps(v))'
}

check() {
  local what=$1 expr=$2 input=$3
  [ "$(printf '%s' "$input" | json "$expr")" = "True" ] || fail "$what"
  echo "ok: $what" >&2
}

for tool in ffmpeg ffprobe python3; do
  command -v "$tool" >/dev/null || fail "$tool not on PATH"
done
[ -x "$AGENTCAM" ] || fail "agentcam binary not found at $AGENTCAM (cargo build --release)"

started=$(run start --tty --size 100x30 --out "$OUT" -- bash --norc)
TAKE=$(printf '%s' "$started" | json 'j["take"]')
[ -d "$TAKE" ] || fail "start did not create the take folder"

status=$(run status)
check "status reports recording" 'j["recording"] is True and j["take"] == "'"$TAKE"'"' "$status"

run type 'printf "\e[1;35mhello\e[0m\n"; ls --color=always /' >/dev/null
run key Return >/dev/null
# Anchored, so the echoed command line (which also contains "hello") does not count.
run wait --text '(?m)^hello' --timeout 10 >/dev/null || fail "hello never appeared"
# Symbols JetBrains Mono lacks, as octal bytes so bash needs no UTF-8 locale. frame-*.png shows them.
run type --delay 1 'printf "glyphs: \342\234\224 \342\234\227 \342\206\222 \342\206\265 \342\217\272 \342\216\277 \342\227\217 \342\226\266 \342\230\205 \342\243\277\342\240\213\342\240\271 \342\225\255\342\224\200\342\225\256\342\224\202\342\225\260\342\224\200\342\225\257 \342\224\214\342\224\254\342\224\220\342\224\224\342\224\264\342\224\230\n"' >/dev/null
run key Return >/dev/null
run wait --text '(?m)^glyphs: ✔' --timeout 10 >/dev/null || fail "glyph line never appeared"
mark=$(run mark done)
check "mark returns its label" 'j["label"] == "done" and j["t"] > 0' "$mark"
sleep 1

run type 'top' >/dev/null
run key Return >/dev/null
run wait --text 'PID' --timeout 10 >/dev/null || fail "top never drew its header"
sleep 1
top_screen=$(run screen)
check "screen shows top" '"PID" in j["text"]' "$top_screen"
run key q >/dev/null

run type 'echo smoke-$((6*7))' >/dev/null
run key Return >/dev/null
run wait --text 'smoke-42' --timeout 10 >/dev/null || fail "shell did not come back after top"
screen=$(run screen)
check "screen has the grid size and cursor" \
  'j["cols"] == 100 and j["rows"] == 30 and 0 <= j["cursor"]["row"] < 30 and 0 <= j["cursor"]["col"] < 100' "$screen"
check "screen shows the shell output" '"smoke-42" in j["text"]' "$screen"

run type 'echo hello-$((1+1))' >/dev/null
run key Return >/dev/null
# The reply lands before the wait starts; --new still sees it because it counts from the last input.
sleep 0.3
run wait --new --text 'hello-2' --timeout 10 >/dev/null || fail "wait --new missed hello-2"
stale=$("$AGENTCAM" wait --new --text 'echo hello' --timeout 1 2>/dev/null) && fail "wait --new matched the echoed command"
check "wait --new ignores what was echoed before the last input" 'j["error"]["code"] == "timeout"' "$stale"
run wait --text 'echo hello' --timeout 1 >/dev/null || fail "plain wait no longer matches the screen"

run type 'for i in 1 2 3 4 5 6; do echo tick-$i; sleep 0.3; done' >/dev/null
run key Return >/dev/null
idle=$(run wait --idle 1 --timeout 10)
check "wait --idle returns idle" 'j["idle"] is True and j["t"] > 0' "$idle"
check "wait --idle holds until the output stops" '"tick-6" in j["text"]' "$(run screen)"
quick=$("$AGENTCAM" wait --idle 5 --timeout 1 2>/dev/null) && fail "wait --idle outlasted its timeout"
check "wait --idle times out" 'j["error"]["code"] == "timeout"' "$quick"

# Dead air, like an agent thinking between steps, for --tighten to cut.
sleep 3

again=$("$AGENTCAM" start --tty -- true 2>/dev/null) && fail "second start succeeded while recording"
check "second start names the active take" \
  'j["error"]["code"] == "already_recording" and "'"$(basename "$TAKE")"'" in j["error"]["message"]' "$again"

stopped=$(run stop)
check "stop prints a finished take" 'j["status"] == "finished" and j["duration"] > 0 and j["version"] == 2' "$stopped"
check "take.json source is tty 100x30" \
  'j["source"] == {"kind": "tty", "command": ["bash", "--norc"], "size": {"cols": 100, "rows": 30}}' "$stopped"
duration=$(printf '%s' "$stopped" | json 'j["duration"]')

check "stop is idempotent" 'j == {"recording": False}' "$(run stop)"
check "status after stop" 'j == {"recording": False}' "$(run status)"

timeline=$(cat "$TAKE/timeline.json")
check "timeline has the typed command" \
  'any(e["type"] == "type" and "printf" in (e["text"] or "") for e in j["events"])' "$timeline"
check "timeline has the Return key" 'any(e == {"t": e["t"], "type": "key", "key": "Return"} for e in j["events"])' "$timeline"
check "timeline has the q key" 'any(e["type"] == "key" and e["key"] == "q" for e in j["events"])' "$timeline"
check "timeline has the marker" 'any(e["type"] == "marker" and e["label"] == "done" for e in j["events"])' "$timeline"
check "timeline is in time order" '[e["t"] for e in j["events"]] == sorted(e["t"] for e in j["events"])' "$timeline"
[ "$(wc -l <"$TAKE/term.cast")" -gt 10 ] || fail "term.cast has almost no output"

exported=$(run export "$TAKE")
check "export lists both layouts" '[e["layout"] for e in j["exports"]] == ["16:9", "9:16"]' "$exported"

# Half a second after the marker: hello and the colored ls output are on screen, top has not started.
mark_t=$(printf '%s' "$mark" | json 'j["t"] + 0.5')
for spec in "16x9 1920 1080" "9x16 1080 1920"; do
  read -r slug w h <<<"$spec"
  mp4="$TAKE/export-$slug.mp4"
  [ -s "$mp4" ] || fail "$mp4 missing"
  probe=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height,codec_name,pix_fmt:format=duration -of json "$mp4")
  check "$slug is ${w}x${h} h264 yuv420p" \
    '{k: j["streams"][0][k] for k in ("width", "height", "codec_name", "pix_fmt")} == {"width": '"$w"', "height": '"$h"', "codec_name": "h264", "pix_fmt": "yuv420p"}' "$probe"
  check "$slug duration within 0.5s of take ($duration)" \
    'abs(float(j["format"]["duration"]) - '"$duration"') <= 0.5' "$probe"
  ffmpeg -v error -y -ss "$mark_t" -i "$mp4" -frames:v 1 "$TAKE/frame-$slug.png" || fail "frame extract for $slug"
  [ -s "$TAKE/frame-$slug.png" ] || fail "frame-$slug.png missing"
done
check "each export reviews itself with a sheet next to the mp4" \
  'all(e["review"]["sheet"] == e["path"][:-4] + ".sheet.png" and len(e["review"]["activity"]) > 0 and e["review"]["every"] >= 1 for e in j["exports"])' "$exported"
check "the smoke take passes review" \
  'all(c["severity"] != "error" for e in j["exports"] for c in e["review"]["checks"])' "$exported"
check "the dead air before stop is an idle span" \
  'all(any(s["to"] - s["from"] >= 3 for s in e["review"]["idle"]) for e in j["exports"])' "$exported"
for slug in 16x9 9x16; do
  sheet="$TAKE/export-$slug.sheet.png"
  python3 -c 'import sys; b=open(sys.argv[1],"rb").read(); sys.exit(b[:8] != b"\x89PNG\r\n\x1a\n" or len(b) < 1000)' "$sheet" ||
    fail "$sheet is not a PNG"
  sheet_size=$(ffprobe -v error -show_entries stream=width,height -of csv=p=0 "$sheet")
  echo "ok: $sheet is a ${sheet_size} PNG" >&2
done
reviewed=$(run review "$TAKE/export-9x16.mp4" --at 1)
check "agentcam review re-checks an mp4 and writes a still" \
  '"error" not in j and j["sheet"].endswith("export-9x16.sheet.png") and len(j["frames"]) == 1 and j["frames"][0]["t"] == 1.0' "$reviewed"
[ "$(ffprobe -v error -show_entries stream=width,height -of csv=p=0 "$TAKE/export-9x16.at-1.png")" = "1080,1920" ] ||
  fail "agentcam review --at did not write a full-size still"
take_review=$(run review "$TAKE")
check "agentcam review on a take reviews each export" \
  '[r["path"].rsplit("/", 1)[1] for r in j["reviews"]] == ["export-16x9.mp4", "export-9x16.mp4"]' "$take_review"
check "a wide grid fits 16:9 whole and follows the action in 9:16" \
  '[e["viewport"] for e in j["exports"]] == ["fit", "follow"]' "$exported"

# panel_at <png> <x> <y> is true when that pixel is the terminal panel (#17191f), not the canvas (#0b0c10).
panel_at() {
  local rgb
  rgb=$(ffmpeg -v error -i "$1" -vf "crop=1:1:$2:$3" -f rawvideo -pix_fmt rgb24 - | od -An -tu1 | xargs)
  python3 -c 'import sys; r,g,b=map(int,sys.argv[1:]); print(r+g+b > (11+12+16+23+25+31)//2)' $rgb
}
fills_9x16() {
  local png=$1 what=$2
  for xy in "540 60" "540 1860" "60 960" "1020 960"; do
    read -r x y <<<"$xy"
    [ "$(panel_at "$png" "$x" "$y")" = "True" ] || fail "$what: ($x, $y) is empty canvas, not terminal"
  done
  echo "ok: $what fills the 9:16 frame" >&2
}
fills_9x16 "$TAKE/frame-9x16.png" "100x30 take"

tight=$(run export "$TAKE" --layout 16:9 --tighten)
check "--tighten reports the raw take length" \
  'abs(j["exports"][0]["tightened"]["duration_raw"] - '"$duration"') < 0.01' "$tight"
check "--tighten is shorter than the take" \
  'j["exports"][0]["tightened"]["duration"] < '"$duration"' - 1' "$tight"
check "--tighten segments run back to back from 0" \
  'all(a["out"][1] == b["out"][0] and a["take"][1] == b["take"][0] for a, b in zip(j["exports"][0]["tightened"]["segments"], j["exports"][0]["tightened"]["segments"][1:])) and j["exports"][0]["tightened"]["segments"][0]["take"][0] == 0' "$tight"
tight_duration=$(printf '%s' "$tight" | json 'j["exports"][0]["tightened"]["duration"]')
probe=$(ffprobe -v error -show_entries format=duration -of json "$TAKE/export-16x9.mp4")
check "--tighten video is as long as tightened.duration ($tight_duration)" \
  'abs(float(j["format"]["duration"]) - '"$tight_duration"') <= 0.1' "$probe"

planned=$(run export "$TAKE" --tighten --plan-out "$OUT/plan.json")
check "--plan-out writes a plan without rendering" '"exports" not in j and j["segments"] > 0' "$planned"
check "the plan carries the quiet screens' text" \
  'any("smoke-42" in s.get("screen_text", "") for s in j["segments"])' "$(cat "$OUT/plan.json")"
replayed=$(run export "$TAKE" --layout 16:9 --plan "$OUT/plan.json")
check "an unedited plan renders the --tighten edit" \
  'j["exports"][0]["tightened"] == '"$(printf '%s' "$tight" | json 'j["exports"][0]["tightened"]')" "$replayed"
python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); p["segments"][1]["take"][1] += 1; json.dump(p, open(sys.argv[1], "w"))' "$OUT/plan.json"
mismatched=$("$AGENTCAM" export "$TAKE" --layout 16:9 --plan "$OUT/plan.json" 2>/dev/null) && fail "a plan for another take rendered"
check "a plan for another take is bad_args" 'j["error"]["code"] == "bad_args"' "$mismatched"

stacked=$(run export "$TAKE" --layout 9:16 --region 50,0,50,30 --region 0,20,50,10 --tighten)
check "--region exports a 1080x1920 stack" \
  '[(e["layout"], e["viewport"], e["width"], e["height"]) for e in j["exports"]] == [("9:16", "stack", 1080, 1920)]' "$stacked"
check "--region reports each region in order with its panel" \
  '[(r["col"], r["row"], r["cols"], r["rows"]) for r in j["exports"][0]["regions"]] == [(50, 0, 50, 30), (0, 20, 50, 10)]' "$stacked"
check "stacked panels sit top to bottom inside the frame, same width for the same columns" \
  '(lambda a, b: a["y"] + a["h"] < b["y"] and b["y"] + b["h"] <= 1920 - 40 and a["w"] == b["w"] and a["y"] >= 40)(*[r["panel"] for r in j["exports"][0]["regions"]])' "$stacked"
check "--region keeps the --tighten time map" \
  'j["exports"][0]["tightened"] == '"$(printf '%s' "$tight" | json 'j["exports"][0]["tightened"]')" "$stacked"
probe=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of json "$TAKE/export-9x16.mp4")
check "stacked export is 1080x1920" 'j["streams"][0] == {"width": 1080, "height": 1920}' "$probe"
outside=$("$AGENTCAM" export "$TAKE" --layout 9:16 --region 60,0,41,30 2>/dev/null) && fail "a region past the grid exported"
check "a region past the grid is bad_args" 'j["error"]["code"] == "bad_args"' "$outside"
empty=$("$AGENTCAM" export "$TAKE" --layout 9:16 --region 0,0,0,30 2>/dev/null) && fail "an empty region exported"
check "an empty region is bad_args" 'j["error"]["code"] == "bad_args"' "$empty"

both=$("$AGENTCAM" start --tty --for 9:16 --size 80x24 -- true 2>/dev/null) && fail "--for with --size succeeded"
check "--for with --size is bad_args" 'j["error"]["code"] == "bad_args"' "$both"

started=$(run start --tty --for 9:16 --out "$OUT" -- bash --norc)
TAKE=$(printf '%s' "$started" | json 'j["take"]')
run type 'ls --color=always / ; echo vertical-$((6*7))' >/dev/null
run key Return >/dev/null
run wait --text 'vertical-42' --timeout 10 >/dev/null || fail "vertical take never printed"
vertical=$(run stop)
check "--for 9:16 records the 53x45 grid that fills 9:16" \
  'j["source"]["size"] == {"cols": 53, "rows": 45}' "$vertical"
exported=$(run export "$TAKE" --layout 9:16)
check "--for 9:16 exports 9:16 whole, with no crop" \
  '[(e["layout"], e["viewport"], e["width"], e["height"]) for e in j["exports"]] == [("9:16", "fit", 1080, 1920)]' "$exported"
mp4="$TAKE/export-9x16.mp4"
probe=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of json "$mp4")
check "--for 9:16 export is 1080x1920" 'j["streams"][0] == {"width": 1080, "height": 1920}' "$probe"
ffmpeg -v error -y -sseof -0.2 -i "$mp4" -frames:v 1 "$TAKE/frame-9x16.png" || fail "frame extract for --for 9:16"
fills_9x16 "$TAKE/frame-9x16.png" "--for 9:16 take"

screen_lines() { printf '%s' "$(run screen)" | json 'j["text"]'; }

started=$(run start --tty --size 100x30 --out "$OUT" -- bash --norc)
TAKE=$(printf '%s' "$started" | json 'j["take"]')
off=$("$AGENTCAM" click 1 1 2>/dev/null) && fail "a click reached a shell with mouse reporting off"
check "click on a program without mouse reporting is mouse_off" 'j["error"]["code"] == "mouse_off"' "$off"
off=$("$AGENTCAM" drag 1 1 5 1 2>/dev/null) && fail "a drag reached a shell with mouse reporting off"
check "drag on a program without mouse reporting is mouse_off" 'j["error"]["code"] == "mouse_off"' "$off"
run move 3 3 >/dev/null
outside=$("$AGENTCAM" click 100 0 2>/dev/null) && fail "a click outside the grid succeeded"
check "a click outside the grid is bad_args" 'j["error"]["code"] == "bad_args"' "$outside"
run stop >/dev/null

# curses picks the mode and encoding from terminfo, so either a click or a press and release counts.
started=$(run start --tty --size 80x24 --out "$OUT" -- python3 "$here/mouse_probe.py")
TAKE=$(printf '%s' "$started" | json 'j["take"]')
run wait --text 'probe ready' --timeout 10 >/dev/null || fail "curses probe never started"
run click 0 5 >/dev/null
run click 79 23 --button right >/dev/null
run drag 10 10 20 12 >/dev/null
run wait --idle 0.5 --timeout 10 >/dev/null
seen=$(screen_lines)
echo "$seen" >&2
grep -Eq '^ev 0 5 (click1|press1)' <<<"$seen" || fail "curses missed the left click at 0,5"
grep -Eq '^ev 79 23 (click3|press3)' <<<"$seen" || fail "curses missed the right click at 79,23"
grep -q '^ev 10 10 press1' <<<"$seen" || fail "curses missed the drag's press at 10,10"
grep -q '^ev 20 12 release1' <<<"$seen" || fail "curses missed the drag's release at 20,12"
echo "ok: curses saw clicks and a drag at the cells sent" >&2
run key q >/dev/null
sleep 0.5
timeline=$(cat "$TAKE/timeline.json")
check "timeline has the click at its cell center" \
  'any(e["type"] == "click" and (e["x"], e["y"], e["button"]) == (0.00625, 0.229167, "left") for e in j["events"])' "$timeline"
check "timeline has the drag" \
  'any(e["type"] == "drag" and (e["x1"], e["y1"], e["x2"], e["y2"]) == (0.13125, 0.4375, 0.25625, 0.520833) for e in j["events"])' "$timeline"

started=$(run start --tty --size 80x24 --out "$OUT" -- python3 "$here/mouse_probe.py" sgr-any)
TAKE=$(printf '%s' "$started" | json 'j["take"]')
run wait --text 'probe ready' --timeout 10 >/dev/null || fail "sgr probe never started"
run move 5 5 >/dev/null
run click 79 23 --button middle >/dev/null
run drag 0 0 6 2 --steps 3 >/dev/null
run wait --idle 0.5 --timeout 10 >/dev/null
seen=$(screen_lines | grep '^sgr ' | tr '\n' '|')
want='sgr 35 5 5 M|sgr 1 79 23 M|sgr 1 79 23 m|sgr 0 0 0 M|sgr 32 2 1 M|sgr 32 4 1 M|sgr 32 6 2 M|sgr 0 6 2 m|'
[ "$seen" = "$want" ] || fail "any-motion SGR reports were $seen, wanted $want"
echo "ok: any-motion SGR reports for move, click, and drag" >&2
run key q >/dev/null
sleep 0.5

# The startup queries Claude Code sends, plus DSR and DA2, each answered as a terminal would.
started=$(run start --tty --size 80x24 --out "$OUT" -- python3 "$here/query_probe.py")
TAKE=$(printf '%s' "$started" | json 'j["take"]')
run wait --text 'probe done' --timeout 10 >/dev/null || fail "query probe never finished"
seen=$(screen_lines | grep '=' | tr '\n' '|')
want='da1=\x1b[?62;22c|kitty=\x1b[?0u|xtversion=\x1bP>|agentcam '"$("$AGENTCAM" --version 2>/dev/null | awk '{print $2}')"'\x1b\\|da2=\x1b[>1;10;0c|dsr=\x1b[0n|cpr=\x1b[1;1R|'
[ "$seen" = "$want" ] || fail "query replies were $seen, wanted $want"
grep -qF 'u001bP>|agentcam' "$TAKE/term.cast" && fail "a query reply leaked into term.cast"
echo "ok: query replies" >&2
run key Return >/dev/null
sleep 0.5

# Keys follow the kitty keyboard flags the program pushed: disambiguate makes ctrl+j and Escape
# CSI u and leaves text and Enter alone; report-all-keys makes everything CSI u.
started=$(run start --tty --size 80x24 --out "$OUT" -- python3 "$here/key_probe.py" 1)
TAKE=$(printf '%s' "$started" | json 'j["take"]')
run wait --text 'probe ready' --timeout 10 >/dev/null || fail "key probe never started"
for k in ctrl+j Escape shift+Tab ctrl+Up Return; do run key "$k" >/dev/null; sleep 0.1; done
run type a >/dev/null
run wait --idle 0.5 --timeout 10 >/dev/null
seen=$(screen_lines | grep '^key ' | tr '\n' '|')
want='key \x1b[106;5u|key \x1b[27u|key \x1b[9;2u|key \x1b[1;5A|key \r|key a|'
[ "$seen" = "$want" ] || fail "keys under kitty flag 1 were $seen, wanted $want"
echo "ok: keys under kitty disambiguate" >&2
run key q >/dev/null
sleep 0.5
started=$(run start --tty --size 80x24 --out "$OUT" -- python3 "$here/key_probe.py" 8)
TAKE=$(printf '%s' "$started" | json 'j["take"]')
run wait --text 'probe ready' --timeout 10 >/dev/null || fail "key probe never started"
run type a >/dev/null
sleep 0.1
run key Return >/dev/null
run wait --idle 0.5 --timeout 10 >/dev/null
seen=$(screen_lines | grep '^key ' | tr '\n' '|')
want='key \x1b[97u|key \x1b[13u|'
[ "$seen" = "$want" ] || fail "keys under kitty flag 8 were $seen, wanted $want"
echo "ok: keys under kitty report-all-keys" >&2
"$AGENTCAM" stop >/dev/null 2>&1

echo "PASS: $TAKE"
