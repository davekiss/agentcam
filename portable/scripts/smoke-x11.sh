#!/usr/bin/env bash
# End-to-end check of the x11 source: Xvfb + Ghostty, XTEST input, screenshots, stop, export.
# Linux only. Needs: a built agentcam (AGENTCAM=path, default target/release/agentcam), Xvfb, ffmpeg, ffprobe,
# python3, and ghostty (APP to use another terminal that runs bash).
set -euo pipefail

if [ "$(uname -s)" != Linux ]; then
  echo "SKIP: the x11 source needs Linux (Xvfb)"
  exit 0
fi

here=$(cd "$(dirname "$0")" && pwd)
AGENTCAM=${AGENTCAM:-$here/../target/release/agentcam}
APP=${APP:-ghostty}
OUT=${OUT:-$(mktemp -d "${TMPDIR:-/tmp}/agentcam-smoke-x11.XXXXXX")}
TAKE=""

fail() {
  echo "FAIL: $*" >&2
  if [ -n "$TAKE" ] && [ -f "$TAKE/recorder.log" ]; then
    echo "--- recorder.log" >&2
    grep -v '^ffmpeg: ' "$TAKE/recorder.log" | tail -20 >&2
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

json() {
  python3 -c 'import json,sys; j=json.load(sys.stdin); v='"$1"'; print(v if not isinstance(v,(dict,list)) else json.dumps(v))'
}

check() {
  local what=$1 expr=$2 input=$3
  [ "$(printf '%s' "$input" | json "$expr")" = "True" ] || fail "$what"
  echo "ok: $what" >&2
}

# rgb <image> <x> <y> prints that pixel as "r g b".
rgb() {
  ffmpeg -v error -i "$1" -vf "crop=1:1:$2:$3" -f rawvideo -pix_fmt rgb24 - | od -An -tu1 | xargs
}

for tool in Xvfb ffmpeg ffprobe python3 "$APP"; do
  command -v "$tool" >/dev/null || fail "$tool not on PATH"
done
[ -x "$AGENTCAM" ] || fail "agentcam binary not found at $AGENTCAM (cargo build --release)"

check "doctor reports x11 ready" 'j["sources"]["x11"]["ok"] is True' "$(run doctor)"
check "sources lists x11" 'j["x11"] is True' "$(run sources)"

started=$(run start --x11 --out "$OUT" -- "$APP")
TAKE=$(printf '%s' "$started" | json 'j["take"]')
DISPLAY_NUM=$(printf '%s' "$started" | json 'j["display"]')
check "start returns the display" 'j["display"].startswith(":")' "$started"
check "status reports the display" 'j["recording"] is True and j["display"] == "'"$DISPLAY_NUM"'"' "$(run status)"

run wait --idle 1 --timeout 20 >/dev/null
before=$(run screen --png "$OUT/before.png")
check "screen --png writes the whole screen" 'j["width"] == 1920 and j["height"] == 1080 and j["t"] > 0' "$before"

run type 'echo hello from x11' >/dev/null
run key Return >/dev/null
check "wait --idle returns once the screen settles" 'j["idle"] is True' "$(run wait --idle 1 --timeout 20)"
run screen --png "$OUT/hello.png" >/dev/null
cmp -s "$OUT/before.png" "$OUT/hello.png" && fail "typing changed nothing on screen"
echo "ok: typing changed the screen ($OUT/hello.png)" >&2

# Characters off the US keymap go through borrowed keycodes; the shell writes back what arrived.
typed='café ✔ → ok ~$HOME'
run type "printf '%s\\n' '$typed' > $OUT/typed.txt" >/dev/null
run key Return >/dev/null
run wait --idle 1 --timeout 20 >/dev/null
[ "$(cat "$OUT/typed.txt" 2>/dev/null)" = "$typed" ] || fail "typed text arrived as '$(cat "$OUT/typed.txt" 2>/dev/null)'"
echo "ok: every character arrived, including ones off the keymap" >&2

run type 'sleep 30' >/dev/null
run key Return >/dev/null
sleep 0.5
run key ctrl+c >/dev/null
run type "echo interrupted > $OUT/ctrlc.txt" >/dev/null
run key Return >/dev/null
run wait --idle 1 --timeout 20 >/dev/null
[ "$(cat "$OUT/ctrlc.txt" 2>/dev/null)" = interrupted ] || fail "ctrl+c did not interrupt sleep"
echo "ok: ctrl+c reached the shell" >&2

clicked=$(run click 960 540)
check "click returns its time" 'j["t"] > 0' "$clicked"
outside=$("$AGENTCAM" click 1920 10 2>/dev/null) && fail "a click off the screen succeeded"
check "a click off the screen is bad_args" 'j["error"]["code"] == "bad_args"' "$outside"
notext=$("$AGENTCAM" wait --text foo --timeout 1 2>/dev/null) && fail "wait --text worked on x11"
check "wait --text is not_supported on x11" 'j["error"]["code"] == "not_supported"' "$notext"
# Fill the screen with text that differs at every x, so the 9:16 window check below can tell where
# the window sits even in a terminal that leaves most of the screen blank.
run type 'seq -s " " 100000 104000' >/dev/null
run key Return >/dev/null
run wait --idle 1 --timeout 20 >/dev/null
run move 1700 900 >/dev/null
sleep 2

ps -eo pid=,comm= | awk '$2 ~ /^(Xvfb|ffmpeg|agentcam|'"$APP"')$/ {print $1}' >"$OUT/pids"
cpu=$(python3 - "$OUT/pids" <<'EOF'
import os, sys, time
pids = [p.strip() for p in open(sys.argv[1]) if p.strip()]
def ticks(p):
    f = open(f"/proc/{p}/stat").read().rsplit(")", 1)[1].split()
    return int(f[11]) + int(f[12])
def comm(p):
    return open(f"/proc/{p}/comm").read().strip()
a = {p: ticks(p) for p in pids}
time.sleep(3)
hz = os.sysconf("SC_CLK_TCK")
print(", ".join(f"{comm(p)} {100 * (ticks(p) - a[p]) / hz / 3:.0f}%" for p in pids))
EOF
)
echo "cpu during capture: $cpu" >&2

stopped=$(run stop)
check "stop prints a finished x11 take" \
  'j["status"] == "finished" and j["source"] == {"kind": "x11", "command": ["'"$APP"'"], "frame": {"x": 0, "y": 0, "width": 1920, "height": 1080}}' "$stopped"
check "take has a screen track starting within a second" \
  'len(j["tracks"]) == 1 and j["tracks"][0]["kind"] == "screen" and j["tracks"][0]["file"] == "screen.mp4" and 0 <= j["tracks"][0]["offset"] < 1 and (j["tracks"][0]["width"], j["tracks"][0]["height"]) == (1920, 1080)' "$stopped"
check "the pointer is left out of screen.mp4, for export to draw" 'j["tracks"][0]["pointer"] == "timeline"' "$stopped"
duration=$(printf '%s' "$stopped" | json 'j["duration"]')
offset=$(printf '%s' "$stopped" | json 'j["tracks"][0]["offset"]')

probe=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height,codec_name,pix_fmt:format=duration -of json "$TAKE/screen.mp4")
check "screen.mp4 is 1920x1080 h264 yuv420p" \
  '{k: j["streams"][0][k] for k in ("width", "height", "codec_name", "pix_fmt")} == {"width": 1920, "height": 1080, "codec_name": "h264", "pix_fmt": "yuv420p"}' "$probe"
