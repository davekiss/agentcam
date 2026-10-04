# rec

A screen recorder for agents. Its main user is an agent running in a headless Linux microVM (Vercel Sandbox, E2B, Modal, Daytona, Fly) that needs to make a video of what it built: a TUI, a Claude Code mod, a CLI demo, or a desktop app. A person on a Mac is the second user. Every command prints one JSON object to stdout so the calling program can read the result.

Two ideas hold the design together:

1. **Recording and composing are separate.** `rec` records the raw pieces of a take: the screen or terminal stream, optional camera and mic, and what happened as data. Layouts (16:9, 9:16), theming, and the attention border are applied at export. One recording gives every aspect ratio, and an agent can edit from the data instead of the pixels.
2. **In a microVM, `rec` owns the environment it records.** A Firecracker VM has no display, GPU, camera, or sound device. `rec` creates the terminal or virtual display, runs the program inside it, and drives it with input, so the agent can launch, type, wait, and film without any desktop.

Status: the macOS backend (ScreenCaptureKit, camera, mic, preview panel, CoreImage export) is implemented in Swift. Everything marked *Linux* below is planned.

## Verified on Vercel Sandbox (2026-10-03)

Probed on the default image: Ubuntu 26.04, kernel 6.18, 4 vCPU, 8 GB, non-root `ubuntu` user with sudo.

- No `/dev/dri` and no `/dev/snd`. `/dev/pts` is present, and `script`, `tmux`, `claude`, and `codex` come preinstalled.
- `apt-get install xvfb ffmpeg xdotool mesa` took 34s. `ghostty` 1.3 is in the Ubuntu archive and took 7s more.
- Ghostty runs on Xvfb using Mesa llvmpipe (OpenGL 4.5, software). A 10s `ffmpeg -f x11grab` capture at 1920x1080 and 30 fps kept 299 of 300 frames. During the capture Ghostty used about 90% of one core, ffmpeg (x264 ultrafast) about 70%, and Xvfb about 20%. xdotool typing into Ghostty works.
- `script --log-io --log-timing` captures a PTY session with timing and no display: 1.4 KB for a one-second session. tmux runs htop headless.

So the pixel route works but costs about two cores, and the terminal route is nearly free. Dependency install time (around 40s per boot) is the biggest cost an agent pays, which is why install is a requirement below.

## Sources

A take records exactly one primary source.

| kind | where | how it is captured |
|---|---|---|
| `tty` | Linux, macOS | `rec` spawns the command in a PTY it owns and records the output byte stream with timestamps. No display needed. Rendered to pixels at export. |
| `x11` | Linux | `rec` starts Xvfb at a given size, the agent (or `rec`) launches apps into it, and it is captured with x11grab. Covers GUI apps and real terminal emulators like Ghostty. |
| `display`, `window` | macOS | ScreenCaptureKit. Implemented. |

Terminal programs should use `tty`. It is lossless, re-renders at any size and theme, and costs almost nothing during capture. Use `x11` only when the window chrome or a GUI is the point.

Camera and mic are optional tracks, available only where the devices exist (macOS today). Wayland desktops (xdg-desktop-portal ScreenCast over PipeWire) are out of scope until someone needs them.

## Input

In a microVM, nobody else can type. `rec` sends input to the source it owns, and logs every input it sends into the timeline with an exact timestamp. Those logged events feed click highlights, typing effects, and later auto-zoom. This replaces macOS input monitoring for these sources.

- `tty` input is written to the PTY.
- `x11` input goes through XTEST (what xdotool uses).
- On macOS sources, `rec` only observes the cursor and clicks, as it does today.

## Take folder

```
<out>/take-20261002-213501/
  take.json        manifest (written at start, finalized at stop)
  term.cast        tty only: asciicast v2 output stream
  screen.mp4       x11 / macOS: H.264, no audio        (screen.mov on macOS)
  cam.mov          H.264 webcam                         (macOS, absent with --no-cam)
  mic.m4a          AAC microphone                       (macOS, absent with --no-mic)
  timeline.json    events on the take clock             (written at stop)
  markers.jsonl    appended to by `rec mark` while recording; merged into timeline.json at stop
  recorder.log     stderr of the background recorder
  export-16x9.mp4  written by `rec export`
  export-9x16.mp4
```

