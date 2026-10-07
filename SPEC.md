# rec

A screen recorder for agents. Its main user is an agent running in a headless Linux microVM (Vercel Sandbox, E2B, Modal, Daytona, Fly) that needs to make a video of what it built: a TUI, a Claude Code mod, a CLI demo, or a desktop app. A person on a Mac is the second user. Every command prints one JSON object to stdout so the calling program can read the result.

Two ideas hold the design together:

1. **Recording and composing are separate.** `rec` records the raw pieces of a take: the screen or terminal stream, optional camera and mic, and what happened as data. Layouts (16:9, 9:16), theming, and the attention border are applied at export. One recording gives every aspect ratio, and an agent can edit from the data instead of the pixels.
2. **In a microVM, `rec` owns the environment it records.** A Firecracker VM has no display, GPU, camera, or sound device. `rec` creates the terminal or virtual display, runs the program inside it, and drives it with input, so the agent can launch, type, wait, and film without any desktop.

Status: the macOS backend (ScreenCaptureKit, camera, mic, preview panel, CoreImage export) is implemented in Swift. The portable Rust core in `portable/` implements the `tty` and `x11` sources, input, export, and upload.

## Verified on Vercel Sandbox (2026-10-03)

Probed on the default image: Ubuntu 26.04, kernel 6.18, 4 vCPU, 8 GB, non-root `ubuntu` user with sudo.

- No `/dev/dri` and no `/dev/snd`. `/dev/pts` is present, and `script`, `tmux`, `claude`, and `codex` come preinstalled.
- `apt-get install xvfb ffmpeg xdotool mesa` took 34s. `ghostty` 1.3 is in the Ubuntu archive and took 7s more.
- Ghostty runs on Xvfb using Mesa llvmpipe (OpenGL 4.5, software). A 10s `ffmpeg -f x11grab` capture at 1920x1080 and 30 fps kept 299 of 300 frames. During the capture Ghostty used about 90% of one core, ffmpeg (x264 ultrafast) about 70%, and Xvfb about 20%. xdotool typing into Ghostty works.
- `script --log-io --log-timing` captures a PTY session with timing and no display: 1.4 KB for a one-second session. tmux runs htop headless.

So the pixel route works but costs about two cores, and the terminal route is nearly free.

`rec`'s own x11 source, measured on the same box (2026-10-04) with Ghostty filling a 1920x1080 screen at an idle prompt: ffmpeg (x264 `fast`) used about 75 to 90% of one core, Ghostty 4 to 20%, Xvfb 6%, and the recorder itself, sampling the pointer at 30 Hz, under 1%. Xvfb plus ffmpeg's first frame takes about a second and a half. Dependency install time (around 40s per boot) is the biggest cost an agent pays, which is why install is a requirement below.

## Sources

A take records exactly one primary source.

| kind | where | how it is captured |
|---|---|---|
| `tty` | Linux, macOS | `rec` spawns the command in a PTY it owns and records the output byte stream with timestamps. No display needed. Rendered to pixels at export. |
| `x11` | Linux | `rec` starts Xvfb at a given size, the agent (or `rec`) launches apps into it, and it is captured with x11grab into H.264. Covers GUI apps and real terminal emulators like Ghostty. |
| `display`, `window` | macOS | ScreenCaptureKit. Implemented. |

Terminal programs should use `tty`. It is lossless, re-renders at any size and theme, and costs almost nothing during capture. Use `x11` only when the window chrome or a GUI is the point.

Camera and mic are optional tracks, available only where the devices exist (macOS today). Wayland desktops (xdg-desktop-portal ScreenCast over PipeWire) are out of scope until someone needs them.

## Input

In a microVM, nobody else can type. `rec` sends input to the source it owns, and logs every input it sends into the timeline with an exact timestamp. Those logged events feed click highlights, typing effects, and later auto-zoom. This replaces macOS input monitoring for these sources.

- `tty` input is written to the PTY. Keys are the bytes a modern terminal sends: xterm's legacy encoding, or the kitty keyboard protocol once the program turns it on (see `rec key`). Mouse input becomes the xterm mouse reports a terminal would send, in the mode and encoding the program turned on.
- `x11` input goes through XTEST (what xdotool uses), sent by `rec` over its own X connection, so xdotool is not needed.
- On macOS sources, `rec` only observes the cursor and clicks, as it does today.

### The terminal a tty program sees