check "screen.mp4 lasts the take within 0.5s ($duration)" 'abs(float(j["format"]["duration"]) - '"$duration"') <= 0.5' "$probe"

sleep 0.5
left=$(pgrep -a -f "Xvfb $DISPLAY_NUM |x11grab|^$APP" || true)
[ -z "$left" ] || fail "processes left after stop: $left"
[ ! -e "/tmp/.X${DISPLAY_NUM#:}-lock" ] || fail "Xvfb lock file left behind"
echo "ok: no Xvfb, ffmpeg, or $APP left, and the display lock is gone" >&2

timeline=$(cat "$TAKE/timeline.json")
check "timeline has the typed text" 'any(e["type"] == "type" and e["text"] == "echo hello from x11" for e in j["events"])' "$timeline"
check "timeline has Return and ctrl+c" '{"Return", "ctrl+c"} <= {e["key"] for e in j["events"] if e["type"] == "key"}' "$timeline"
check "timeline has the click, normalized" \
  'any(e["type"] == "click" and (e["x"], e["y"], e["button"]) == (0.5, 0.5, "left") for e in j["events"])' "$timeline"
check "timeline has cursor samples ending where the pointer moved" \
  '[(e["x"], e["y"]) for e in j["events"] if e["type"] == "cursor"][-1] == (round(1700/1920, 6), round(900/1080, 6))' "$timeline"
check "timeline opens with where the pointer starts, the middle of the screen" \
  '[(e["t"] < 0.1, e["x"], e["y"]) for e in j["events"] if e["type"] == "cursor"][0] == (True, 0.5, 0.5)' "$timeline"