The default `<out>` is `~/Movies/rec` on macOS and `$XDG_DATA_HOME/rec` (falling back to `~/.local/share/rec`) on Linux.

`term.cast` uses asciicast v2 (a JSON header line, then `[t, "o", data]` lines) so existing players and `agg` can read it. Only output (`"o"`) events go in the cast. Input lives in timeline.json.

### take.json

```json
{
  "version": 2,
  "id": "take-20261002-213501",
  "createdAt": "2026-10-02T21:35:01Z",
  "status": "recording | finished | failed",
  "duration": 42.7,
  "source": {
    "kind": "tty | x11 | display | window",
    "command": ["herdr"],
    "size": { "cols": 120, "rows": 36 },
    "frame": { "x": 0, "y": 0, "width": 1920, "height": 1080 },
    "title": "...",
    "app": "...",
    "scale": 1
  },
  "tracks": [
    { "kind": "term",   "file": "term.cast",  "offset": 0.0 },
    { "kind": "screen", "file": "screen.mp4", "offset": 0.0,  "width": 1920, "height": 1080 },
    { "kind": "camera", "file": "cam.mov",    "offset": 0.12, "width": 1920, "height": 1080 },
    { "kind": "mic",    "file": "mic.m4a",    "offset": 0.03 }
  ]
}
```

Which `source` fields are present depends on the kind: `command` and `size` for `tty`; `command` (if `rec` launched one) and `frame` for `x11`; `id`, `title`, `app`, `frame`, and `scale` for macOS. When `status` is `failed`, take.json also carries `"error": {"code", "message"}`. Version 1 takes (macOS, before this change) stay readable.

The take clock starts at t0, the moment the recorder begins writing. Each track's `offset` is the seconds between t0 and that track's first sample. Export lines tracks up using only these offsets, so an agent can fix sync by editing them.

With the camera on, t0 waits until the webcam's exposure has settled (a lit picture that has stopped changing, or 3s at most), so cam.mov never opens on black warm-up frames.

### timeline.json

```json
{
  "version": 2,
  "events": [
    { "t": 0.033, "type": "cursor", "x": 0.412, "y": 0.230 },
    { "t": 1.200, "type": "click",  "x": 0.415, "y": 0.231, "button": "left" },
    { "t": 2.000, "type": "type",   "text": "ls -la" },
    { "t": 2.400, "type": "key",    "key": "Return" },
    { "t": 2.900, "type": "type",   "text": null, "redacted": true },
    { "t": 3.900, "type": "marker", "label": "ran tests" }
  ]
}
```

`t` is seconds on the take clock. `x` and `y` are normalized 0..1 relative to the source's frame, top-left origin, so they survive any crop or resolution. For `tty` they are cell-center coordinates normalized to the grid. Cursor is sampled at 30 Hz and written only when it moves.

`type` and `key` events record only input that `rec` itself sent. Keystrokes a human types on macOS are never recorded. `rec type --secret` still sends the text but logs it as redacted.

## Commands

All output is a single JSON object on stdout. Human-readable progress goes to stderr. Failures print `{"error": {"code": "...", "message": "..."}}` and exit nonzero.

### Recording