Programs ask their terminal what it is and pick their key encoding from the answers. Claude Code (v2.1, fullscreen) sends DA1, the kitty keyboard query, and XTVERSION at startup, and with no answers it falls back to legacy keys, where `ctrl+j` is the same byte as Enter. So the `tty` recorder answers as a terminal does. It watches the program's output through the emulator, so a query split across reads, or written in the middle of a frame, is answered in order with the screen state it asks about. Replies are written to the PTY as input and never appear in term.cast.

| query | sequence | reply |
|---|---|---|
| Primary device attributes (DA1) | `CSI c` | `CSI ? 62 ; 22 c`, a VT220-class terminal with ANSI color |
| Secondary device attributes (DA2) | `CSI > c` | `CSI > 1 ; 10 ; 0 c` |
| Terminal name and version (XTVERSION) | `CSI > q` | `DCS > \| rec <version> ST` |
| Device status (DSR) | `CSI 5 n` | `CSI 0 n` |
| Cursor position (CPR) | `CSI 6 n`, `CSI ? 6 n` | `CSI <row> ; <col> R` (`CSI ? <row> ; <col> R`), 1-based, from the emulated screen |
| Kitty keyboard flags | `CSI ? u` | `CSI ? <flags> u` for the screen in use |
| DEC private mode (DECRQM) | `CSI ? <mode> $ p` | `CSI ? <mode> ; <1 set, 2 reset> $ y` for the modes the emulator tracks (cursor keys, cursor visibility, the alternate screen, mouse modes and encodings, bracketed paste), and `0` (not recognized) for the rest, such as synchronized output (2026) |

Other queries, such as the kitty graphics query or the window size in pixels, get no reply, as from a terminal without that feature. A program that sends DA1 after its other queries, as Claude Code does, learns that at once.

The recorder also keeps the kitty keyboard flags the program sets, one stack for the main screen and one for the alternate screen: `CSI > <flags> u` pushes (16 entries kept, the oldest evicted), `CSI < <n> u` pops `n` (default 1, and emptying the stack resets the flags to 0), and `CSI = <flags> ; <mode> u` sets the current flags (mode 1 replaces, 2 adds bits, 3 clears bits).

It sends no focus reports (`CSI I`, `CSI O`), even when the program turns on `?1004`. Claude Code treats an unknown focus state as focused and gates no input on focus; focus reports sent by hand changed nothing in it or in its plugins, and a focus-in would only arm its window-activation check, which takes a click within 400 ms of focus for the click that raised the window.

## Take folder

```
<out>/take-20261002-213501/
  take.json        manifest (written at start, finalized at stop)
  term.cast        tty only: asciicast v2 output stream
  screen.mp4       x11 / macOS: H.264, no audio        (screen.mov on macOS)
  screen-<t>.png   x11: written by `rec screen` with no --png path
  cam.mov          H.264 webcam                         (macOS, absent with --no-cam)
  mic.m4a          AAC microphone                       (macOS, absent with --no-mic)
  timeline.json    events on the take clock             (written at stop)
  markers.jsonl    appended to by `rec mark` while recording; merged into timeline.json at stop
  recorder.log     stderr of the background recorder
  export-16x9.mp4  written by `rec export`
  export-9x16.mp4
  export-9x16.sheet.png  contact sheet from export's review (see Review)
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
    { "kind": "screen", "file": "screen.mp4", "offset": 0.0,  "width": 1920, "height": 1080, "pointer": "timeline" },
    { "kind": "camera", "file": "cam.mov",    "offset": 0.12, "width": 1920, "height": 1080 },
    { "kind": "mic",    "file": "mic.m4a",    "offset": 0.03 }
  ]
}
```

Which `source` fields are present depends on the kind: `command` and `size` for `tty`; `command` (if `rec` launched one) and `frame` for `x11`; `id`, `title`, `app`, `frame`, and `scale` for macOS. When `status` is `failed`, take.json also carries `"error": {"code", "message"}`. Version 1 takes (macOS, before this change) stay readable.