check "timeline is in time order" '[e["t"] for e in j["events"]] == sorted(e["t"] for e in j["events"])' "$timeline"

tight=$("$AGENTCAM" export "$TAKE" --tighten 2>/dev/null) && fail "--tighten worked on x11"
check "--tighten is not_supported on x11" 'j["error"]["code"] == "not_supported"' "$tight"

exported=$(run export "$TAKE")
check "a landscape screen fits 16:9 and follows in 9:16" \
  '[(e["layout"], e["viewport"], e["width"], e["height"]) for e in j["exports"]] == [("16:9", "fit", 1920, 1080), ("9:16", "follow", 1080, 1920)]' "$exported"
for slug in 16x9 9x16; do
  probe=$(ffprobe -v error -show_entries format=duration -of json "$TAKE/export-$slug.mp4")
  check "export-$slug lasts the screen track within 0.5s" \
    'abs(float(j["format"]["duration"]) - ('"$duration"' - '"$offset"')) <= 0.5' "$probe"
done

# Where the 9:16 window sits at a moment: the left edge, in screen pixels, of the 587px-wide crop
# of screen.mp4 (1000px of panel at 1840/1080 scale) that best matches the export's panel. Both
# are shrunk the same way, by area, to 100 columns per window width, so one step is 5.87px.
cat >"$OUT/camera_x.py" <<'EOF'
import subprocess, sys
export, screen, t = sys.argv[1:4]
def gray(path, vf):
    cmd = ["ffmpeg", "-v", "error", "-ss", t, "-i", path, "-frames:v", "1", "-vf", vf, "-f", "rawvideo", "-pix_fmt", "gray", "-"]
    return subprocess.run(cmd, capture_output=True, check=True).stdout
panel = gray(export, "crop=1000:1840:40:40,scale=100:184:flags=area")
shot = gray(screen, "scale=327:184:flags=area")
def cost(x0):
    return sum(abs(panel[r * 100 + c] - shot[r * 327 + x0 + c]) for r in range(184) for c in range(100))
print(round(min(range(0, 228), key=cost) * 1920 / 327))
EOF
export_len=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$TAKE/export-9x16.mp4")
before_click=$(printf '%s' "$timeline" | json '[e["t"] for e in j["events"] if e["type"] == "click"][0] - '"$offset"' - 0.9')
at_end=$(python3 -c "print($export_len - 0.3)")
typing_x=$(python3 "$OUT/camera_x.py" "$TAKE/export-9x16.mp4" "$TAKE/screen.mp4" "$before_click")
pointer_x=$(python3 "$OUT/camera_x.py" "$TAKE/export-9x16.mp4" "$TAKE/screen.mp4" "$at_end")
[ "$typing_x" -le 600 ] || fail "9:16 window sat at x=$typing_x, right of the typing (the click would center it at 666)"
[ "$pointer_x" -ge 1317 ] || fail "9:16 window sat at x=$pointer_x, not at the pointer on the right (1333)"
echo "ok: 9:16 follows the typing (window at x=$typing_x) and then the pointer (x=$pointer_x)" >&2
ffmpeg -v error -y -ss "$before_click" -i "$TAKE/export-9x16.mp4" -frames:v 1 "$OUT/follow-typing.png"
ffmpeg -v error -y -ss "$at_end" -i "$TAKE/export-9x16.mp4" -frames:v 1 "$OUT/follow-pointer.png"
ffmpeg -v error -y -ss "$at_end" -i "$TAKE/export-16x9.mp4" -frames:v 1 "$OUT/fit-16x9.png"

# The pointer, drawn at export. The pointer rests at the middle of the screen through the typing,
# the click lands there, and the move takes it to (1700, 900). On 16:9 the screen fits at
# 1664/1920 scale from (128, 72), so those points are canvas (960, 540) and (1602, 852).
# pointer_diff <a> <b> <t> <x0> <y0> <w> <h> prints the mean RGB difference of that canvas box.
cat >"$OUT/pointer_diff.py" <<'EOF'
import subprocess, sys
a, b, t = sys.argv[1:4]
x, y, w, h = map(int, sys.argv[4:8])
def box(path):
    cmd = ["ffmpeg", "-v", "error", "-ss", t, "-i", path, "-frames:v", "1", "-vf", f"crop={w}:{h}:{x}:{y}", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]
    return subprocess.run(cmd, capture_output=True, check=True).stdout