- `rec start --tty [--size 120x36 | --for <layout>] -- <cmd...>` spawns `<cmd>` in a PTY under a background recorder and returns once it is recording: `{"take", "pid"}`. The take finishes when `rec stop` runs or the command exits. The grid defaults to 120x36. `--for 9:16` or `--for 16:9` picks the grid whose panel fills that layout's canvas at a legible size in the embedded font, so exporting to that layout fits with no crop and no empty bands beyond the margin: 53x45 for 9:16 (about 30px text on a 1080-wide frame) and 124x29 for 16:9. Passing both `--size` and `--for` fails with `bad_args`.
- `rec start --x11 [--size 1920x1080] [-- <cmd...>]` starts Xvfb and the recorder, optionally launches `<cmd>` with `DISPLAY` set, and returns `{"take", "pid", "display": ":99"}` so the agent can launch more apps into it.
- `rec start [--display <id> | --window <id> | --app <name>] [--no-cam] [--no-mic] [--no-preview]` is the macOS form. It is implemented, and the default source there is the main display.
- All `start` forms accept `--out <dir>`.
- `rec record --duration <seconds> [same flags as start]` records in the foreground until the duration ends, the command exits, or SIGINT. `rec start` spawns this same command detached, so there is one recording code path.
- `rec stop` stops the active recorder, waits for files to finalize, tears down anything `rec` started (PTY child, Xvfb), and prints the finished take.json.
- `rec status` prints `{"recording": true, "take", "elapsed", "source"}` or `{"recording": false}`.
- `rec mark <label>` adds a marker at the current moment: `{"take", "t", "label"}`. Each line of markers.jsonl is `{"t", "label"}`.

### Driving and observing (tty, x11)

- `rec type <text> [--delay <ms>] [--secret]` types text at a human-looking pace (default 40 ms per character).
- `rec key <combo>` sends a key or chord, such as `Return`, `ctrl+c`, or `alt+tab`.
- `rec click <x> <y> [--button left]` and `rec move <x> <y>` work on `x11` only. Coordinates are pixels of the source frame.
- `rec wait --text <regex> [--new] [--timeout <s>]` blocks until the current screen matches the pattern, so agents wait on output instead of sleeping. It prints `{"matched": true, "t"}`, or exits with error `timeout`. On `tty` it matches the emulated screen. On `x11` it needs OCR and comes later.
  - `--new` (`tty` only) matches the program's output since the last `type` or `key` instead of the screen, so the echo of a command typed earlier does not count. The recorder marks its output stream just before each input byte reaches the PTY, and `--new` matches everything written after that mark, as plain text with escape sequences and carriage returns removed. Output that arrived between the input and the start of the wait still counts, so `rec key Return; rec wait --new` has no race, and repeating the wait gives the same answer. With no input sent yet, it covers all output so far. The recorder holds the last 1 MiB of output text.
- `rec screen [--png <path>]` prints what is on screen now: `{"text", "cols", "rows", "cursor"}` for `tty`, or writes a PNG and prints `{"png"}`. This is how the agent checks its work mid-take.

Each driving command records itself in the timeline and returns `{"t"}` on the take clock.

### Composing and delivering

- `rec export <take> [--layout 16:9] [--layout 9:16] [--border] [--tighten] [--theme <name>] [--font <name>] [--upload <target>]` composes the take and prints `{"take", "exports": [{"layout", "viewport", "path", "width", "height", "duration", "url"?, "tightened"?}]}`. With no `--layout`, it exports both. `viewport` is `fit` when the whole grid shows and `follow` when 9:16 crops a wide grid to a panning window (see Export). `url` is present only with `--upload`. `--tighten` retimes the take so its pacing follows the program rather than the agent driving it (see Tighten), and adds `tightened` to each export.
- `rec sources` lists what can be captured: macOS displays and windows, plus `{"tty": true, "x11": <bool>}`.
- `rec doctor` reports the platform, what each source kind needs, and what is missing, as JSON. Agents run it first.

The VM, and the take folder with it, is gone when the session ends, so delivery is part of export. `--upload` streams each finished export to its target and adds the `url` it can be fetched from. The target is one of:

- `blob`: Vercel Blob, with the read-write token from `BLOB_READ_WRITE_TOKEN`. Each export goes to `rec/<take-id>/export-<layout>.mp4` with no random suffix, and exporting the same take again overwrites it. `blob` uploads public blobs; `blob:private` is for stores configured as private, which reject public uploads, and its URL needs the token to fetch. `url` is the URL the Blob API returns. The request matches what `put()` in `@vercel/blob` 2.8 sends (`PUT https://vercel.com/api/blob/?pathname=...`, API version 12). `REC_BLOB_API_URL` replaces the API base, for tests.
- An `https://` URL: a presigned PUT, which S3, R2, GCS, and Mux direct uploads all hand out. It names one object, so it needs exactly one `--layout`, or export fails with `bad_args` before rendering. `url` is the presigned URL without its query string. Plain `http://` is accepted only for localhost.

