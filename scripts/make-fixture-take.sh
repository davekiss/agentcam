#!/usr/bin/env bash
# Synthesizes a finished take with ffmpeg, so export can be exercised without any capture permission.
# Usage: scripts/make-fixture-take.sh <dir> [--no-cam] [--no-mic]
set -euo pipefail

dir=${1:?usage: make-fixture-take.sh <dir> [--no-cam] [--no-mic]}
shift
cam=1
mic=1
for arg in "$@"; do
  case $arg in
    --no-cam) cam=0 ;;
    --no-mic) mic=0 ;;
    *) echo "unknown flag $arg" >&2; exit 2 ;;
  esac
done

ff() { ffmpeg -hide_banner -loglevel error -y "$@"; }

mkdir -p "$dir"
rm -f "$dir"/*.mov "$dir"/*.m4a "$dir"/*.mp4

ff -f lavfi -i "testsrc=size=2880x1800:rate=30:duration=10" \
  -c:v libx264 -pix_fmt yuv420p -preset veryfast "$dir/screen.mov"

tracks='{ "kind": "screen", "file": "screen.mov", "offset": 0.0, "width": 2880, "height": 1800 }'
if [ $cam = 1 ]; then
  ff -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=10" \
    -c:v libx264 -pix_fmt yuv420p -preset veryfast "$dir/cam.mov"
  tracks="$tracks,
    { \"kind\": \"camera\", \"file\": \"cam.mov\", \"offset\": 0.25, \"width\": 1280, \"height\": 720 }"
fi
if [ $mic = 1 ]; then
  ff -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=10" -c:a aac -b:a 128k "$dir/mic.m4a"
  tracks="$tracks,
    { \"kind\": \"mic\", \"file\": \"mic.m4a\", \"offset\": 0.0 }"
fi

cat > "$dir/take.json" <<JSON
{
  "version": 1,
  "id": "$(basename "$dir")",
  "createdAt": "2026-10-02T21:35:01Z",
  "status": "finished",
  "duration": 10.0,
  "source": {
    "kind": "display",
    "id": 1,
    "title": "Fixture Display",
    "frame": { "x": 0, "y": 0, "width": 1440, "height": 900 },
    "scale": 2
  },
  "tracks": [
    $tracks
  ]
}
JSON

# The cursor drifts around x=0.25, so a 9:16 export should crop toward the left of the screen.
{
  echo '{ "version": 1, "events": ['
  for i in $(seq 1 299); do
    awk -v i="$i" 'BEGIN { t = i / 30; printf "  { \"t\": %.3f, \"type\": \"cursor\", \"x\": %.3f, \"y\": %.3f },\n", t, 0.25 + 0.05 * sin(t), 0.5 + 0.1 * cos(t) }'
  done
  echo '  { "t": 2.000, "type": "click", "x": 0.260, "y": 0.540, "button": "left" },'
  echo '  { "t": 5.000, "type": "marker", "label": "fixture midpoint" }'
  echo '] }'
} > "$dir/timeline.json"

echo "$dir"