A screen track's `pointer` says where the mouse pointer is. `timeline` means the capture left it out of the video and export draws it from the timeline's cursor and click events; `x11` takes write this. `baked` means the pointer is part of the video, so export draws nothing over it. A track without the field is `baked`, which covers `x11` takes recorded before the pointer moved to the timeline.

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
    { "t": 3.100, "type": "drag",   "x1": 0.1, "y1": 0.2, "x2": 0.4, "y2": 0.2, "button": "left" },
    { "t": 3.900, "type": "marker", "label": "ran tests" }
  ]
}
```

`t` is seconds on the take clock. `x` and `y` are normalized 0..1 relative to the source's frame, top-left origin, so they survive any crop or resolution: on `x11`, pixel `x` of a `width`-pixel screen is `x / width`, rounded to six places. For `tty` they are cell-center coordinates normalized to the grid: cell (`col`, `row`) of a `cols`x`rows` grid is `((col + 0.5) / cols, (row + 0.5) / rows)`. A `drag` carries its press point as `x1`, `y1` and its release point as `x2`, `y2`. On `x11` the cursor is sampled at 30 Hz and written only when it moves: the recorder reads the pointer with QueryPointer, and the first sample, logged at t0, is where the pointer starts, so export knows where it is from the first frame.

`type` and `key` events record only input that `rec` itself sent. Keystrokes a human types on macOS are never recorded. `rec type --secret` still sends the text but logs it as redacted.

## Commands

All output is a single JSON object on stdout. Human-readable progress goes to stderr. Failures print `{"error": {"code": "...", "message": "..."}}` and exit nonzero.

### Recording

- `rec start --tty [--size 120x36 | --for <layout>] -- <cmd...>` spawns `<cmd>` in a PTY under a background recorder and returns once it is recording: `{"take", "pid"}`. The take finishes when `rec stop` runs or the command exits. The grid defaults to 120x36. `--for 9:16` or `--for 16:9` picks the grid whose panel fills that layout's canvas at a legible size in the embedded font, so exporting to that layout fits with no crop and no empty bands beyond the margin: 53x45 for 9:16 (about 30px text on a 1080-wide frame) and 124x29 for 16:9. Passing both `--size` and `--for` fails with `bad_args`.
- `rec start --x11 [--size 1920x1080 | --for <layout>] [-- <cmd...>]` starts Xvfb on the first free display from `:99` (one with neither `/tmp/.X<n>-lock` nor `/tmp/.X11-unix/X<n>`), starts ffmpeg's x11grab on it, optionally launches `<cmd>` with `DISPLAY` set, and returns `{"take", "pid", "display": ":99"}` so the agent can launch more apps into it. It returns once ffmpeg is writing frames and, when it launched `<cmd>`, once that app has mapped a window, because input sent before then goes nowhere. There is no window manager, so `rec` moves and sizes the app's first window to cover the screen. The screen defaults to 1920x1080 and needs even sides. `--for 9:16` or `--for 16:9` picks the screen that fills that layout's canvas inside its margin pixel for pixel, so that export fits with no scaling: 1000x1840 for 9:16 and 1776x936 for 16:9. The recorder owns three children, Xvfb, ffmpeg, and the app, and the take finishes when `rec stop` runs or the app exits. Capture is `ffmpeg -f x11grab -framerate 30 -draw_mouse 0` into libx264 (`fast` preset) yuv420p, so screen.mp4 has no pointer in it: the pointer lives in the timeline and export draws it (see Export); the screen track's `offset` is the first frame's timestamp, read from ffmpeg, on the take clock.
- `rec start [--display <id> | --window <id> | --app <name>] [--no-cam] [--no-mic] [--no-preview]` is the macOS form. It is implemented, and the default source there is the main display.
- All `start` forms accept `--out <dir>`.
- `rec record --duration <seconds> [same flags as start]` records in the foreground until the duration ends, the command exits, or SIGINT. `rec start` spawns this same command detached, so there is one recording code path.
- `rec stop` stops the active recorder, waits for files to finalize, tears down anything `rec` started (PTY child; on `x11`, ffmpeg first so screen.mp4 ends with the app still on screen, then the app, then Xvfb and its lock files), and prints the finished take.json. The same teardown runs when the app exits, on SIGINT, SIGTERM, or SIGHUP, and when starting fails partway. When the recorder is killed outright, ffmpeg and the app die with it on Linux (PR_SET_PDEATHSIG), but Xvfb ignores that, so the next `rec` command finds the stale record and stops it.
- `rec status` prints `{"recording": true, "take", "elapsed", "source", "display"?}` or `{"recording": false}`. `display` is present on `x11`.
- `rec mark <label>` adds a marker at the current moment: `{"take", "t", "label"}`. Each line of markers.jsonl is `{"t", "label"}`.

### Driving and observing (tty, x11)

- `rec type <text> [--delay <ms>] [--secret]` types text at a human-looking pace (default 40 ms per character). On `x11` each character becomes its keysym and is pressed on the keycode the keyboard map gives it, with shift when the keysym is the key's shifted one. Characters the map lacks (`é`, `✔`) go on spare, empty keycodes for the duration of the run, xdotool's technique, and all of a run's missing characters are mapped in one change before typing starts, because clients reload their keymap on each change and can drop a keystroke while they do. On `tty` each character is sent as itself, unless the program asked the kitty keyboard protocol to report all keys, when each becomes the key event `rec key` would send for it (a newline is `Return`, a tab `Tab`).
- `rec key <combo>` sends a key or chord, such as `Return`, `ctrl+c`, or `alt+tab`. Modifiers are `ctrl`, `alt`, `shift`, and `super`, and key names are `Return`/`Enter`, `Tab`, `Escape`, `BackSpace`, `Space`, `Up`/`Down`/`Left`/`Right`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`, `Delete`, `F1` to `F12`, or a single character, one table for both sources. A letter under `ctrl`, `alt`, or `super` is its lowercase key on both, so `ctrl+C` is `ctrl+c`. On `tty` the bytes follow the kitty keyboard flags of the screen the program is on (see The terminal a tty program sees), as in kitty, Ghostty, or iTerm2:
  - With no flags, the legacy xterm bytes: control characters for `ctrl` (`ctrl+j` is LF, the same as Enter), `ESC` before the key for `alt`, SS3 cursor keys in application cursor mode. `shift` and `super` only combine with a key here on `x11`, or on `tty` once the program has turned on kitty flags; otherwise type the shifted character.
  - Disambiguate (1): `ctrl`, `alt`, and `super` chords and `Escape` become `CSI <code> ; <modifiers> u`, with the unshifted key's code point (`ctrl+j` is `CSI 106;5u`), and modified function keys become `CSI 1 ; <modifiers> <letter>` or `CSI <number> ; <modifiers> ~` from the protocol's table (`ctrl+Up` is `CSI 1;5A`, `shift+F3` is `CSI 13;2~`). Text, `Space`, and unmodified `Return`, `Tab`, `BackSpace`, and function keys keep their legacy bytes; modified ones (`shift+Tab`, `shift+Return`) are `CSI u` too. Modifiers are 1 plus shift 1, alt 2, ctrl 4, super 8.
  - Report event types (2): every key sent as an escape code is followed by its release (`:3` after the modifiers, as in `CSI 106;5:3u`); text, and unmodified `Return`, `Tab`, and `BackSpace`, have none unless all keys are reported.
  - Report alternate keys (4): a key sent as `CSI u` with `shift` held adds its shifted key (`ctrl+shift+a` is `CSI 97:65;6u`).
  - Report all keys (8): every key is an escape code, text and `Return`, `Tab`, and `BackSpace` included (`a` is `CSI 97u`, `Return` is `CSI 13u`), and each held modifier is pressed as its own key event first (left shift 57441, control 57442, alt 57443, super 57444), and released after when event types are on. A shifted character is its key with `shift` (`A` is `CSI 97;2u`, `!` is `1` with shift on a US layout).
  - Report associated text (16), with 8: a key that types text carries it as a third field (`shift+a` is `CSI 97;2;65u`).