A missing token fails with `missing_credentials` naming the variable, before anything renders. A refused upload fails with `upload_failed`, carrying the HTTP status and the start of the response body. The export files stay on disk either way.

### Behavior shared by all commands

If two takes start in the same second, the second folder gets a `-2` suffix.

Commands are idempotent where it matters. `start` while recording returns an error naming the active take. `stop` with nothing recording returns `{"recording": false}`. A stale active record (pid no longer alive) is cleaned up silently, along with any Xvfb it left behind.

Active state lives in `active.json`, holding `{"pid", "take", "startedAt", "source", "display"?}`. It is in `~/Library/Application Support/rec/` on macOS and `$XDG_STATE_HOME/rec/` (falling back to `~/.local/state/rec/`) on Linux.

## Install

On Linux, `rec` ships as a single static binary (musl) that runs as a non-root user and installs with one `curl | sh` into `~/.local/bin`. Everything a `tty` take needs, from capture through export, must work with no package installs. The `x11` source may require Xvfb, which `rec doctor` reports, and installs when given `--fix` and sudo is available.

Encoding: export pipes raw RGBA frames into an encoder. v1 requires an `ffmpeg` on `PATH` and `rec doctor --fix` fetches a static build. Linking an H.264 encoder into the binary is the follow-up that removes that step.

## Live preview (macOS)

While recording with the camera on, a borderless floating circular panel shows the webcam in the bottom-right corner of the screen. It is draggable and is excluded from the screen capture; the bubble in the final video is drawn at export. The panel plays the attention border once at the start, so the presenter sees the moment the take begins.

## Export

Export is a compositor that `rec` owns, which writes raw frames to an encoder. It does not use ffmpeg filtergraphs. The same compositor runs on every platform, so a take looks the same wherever it is exported. The macOS CoreImage exporter is the current implementation of this and gets replaced by the portable one.

Layouts are data: a table of presets keyed by aspect, each giving the canvas size, the screen rect, and the camera rect and shape.

- `16:9` is 1920x1080. The screen is fit inside with a small margin on a dark background. The camera, if present, is a circle in the bottom-right corner.
- `9:16` is 1080x1920, and the terminal always fills it. Each export picks a viewport from the grid's shape. A grid whose fitted panel covers at least three quarters of the height, like one recorded with `--for 9:16`, is fit whole (`fit`). A wider grid would shrink to a strip with empty bands above and below, so instead its rows fill the height and a window as wide as the frame, inside a 40px margin, crops the columns (`follow`). A 100x30 take shows about 37 columns at a 45px font this way. The window pans to follow the action: the cursor when the program shows it, and otherwise the cells that gained ink since the previous frame, which covers TUIs that hide the cursor. Focus inside the window, clear of a margin of a sixth of its width, does not move it, so typing a few characters holds still. When focus leaves, the window recenters on it, or starts at the left end of new ink wider than the window. It glides there on a critically damped spring that settles in about half a second, stays inside the grid, and holds still on idle frames. Its position depends only on the frames before it, so re-exports match. The camera, if present, is a large circle in the lower portion. Without a camera, the screen gets the full height.

`tty` takes are rendered at export. The cast is replayed through a terminal emulator (libghostty-vt is the intended engine) onto a canvas. Font, theme, and pixel scale are export choices. The grid (cols x rows) is fixed when the take is recorded, because the program laid out its output for that size. A take meant for vertical video records with `--for 9:16`, so 9:16 shows every column instead of panning. Typed input from the timeline can drive a typing highlight.

The attention border is a risograph ring around the camera circle, or around the visible terminal panel (the window, when 9:16 follows) when there is no camera: one grainy ring per spot ink (fluorescent pink, riso blue, yellow), each on its own plate, multiplied where they overlap. Its look at time t is a pure function. In the intro, from 0 to about 1.5s, the plates start far out of register, snap into place by 0.75s, and kick apart once on the beat. After that, a thin ring stays slightly misregistered and boils, wobbling at 10 fps. The border is off by default, and `--border` turns it on.

