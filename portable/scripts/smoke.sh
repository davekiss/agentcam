#!/usr/bin/env bash
# End-to-end check of the tty source: record, drive, observe, stop, export, verify the MP4s.
# Needs: a built rec (REC=path, default target/release/rec), ffmpeg, ffprobe, python3.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
REC=${REC:-$here/../target/release/rec}
OUT=${OUT:-$(mktemp -d "${TMPDIR:-/tmp}/rec-smoke.XXXXXX")}
TAKE=""
export BASH_SILENCE_DEPRECATION_WARNING=1

fail() {
  echo "FAIL: $*" >&2
  if [ -n "$TAKE" ] && [ -f "$TAKE/recorder.log" ]; then
    echo "--- recorder.log" >&2
    tail -20 "$TAKE/recorder.log" >&2
  fi
  "$REC" stop >/dev/null 2>&1 || true
  exit 1
}

run() {
  echo "+ rec $*" >&2
  local out
  out=$("$REC" "$@" 2>/dev/null) || fail "rec $1 exited nonzero: $out"
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
[ -x "$REC" ] || fail "rec binary not found at $REC (cargo build --release)"

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

again=$("$REC" start --tty -- true 2>/dev/null) && fail "second start succeeded while recording"
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

echo "PASS: $TAKE"