- On `tty`, each command's input starts at least 40 ms (the default typing delay) after the previous command's input. Claude Code 2.1 delivers only the newest of the input events that reach a plugin's surface within one frame, so `rec type "x."` followed at once by `rec key Return` used to lose the `.`.
- `rec click <x> <y> [--button left|middle|right]`, `rec move <x> <y>`, and `rec drag <x1> <y1> <x2> <y2> [--button left] [--steps 8]` drive the mouse. On `x11` coordinates are pixels of the screen; on `tty` they are 0-based cells, column then row. A point off the screen or grid fails with `bad_args`. A click is logged as `{"type": "click", "x", "y", "button"}` and a drag as `{"type": "drag", "x1", "y1", "x2", "y2", "button"}`, with normalized coordinates. A drag presses at the start, moves in a straight line through `--steps` evenly spaced points (1 to 1000, repeats dropped) 15 ms apart, and releases at the end. On `tty` it holds the press 100 ms before the first move, as a hand does; with motion 15 ms after the press, Claude Code's plugin surfaces lost the press in 4 to 8 of 30 drags, and the drag extended the old selection instead of starting a new one.
  - On `x11` they go through XTEST. A move is not logged itself; the cursor samples record it.
  - On `tty` they reach the program only if it turned on mouse reporting, which `rec` reads from the emulated screen at each command. A click or drag on a program that has not fails with `mouse_off`. What is sent follows the mode: X10 (`?9`) reports presses only; `?1000` presses and releases; `?1002` adds motion while a button is held, so a drag's moves; `?1003` adds motion with no button, so `rec move`. A move the mode does not report is sent as nothing and still succeeds, so a script that moves before it clicks runs anywhere. A tty move is logged as a `cursor` event, since there is no pointer to sample. Reports use the encoding the program chose: SGR (`?1006`, `ESC[<b;x;yM`, `m` for a release) or the default X10 form (`ESC[M` and three bytes, each 32 plus the value), with 1-based x and y, buttons 0, 1, 2 for left, middle, right, 3 for a legacy release, and 32 added for motion. The default form cannot address past column or row 222 (byte 255), and UTF-8 (`?1005`) past 2014, so a cell beyond that fails with `bad_args` rather than sending a wrong position. Claude Code (v2.1, fullscreen) turns on `?1000`, `?1002`, `?1003`, and `?1006`, so it gets any-motion SGR reports, and `rec drag` selects text in it.