### Tighten

An agent drives a take with guessed sleeps and slow think time between inputs, so the raw video is mostly dead air. `--tighten` removes it using only data the take already has: the input events in timeline.json and every output chunk in term.cast. The same input always gives the same edit.

The cast is replayed through the emulator, and each export frame whose screen differs from the frame before is a change. A change is minor when it edits at most 24 cells on at most 2 rows that already had text, like a spinner glyph and an elapsed-time counter. Any other change is content. Changes and inputs closer than 0.5s form a burst. That splits the take into segments that cover it end to end:

| kind | span | output |
|---|---|---|
| `lead` | before anything is drawn | cut |
| `typing` | an input through its echo: 0.5s, plus 0.1s per typed character, ended early by the next input | 1x |
| `content` | the rest of a burst: output nobody just asked for, or output that outlasts the echo | 1x for 3s, the remainder at 2x |
| `busy` | a quiet span (no changes, or only minor ones) that ends in output the program produced on its own, like a spinner before a result | whole if 2s or shorter, otherwise compressed to 2s, but never faster than 12x and never shorter than the reading hold |
| `settled` | a quiet span that ends in an input | the reading hold, then a cut to 0.25s before the input so the viewer sees it land |
| `end` | a quiet span that ends the take | the reading hold, at least 2s |

The reading hold is `clamp(0.6 + words / 4, 1.2, 6)` seconds. `words` counts the words on the span's first screen that the viewer has not already read. A word counts as read once a quiet screen has shown it, or once `rec type` typed it, so a shell screen that comes back after a full-screen program exits holds briefly. A quiet span shorter than its hold plays whole.

The policy yields a monotonic piecewise-linear map from output time to take time, with a jump at each cut. Export draws frame `f` from the screen at `take_time((f + 1) / 30)`. The 9:16 camera and the attention border run on output time, as they do without `--tighten`. Cuts fall only inside quiet spans, so they skip nothing but minor changes.

Each export then carries the edit, so an agent can inspect it:

```json
"tightened": {
  "duration_raw": 179.788,
  "duration": 37.679,
  "segments": [
    { "kind": "settled", "take": [117.209, 158.643], "out": [27.757, 34.007], "words": 194, "hold": 6.0 },
    { "kind": "typing",  "take": [158.643, 158.715], "out": [34.007, 34.079] }
  ]
}
```

`take` and `out` are each segment's span on the take clock and in the video. Segments run back to back from 0 to `duration_raw` and from 0 to `duration`. `words` appears on `busy`, `settled`, and `end`, and `hold` on `settled` and `end`. Tighten does not judge meaning: a fumbled input that gets undone, or which screen is the payoff, needs a model-based pass that edits this segment list.

An export opens at the latest video track offset, the first moment every video track has a picture, so it never starts on dead frames. Earlier media from any track is trimmed. Mic audio is muxed in with its offset applied. Output is H.264 + AAC MP4.

## Build order

1. `tty` end to end: `rec start --tty`, `type`, `key`, `wait`, `screen`, `stop`, rendering the cast to frames, and export to MP4 inside a Vercel Sandbox. This alone covers herdr, Claude Code mods, and CLI demos.
2. The portable compositor with layouts and the border, replacing the CoreImage exporter. Upload.
3. The `x11` source: Xvfb lifecycle, x11grab, XTEST input, `rec screen --png`.
4. The macOS capture backend writes version 2 takes and uses the shared exporter.
5. Later: OCR for `rec wait` on `x11`, auto-zoom on clicks, blur of sensitive regions, transcription and summary, TTS narration, a Claude Code mod, Wayland.

## Open decisions

- **Language for the portable core.** The Linux binary needs PTY handling, a terminal emulator, a compositor, and a static musl build, and none of that uses Apple frameworks. The recommendation is Rust: mature PTY and image crates, it can call libghostty-vt through its C API, and the static build is easy. The Swift code stays as the macOS capture backend.
- **Bundling the encoder:** a static ffmpeg download versus linking openh264 or x264 into the binary. x264 is GPL, and openh264 is BSD-licensed with Cisco's patent arrangement.