pa, pb = box(a), box(b)
print(round(sum(abs(p - q) for p, q in zip(pa, pb)) / len(pa), 2))
EOF
cp "$TAKE/export-16x9.mp4" "$OUT/auto-16x9.mp4"
run export "$TAKE" --layout 16:9 --cursor never >/dev/null
cp "$TAKE/export-16x9.mp4" "$OUT/never-16x9.mp4"
click_t=$(printf '%s' "$timeline" | json '[e["t"] for e in j["events"] if e["type"] == "click"][0] - '"$offset")
move_t=$(printf '%s' "$timeline" | json '[e["t"] for e in j["events"] if e["type"] == "cursor"][-1] - '"$offset")
diff_at() { python3 "$OUT/pointer_diff.py" "$OUT/auto-16x9.mp4" "$OUT/never-16x9.mp4" "$@"; }
at() { python3 -c "print(max(0, $1))"; }
idle=$(diff_at "$(at "$click_t - 1.5")" 954 534 36 46)
moving=$(diff_at "$(at "$move_t + 0.3")" 1596 846 36 46)
ripple=$(diff_at "$(at "$click_t + 0.12")" 928 525 24 30)
gone=$(diff_at "$(at "$export_len - 0.3")" 1596 846 36 46)
echo "pointer box differences, auto vs never: idle $idle, moving $moving, ripple $ripple, gone $gone" >&2
python3 -c "import sys; sys.exit(0 if $idle < 2 else 1)" || fail "auto drew the pointer while it sat idle during typing ($idle)"
python3 -c "import sys; sys.exit(0 if $moving > 10 else 1)" || fail "auto did not draw the pointer after it moved ($moving)"
python3 -c "import sys; sys.exit(0 if $ripple > 6 else 1)" || fail "no ripple at the click ($ripple)"
python3 -c "import sys; sys.exit(0 if $gone < 2 else 1)" || fail "auto still drew the pointer seconds after it stopped ($gone)"
echo "ok: auto hides the idle pointer, shows it when it moves, ripples the click, and fades it out" >&2
ffmpeg -v error -y -ss "$(at "$click_t + 0.12")" -i "$OUT/auto-16x9.mp4" -frames:v 1 "$OUT/click-16x9.png"

bordered=$(run export "$TAKE" --layout 9:16 --border)
check "--border exports 9:16" '[e["viewport"] for e in j["exports"]] == ["follow"]' "$bordered"
ffmpeg -v error -y -ss 1 -i "$TAKE/export-9x16.mp4" -frames:v 1 "$OUT/border-9x16.png"

started=$(run start --x11 --for 9:16 --out "$OUT" -- "$APP")
TAKE=$(printf '%s' "$started" | json 'j["take"]')
run wait --idle 1 --timeout 20 >/dev/null
run type 'echo vertical' >/dev/null
run key Return >/dev/null
run wait --idle 1 --timeout 20 >/dev/null
vertical=$(run stop)
check "--for 9:16 records the 1000x1840 screen that fills 9:16 inside its margin" \
  'j["source"]["frame"] == {"x": 0, "y": 0, "width": 1000, "height": 1840}' "$vertical"
exported=$(run export "$TAKE" --layout 9:16)
check "--for 9:16 exports 9:16 whole" \
  '[(e["layout"], e["viewport"]) for e in j["exports"]] == [("9:16", "fit")]' "$exported"
ffmpeg -v error -y -sseof -0.3 -i "$TAKE/export-9x16.mp4" -frames:v 1 "$OUT/vertical-9x16.png"
# The screen covers x 40..1039 and y 40..1879 exactly; outside is the canvas (#0b0c10).
for xy in "39 960 canvas" "40 960 screen" "1039 960 screen" "1040 960 canvas" "540 39 canvas" "540 40 screen" "540 1879 screen" "540 1880 canvas"; do
  read -r x y want <<<"$xy"
  got=$(rgb "$OUT/vertical-9x16.png" "$x" "$y")
  is=$(python3 -c 'import sys; r,g,b=map(int,sys.argv[1:]); print("canvas" if abs(r-11)+abs(g-12)+abs(b-16) <= 12 else "screen")' $got)
  [ "$is" = "$want" ] || fail "--for 9:16: ($x, $y) is $is ($got), expected $want"
done
echo "ok: --for 9:16 fills the frame inside the margin, pixel for pixel" >&2

echo "frames: $OUT/hello.png $OUT/fit-16x9.png $OUT/click-16x9.png $OUT/follow-typing.png $OUT/follow-pointer.png $OUT/border-9x16.png $OUT/vertical-9x16.png" >&2
echo "PASS: $OUT"