- `rec wait --text <regex> [--new] [--timeout <s>]` blocks until the current screen matches the pattern, so agents wait on output instead of sleeping. It prints `{"matched": true, "t"}`, or exits with error `timeout`. On `tty` it matches the emulated screen. On `x11` it needs OCR, which comes later, and fails with `not_supported`.
- `rec wait --idle <seconds> [--timeout <s>]` blocks until nothing has changed for that long, measured from when the wait starts, and prints `{"idle": true, "t"}`, or exits with error `timeout` (default 30s). On `tty` a change is any output from the program. On `x11` the recorder hashes the whole screen (GetImage) about ten times a second, and a change is a picture that differs from both of the last two distinct pictures, so a blinking cursor toggling between two pictures does not count. Agents use it in place of guessed sleeps: `rec start --x11 -- ghostty; rec wait --idle 1; rec type ...`.
  - `--new` (`tty` only) matches the program's output since the last `type` or `key` instead of the screen, so the echo of a command typed earlier does not count. The recorder marks its output stream just before each input byte reaches the PTY, and `--new` matches everything written after that mark, as plain text with escape sequences and carriage returns removed. Output that arrived between the input and the start of the wait still counts, so `rec key Return; rec wait --new` has no race, and repeating the wait gives the same answer. With no input sent yet, it covers all output so far. The recorder holds the last 1 MiB of output text.
- `rec screen [--png <path>]` prints what is on screen now: `{"text", "cols", "rows", "cursor"}` for `tty`. On `x11` it writes a PNG of the whole screen (GetImage of the root window) to `<path>`, or to `<take>/screen-<t>.png` without one, and prints `{"png", "width", "height", "t"}`. This is how the agent checks its work mid-take. `--png` on `tty` fails with `not_supported` for now; rendering the grid through the export renderer is the follow-up.

Each driving command records itself in the timeline and returns `{"t"}` on the take clock.

### Composing and delivering

- `rec export <take> [--layout 16:9] [--layout 9:16] [--region COL,ROW,COLS,ROWS ...] [--border] [--cursor auto|always|never] [--tighten [--plan-out <file>]] [--plan <file>] [--theme <name>] [--font <name>] [--upload <target>]` composes the take and prints `{"take", "exports": [{"layout", "viewport", "regions"?, "path", "width", "height", "duration", "url"?, "tightened"?, "review"}]}`. Every export reviews itself as it encodes, and `review` carries the result (see Review); there is no flag to skip it. With no `--layout`, it exports both. `viewport` is `fit` when the whole grid shows, `follow` when 9:16 crops a wide grid to a panning window, and `stack` when `--region` picks the cells to show (see Export). A `stack` export lists `regions` in the order given, each as `{"col", "row", "cols", "rows", "panel": {"x", "y", "w", "h"}}` with its panel in canvas pixels. `--region` is repeatable, counts grid cells from 0, and fails with `bad_args` when a region is empty or reaches past the take's grid; on an `x11` take it fails with `not_supported`, since pixel regions are a later step. `url` is present only with `--upload`. `--tighten` retimes the take so its pacing follows the program rather than the agent driving it (see Tighten), and adds `tightened` to each export. Tighten reads the terminal stream, so `--tighten`, `--plan-out`, and `--plan` fail with `not_supported` on an `x11` take; a pixel-diff tighten is a later step. `--tighten --plan-out` writes that edit as a plan file instead of rendering, and `--plan` renders with an edited plan (see Plans).
- `rec review <file.mp4 | take> [--at <seconds> ...]` runs the same review over an existing MP4, decoded by ffmpeg at 30 fps, and writes `<name>.sheet.png` next to it. Each `--at` also writes the frame at that time, full size, to `<name>.at-<seconds>.png` (the last frame when the time is past the end). For a file it prints the review plus `{"path", "frames": [{"t", "png"}]}`. For a take folder or id it reviews each `export-*.mp4` in it and prints `{"take", "reviews": [...]}`. Export does not use this command; it is for checking a file again.
- `rec sources` lists what can be captured: macOS displays and windows, plus `{"tty": true, "x11": <bool>}`. `x11` is true when `Xvfb` and `ffmpeg` are on `PATH`.
- `rec doctor` reports the platform, what each source kind needs, and what is missing, as JSON. Agents run it first. Its `sources.x11` is `{"ok", "needs": ["Xvfb", "ffmpeg"], "missing"}`, and a missing `Xvfb` also appears in the top-level `missing`.

