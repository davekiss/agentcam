# rec

A native macOS screen recorder built for agents. Every command prints one JSON object to stdout, so a script or an agent can drive a take and read the result. Recording and composing are separate: `rec` records the raw screen, webcam, mic, and cursor data, and `rec export` turns one take into 16:9 and 9:16 videos with a webcam bubble and an animated border.

[SPEC.md](SPEC.md) is the contract for the take folder, the JSON shapes, and the commands. It also covers where `rec` is headed: agents recording terminals and apps from inside headless Linux microVMs such as Vercel Sandbox. This README documents the macOS recorder that exists today.

## Install

Requires macOS 14 or later and Xcode 16.

```sh
swift build -c release
cp .build/release/rec /usr/local/bin/rec
```

macOS asks for Screen Recording, Camera, and Microphone permission for the app that runs `rec`, usually your terminal. When a permission is missing, the command fails with `{"error": {"code": "permission_denied", ...}}` and names the permission.

## Commands

List what can be captured:

```sh
rec sources
```

Start a background recording of the main display with webcam and mic. It returns once the recorder is actually recording:

```sh
rec start
rec start --window 12500 --no-cam
rec start --app "Google Chrome" --no-preview --out ~/Desktop/takes
```

Check on it, drop a marker, and stop. `stop` prints the finished take.json:

```sh
rec status
rec mark "ran tests"
rec stop
```

Record in the foreground for a fixed time, or until Ctrl-C:

```sh
rec record --duration 30 --no-mic
```

Compose a take. With no `--layout` it writes both:

```sh
rec export ~/Movies/rec/take-20261002-213501
rec export ~/Movies/rec/take-20261002-213501 --layout 9:16 --no-border
```

## The take folder

Takes land in `~/Movies/rec/take-<yyyyMMdd-HHmmss>/` unless `--out` names another parent folder.

```
take.json        manifest: source, tracks, per-track sync offsets, status
screen.mov       H.264 display or window capture
cam.mov          H.264 webcam (absent with --no-cam)
mic.m4a          AAC microphone (absent with --no-mic)
timeline.json    cursor, click, and marker events on the take clock
markers.jsonl    markers appended by `rec mark` while recording
recorder.log     the background recorder's output
export-16x9.mp4  written by `rec export`
export-9x16.mp4
```

Each track's `offset` in take.json is how many seconds after the take clock's start its first sample arrived. Export uses only these offsets to line the tracks up, so you can fix sync by editing them.

## Development

```sh
swift test
scripts/verify-export.sh            # synthesize a take with ffmpeg, export both layouts, check with ffprobe
scripts/make-fixture-take.sh /tmp/t # just the fixture take
```

`verify-export.sh` needs `ffmpeg` and `ffprobe` and no capture permissions. It writes frames at t=0.7 (border intro) and t=5 (calm ring) next to each export for a visual check.
