# rec

An agent-first screen recorder for macOS. Built for demos and devrel. An agent (or a person) drives it from the command line, and every command prints one JSON object to stdout so another program can read the result.

The core idea is that recording and composing are separate. `rec` records the raw pieces of a take (screen, webcam, mic, and what happened on screen as data). Layouts (16:9, 9:16), the webcam bubble, and its attention-grabbing border are applied when you export. One recording gives every aspect ratio, and an agent can edit from the data instead of the pixels.

## Scope of the first slice

In: the capture sources, start/stop/status/mark, the take folder, the floating webcam preview that stays out of the capture, and export to 16:9 and 9:16 with an animated border.

Later: blur of sensitive regions, auto-zoom on clicks, transcription and summary, Mux upload, a Claude Code mod.

## Take folder

```
~/Movies/rec/take-20261002-213501/
  take.json        manifest (written at start, finalized at stop)
  screen.mov       H.264, the captured display or window, no audio
  cam.mov          H.264, webcam only, no audio          (absent with --no-cam)
  mic.m4a          AAC, microphone                        (absent with --no-mic)
  timeline.json    events on the take clock               (written at stop)
  markers.jsonl    appended to by `rec mark` while recording; merged into timeline.json at stop
  recorder.log     stderr of the background recorder
  export-16x9.mp4  written by `rec export`
  export-9x16.mp4
```

### take.json

```json
{
  "version": 1,
  "id": "take-20261002-213501",
  "createdAt": "2026-10-02T21:35:01Z",
  "status": "recording | finished | failed",
  "duration": 42.7,
  "source": {
    "kind": "display | window",
    "id": 1,
    "title": "Built-in Retina Display | <window title>",
    "app": "Google Chrome",
    "frame": { "x": 0, "y": 0, "width": 1512, "height": 982 },
    "scale": 2
  },
  "tracks": [
    { "kind": "screen", "file": "screen.mov", "offset": 0.0,  "width": 3024, "height": 1964 },
    { "kind": "camera", "file": "cam.mov",    "offset": 0.12, "width": 1920, "height": 1080 },
    { "kind": "mic",    "file": "mic.m4a",    "offset": 0.03 }
  ]
}
```

When `status` is `failed`, take.json also carries `"error": {"code", "message"}`.

The take clock starts at t0, the host time when the recorder begins writing. Each track's `offset` is the seconds between t0 and that track's first sample. Export uses offsets to line the tracks up. This is the only sync mechanism, and it is explicit so an agent can fix it by hand.

With the camera on, t0 waits until the webcam's exposure has settled (a lit picture that has stopped changing, or 3s at most), so cam.mov never opens on the black frames a webcam produces while it warms up.

### timeline.json

```json
{
  "version": 1,
  "events": [
    { "t": 0.033, "type": "cursor", "x": 0.412, "y": 0.230 },
    { "t": 1.200, "type": "click",  "x": 0.415, "y": 0.231, "button": "left" },
    { "t": 3.900, "type": "marker", "label": "ran tests" }
  ]
}
```

`t` is seconds on the take clock. `x` and `y` are normalized 0..1 relative to the captured source's frame, top-left origin, so they survive any crop or resolution. Cursor is sampled at 30 Hz and only written when it moves. Keystrokes are deliberately not recorded.

## Commands

All output is a single JSON object on stdout. Human-readable progress goes to stderr. Failures print `{"error": {"code": "...", "message": "..."}}` and exit nonzero.

- `rec sources` lists displays and shareable on-screen windows with their ids, app names, titles, and frames.
- `rec start [--display <id> | --window <id> | --app <name>] [--no-cam] [--no-mic] [--no-preview] [--out <dir>]` starts a background recorder and returns once it is actually recording: `{"take": "<path>", "pid": 1234}`. The default source is the main display. `--app` picks that app's frontmost window.
- `rec stop` stops the active recorder, waits for the files to finalize, and prints the finished take.json.
- `rec status` prints `{"recording": true, "take": "...", "elapsed": 12.3}` or `{"recording": false}`.
- `rec mark <label>` adds a marker at the current moment of the active take.
- `rec record --duration <seconds> [same flags as start]` records in the foreground until the duration ends or SIGINT. `rec start` spawns this same command detached, so there is one recording code path.
- `rec export <take> [--layout 16:9] [--layout 9:16] [--no-border]` composes the take and prints the output paths. With no `--layout`, it exports both.

Output shapes not shown above:

- `rec sources`: `{"displays": [<source>], "windows": [<source>]}`, where each entry has the same fields as take.json's `source`.
- `rec mark`: `{"take", "t", "label"}`. Each line of markers.jsonl is `{"t", "label"}` with `t` on the take clock.
- `rec export`: `{"take", "exports": [{"layout", "path", "width", "height", "duration"}]}`.

`--out` names the parent folder for the take (default `~/Movies/rec`). If two takes start in the same second, the second folder gets a `-2` suffix.

Commands are idempotent where it matters. `start` while recording returns an error naming the active take. `stop` with nothing recording returns `{"recording": false}`. A stale active record (pid no longer alive) gets cleaned up silently.

Active state lives in `~/Library/Application Support/rec/active.json`: `{"pid", "take", "startedAt"}`.

## Live preview

While recording with the camera on, a borderless floating circular panel shows the webcam in the bottom-right corner of the screen. It is draggable. It is excluded from the screen capture. The recording does not contain it, because the bubble in the final video is drawn at export. The panel plays the attention border once at the start, so the presenter sees the moment the take begins.

## Export

Layouts are data: a table of presets keyed by aspect, each giving the canvas size, the screen rect, and the camera rect and shape.

- `16:9` is 1920x1080. The screen is fit inside with a small margin on a dark background, and the camera is a circle in the bottom-right corner.
- `9:16` is 1080x1920. The screen sits in the top portion, cropped to fill a 1080-wide region, centered on the cursor's average position if there is timeline data. The camera is a large circle in the lower portion.

The attention border is a rotating gradient ring around the camera circle. Its look at time t is a pure function: an intro from 0 to about 1.5s (the ring sweeps in, glows, and pulses once), then a calm thin ring for the rest of the video. `--no-border` turns it off.

An export opens at the latest video track offset, the first moment both the screen and the camera have a picture, so it never starts on dead frames. Earlier media from any track is trimmed. Mic audio is muxed in with its offset applied. Output is H.264 + AAC MP4.