The VM, and the take folder with it, is gone when the session ends, so delivery is part of export. `--upload` streams each finished export to its target and adds the `url` it can be fetched from. The target is one of:

- `blob`: Vercel Blob, with the read-write token from `BLOB_READ_WRITE_TOKEN`. Each export goes to `rec/<take-id>/export-<layout>.mp4` with no random suffix, and exporting the same take again overwrites it. `blob` uploads public blobs; `blob:private` is for stores configured as private, which reject public uploads, and its URL needs the token to fetch. `url` is the URL the Blob API returns. The request matches what `put()` in `@vercel/blob` 2.8 sends (`PUT https://vercel.com/api/blob/?pathname=...`, API version 12). `REC_BLOB_API_URL` replaces the API base, for tests.
- An `https://` URL: a presigned PUT, which S3, R2, GCS, and Mux direct uploads all hand out. It names one object, so it needs exactly one `--layout`, or export fails with `bad_args` before rendering. `url` is the presigned URL without its query string. Plain `http://` is accepted only for localhost.

A broken video is never delivered. When any export's review has an `error` check, `--upload` sends nothing and fails with `review_failed`, whose message names each error check and the sheet to look at. The MP4s and sheets stay on disk. Warnings never block an upload.

A missing token fails with `missing_credentials` naming the variable, before anything renders. A refused upload fails with `upload_failed`, carrying the HTTP status and the start of the response body. The export files stay on disk either way.

### Behavior shared by all commands

If two takes start in the same second, the second folder gets a `-2` suffix.

Commands are idempotent where it matters. `start` while recording returns an error naming the active take. `stop` with nothing recording returns `{"recording": false}`. A stale active record (pid no longer alive) is cleaned up silently, along with any Xvfb it left behind.

Active state lives in `active.json`, holding `{"pid", "take", "startedAt", "source", "display"?}`. A stale record with a `display` also has that display's Xvfb stopped, if it still runs, and its lock and socket files removed. It is in `~/Library/Application Support/rec/` on macOS and `$XDG_STATE_HOME/rec/` (falling back to `~/.local/state/rec/`) on Linux.

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

`x11` takes are decoded at export (`ffmpeg -i screen.mp4 -f rawvideo -pix_fmt rgba -`) and scaled bilinearly into the layout, starting at the screen track's first frame. They pick a viewport by the same rule as a grid: a screen whose fitted picture covers at least three quarters of the height is fit whole (`fit`), which covers 16:9 always and 9:16 for a portrait screen; a landscape screen on 9:16 is scaled to fill the height and cropped by a window as wide as the frame inside its margin (`follow`). The window uses the same camera as a terminal, in surface pixels instead of columns. The pointer, from the timeline's cursor and click events, plays the cursor's part, half a second ahead, so the window is already over a click when it lands. It plays that part whether or not the pointer is drawn. The ink is everything that has changed on screen since the last `type` or `key`, counted only within half a second (plus 50 ms per typed character) after it, so the window follows the echo of what was typed but not a cursor blinking elsewhere, and a title bar redrawn after the prompt widens the ink rather than pulling the window off the prompt. The first frame starts on the pointer, or the middle of the screen, with no glide.

When the screen track's `pointer` is `timeline`, export draws the pointer over the screen, in screen coordinates, so it scales and pans with the picture and is clipped to the panel. It is a white arrow with a dark outline and a soft shadow, drawn with antialiasing at 2.8% of the canvas height (30px on 16:9, 54px on 9:16), so it reads on light and dark screens and on a phone. It is always an arrow, whatever shape the app set; the shape is not recorded. A click adds a ring at the click point that expands and fades. A drag is drawn as a click at its press, and the cursor samples carry its motion. What is drawn at a moment is a pure function of the timeline, set by `--cursor`:

- `auto` (the default) shows the pointer only while it matters: it fades in when the pointer starts moving or shortly before a click, stays while it moves, and fades out once it has been still for a while. A pointer that only sits there, such as Ghostty's I-beam in the middle of the screen while the agent types, is never drawn.
- `always` draws it on every frame, from where it starts.
- `never` draws neither the pointer nor click rings.

