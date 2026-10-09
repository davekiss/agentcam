#!/usr/bin/env bash
# Builds agentcam-mac, synthesizes a fixture take, exports both layouts, and checks the MP4s with ffprobe.
# Frames at t=0.7 (border intro) and t=5 (calm) are written as PNGs for a visual check.
# Usage: scripts/verify-export.sh [work-dir]
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=${1:-$root/fixtures/verify}
rec="$root/.build/debug/agentcam-mac"
fail=0

check() {
  if [ "$2" = "$3" ]; then
    echo "ok   $1: $2"
  else
    echo "FAIL $1: got '$2', want '$3'"
    fail=1
  fi
}

# Mean brightness (0-255) of a 20x20 patch centered at x,y of the frame at time t.
patch_luma() {
  ffmpeg -hide_banner -loglevel error -ss "$2" -i "$1" -frames:v 1 \
    -vf "crop=20:20:$(($3 - 10)):$(($4 - 10)),format=gray,scale=1:1:flags=area" -f rawvideo - | od -An -tu1 | tr -d ' '
}

probe() { ffprobe -v error -select_streams "$2" -show_entries "$3" -of default=nw=1:nk=1 "$1"; }

(cd "$root" && swift build >/dev/null)

verify_take() {
  local name=$1; shift
  local take="$work/$name"
  "$root/scripts/make-fixture-take.sh" "$take" "$@" >/dev/null
  "$rec" export "$take" --border > "$take/export.json"

  for layout in 16x9 9x16; do
    local mp4="$take/export-$layout.mp4"
    local want_size=$([ $layout = 16x9 ] && echo 1920x1080 || echo 1080x1920)
    echo "== $name $layout"
    check "video codec" "$(probe "$mp4" v:0 stream=codec_name)" h264
    check "size" "$(probe "$mp4" v:0 stream=width)x$(probe "$mp4" v:0 stream=height)" "$want_size"
    if [[ " $* " == *" --no-mic "* ]]; then
      check "audio streams" "$(probe "$mp4" a stream=codec_name | wc -l | tr -d ' ')" 0
    else
      check "audio codec" "$(probe "$mp4" a:0 stream=codec_name)" aac
    fi
    local duration
    duration=$(ffprobe -v error -show_entries format=duration -of default=nw=1:nk=1 "$mp4")
    check "duration within 0.3s of 10s" "$(awk -v d="$duration" 'BEGIN { print (d > 9.7 && d < 10.3) ? "yes" : "no (" d ")" }')" yes
    for t in 0.7 5; do
      ffmpeg -hide_banner -loglevel error -y -ss "$t" -i "$mp4" -frames:v 1 "$take/frame-$layout-t$t.png"
    done
  done
}

verify_take full
echo "== head trim (cam.mov starts at 0.25s, so the export opens there; 9:16 bubble center 540,1560)"
check "bubble lit on the first frame" "$(awk -v l="$(patch_luma "$work/full/export-9x16.mp4" 0 540 1560)" 'BEGIN { print (l >= 30) ? "lit" : "dark (" l ")" }')" lit
verify_take no-cam-no-mic --no-cam --no-mic

echo
echo "frames:"
ls "$work"/*/frame-*.png
exit $fail