| step | seconds |
|---|---|
| fade in, from the start of a move | 0.12 |
| lead-in before a click | 0.4 |
| hold after the last move or click | 1.0 |
| fade out after the hold | 0.25 |
| glide for a jump from rest | 0.25 |
| click ring | 0.4 |

`rec move` and `rec click` jump the pointer in one step, so a move from rest is drawn as a glide that eases from the old position and arrives when the pointer did; continuous motion is interpolated between samples. A take whose pointer is `baked` exports as recorded and ignores `--cursor`, and `tty` takes have no pointer.

A stacked viewport (`stack`) shows chosen regions of a tty grid instead of the whole grid, for a side-by-side TUI that would otherwise pan on 9:16. Claude Code at 150x44 with a pane docked on columns 84 to 150 exports as `--region 84,0,66,44 --region 0,0,84,44`: the pane on top, the chat below. Each region gets its own panel, and the panels stack top to bottom in the order given, 28px apart, with a 2px line in the theme's divider color centered in each gap. All panels share one font size, so text reads the same size in each: the largest at which every panel fits the width inside the margin and the panels plus the gaps fit the height. Each panel is centered horizontally, and the stack is centered vertically. Only the cells inside each region are rasterized, at that size, so text stays as crisp as a fit export. The 66-column and 84-column stack above, 44 rows each, is bound by height and gets a 15px font (9x20 cells) on 9:16; shorter regions get larger text. `--region` works on 16:9 too, by the same rule, and with `--font-size`, `--tighten`, and `--plan`, whose time map does not depend on the viewport.

`tty` takes are rendered at export. The cast is replayed through a terminal emulator (libghostty-vt is the intended engine) onto a canvas. Font, theme, and pixel scale are export choices. The grid (cols x rows) is fixed when the take is recorded, because the program laid out its output for that size. A take meant for vertical video records with `--for 9:16`, so 9:16 shows every column instead of panning. Typed input from the timeline can drive a typing highlight.

The attention border is a risograph ring around the camera circle, or around the visible terminal panel (the window, when 9:16 follows, and the bounds of all panels, when the export stacks regions) when there is no camera: one grainy ring per spot ink (fluorescent pink, riso blue, yellow), each on its own plate, multiplied where they overlap. Its look at time t is a pure function. In the intro, from 0 to about 1.5s, the plates start far out of register, snap into place by 0.75s, and kick apart once on the beat. After that, a thin ring stays slightly misregistered and boils, wobbling at 10 fps. The border is off by default, and `--border` turns it on.

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

`take` and `out` are each segment's span on the take clock and in the video. Segments run back to back from 0 to `duration_raw` and from 0 to `duration`. `words` appears on `busy`, `settled`, and `end`, and `hold` (how long the screen stays up) on `settled` and `end`. A segment a plan dropped carries `"dropped": true`.

#### Plans

Tighten does not judge meaning: a fumbled input that gets undone, or which screen is the payoff, needs a pass that understands the screens. A plan file lets one edit the policy's choices.

`rec export <take> --tighten --plan-out <file>` writes the plan and renders nothing, so it takes well under a second. It prints `{"take", "plan", "segments"}`. The plan holds the take id, `duration_raw`, an empty `purpose` for the caller to fill with what the video is about, and the segments in order:

```json
{ "id": 9, "kind": "settled", "take": [117.209, 158.643], "out_len": 6.0, "drop": false,
  "words": 194, "inputs": ["key Return"], "screen_text": "...", "new_text": "..." }
```

`id` is the segment's index. `out_len` is the policy's choice in seconds, and what it means depends on `kind`. For `settled` it is the hold before the cut to the preroll. For `end` it is how much of the final screen plays. For every other kind it is the length the whole span plays in, so a `busy` span's compressed length and a `typing` or `content` span's playback length. A hold never outlasts the time the take spent on that screen. `busy`, `settled`, and `end` segments also carry `screen_text`, the screen at the end of the span with trailing blank lines trimmed; `new_text`, the words the reading-time rule counted as unread, joined by spaces; `words`, their count; and `inputs`, the inputs sent since the previous quiet segment, as `type "<text>"`, `key <combo>`, `click <col> <row>`, or `drag <col> <row> <col> <row>`, the last two followed by `--button <name>` when it is not left. Clicks and drags are inputs to tighten like typing; moves are not.

`rec export <take> --plan <file>` renders with an edited plan in place of the policy. It honors each segment's `out_len`, and `"drop": true` cuts the segment entirely. Take time still only moves forward, so the screen after a cut is exactly what the take showed then. A dropped `settled` segment still plays its 0.25s preroll when the input after it is kept, so the viewer sees that input land. The plan must come from the same take: the segment count, each `id`, `kind`, and `take` span (to the millisecond) must match what the take segments into, or export fails with `bad_args` before rendering. An unedited plan renders the same video as `--tighten`. `--plan` cannot be combined with `--tighten`, and `--plan-out` needs `--tighten`. Unknown fields in a plan are ignored, so a caller can annotate segments.

An export opens at the latest video track offset, the first moment every video track has a picture, so it never starts on dead frames. Earlier media from any track is trimmed. Mic audio is muxed in with its offset applied. Output is H.264 + AAC MP4.

### Review

Agents film demos and file them without watching, so every export watches itself. Each frame written to the encoder also goes to a reviewer, the same one for `tty` and `x11` takes, and the export's JSON carries what it found:

```json
"review": {
  "sheet": "/.../export-9x16.sheet.png",
  "every": 1.0,
  "activity": [0.0, 0.0685, 0.0, 0.0805, 0.0, 0.0, 0.0, 0.0007],
  "idle": [{ "from": 4.0, "to": 7.0 }],
  "checks": []
}
```

A check looks like this:

```json
{ "code": "long_idle", "severity": "warning", "message": "nothing moves from 10s to 18s (8.0s); re-export with --tighten to cut dead air", "span": { "from": 10.0, "to": 18.0 } }
```

- `activity` has one value per second of video: the largest fraction of the canvas that any one frame in that second changed, rounded to 4 places. A pixel changes when a channel moves by more than 10, so a decoded video's encoder noise is not change. A pixel counts only when it differs from both of the last two distinct pictures, as in `rec wait --idle`, so a cursor blinking between two pictures is not change.
- A second is idle when its activity is below 0.0001, about one typed character on the smallest export font. `idle` lists each run of 3 or more idle seconds.
- `sheet` is a contact sheet PNG next to the MP4: a tile every `every` seconds from 0, plus the last frame, each labelled with its time. `every` is the video's length over 24, at least 1, rounded up to a half second, so a sheet has at most about 25 tiles. Tiles are about 270px on their short side, 6 to a row for 9:16 and 4 for 16:9.
- A frame is blank when it is one flat color: every pixel matches the corner's color (the canvas) or the center's (the panel, which is always centered), so an app that drew nothing on a panel counts. Blank is sampled every 0.5s and on the last frame.
- `checks` is empty when nothing is wrong. Each has a `code`, a `severity`, a `message`, and a `span` when it covers part of the video.

| code | severity | when |
|---|---|---|
| `static` | error | every second after the first is idle: the program drew once and froze, or never ran |
| `blank` | error | the last frame is blank, or more than half the samples are |
| `blank` | warning | otherwise, a blank run of 1s or more |
| `long_idle` | warning | an idle span of 7s or more, one second past tighten's longest reading hold, so a tightened export has one only when a plan held a screen that long. The message suggests `--tighten` on an untightened tty export. A `static` video gets no `long_idle` |

The review costs about 0.1s on a 20-second 1080x1920 tty export (1.2s before, 1.3s after, measured on a Mac).

## Build order

1. `tty` end to end: `rec start --tty`, `type`, `key`, `wait`, `screen`, `stop`, rendering the cast to frames, and export to MP4 inside a Vercel Sandbox. This alone covers herdr, Claude Code mods, and CLI demos.
2. The portable compositor with layouts and the border, replacing the CoreImage exporter. Upload.
3. The `x11` source: Xvfb lifecycle, x11grab, XTEST input, `rec screen --png`. Done, along with `rec wait --idle` for both sources.
4. The macOS capture backend writes version 2 takes and uses the shared exporter.
5. Later: OCR for `rec wait` on `x11`, pixel-diff `--tighten` for `x11`, `rec screen --png` on `tty`, auto-zoom on clicks, blur of sensitive regions, transcription and summary, TTS narration, a Claude Code mod, Wayland.

## Open decisions

- **Language for the portable core.** The Linux binary needs PTY handling, a terminal emulator, a compositor, and a static musl build, and none of that uses Apple frameworks. The recommendation is Rust: mature PTY and image crates, it can call libghostty-vt through its C API, and the static build is easy. The Swift code stays as the macOS capture backend.
- **Bundling the encoder:** a static ffmpeg download versus linking openh264 or x264 into the binary. x264 is GPL, and openh264 is BSD-licensed with Cisco's patent arrangement.
