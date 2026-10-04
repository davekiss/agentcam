# rec

A screen recorder for agents. An agent running in a headless Linux microVM (Vercel Sandbox, E2B, Modal, Daytona, Fly) can start a terminal program or a desktop app, drive it with keystrokes and clicks, and export a finished 16:9 or 9:16 video. Every command prints one JSON object to stdout, so the calling program can read the result.

Recording and composing are separate. `rec` records the raw pieces of a take and what happened as data, and `rec export` turns one take into each aspect ratio, retimes it, and optionally uploads it.

[SPEC.md](SPEC.md) is the contract for the take folder, the JSON shapes, and every command.

There are two implementations in this repo:

- `portable/` is the Rust `rec` for Linux and macOS. It records terminals (`tty`) and Linux desktops (`x11`). This is the one agents use.
- `Sources/` is the Swift `rec` for macOS. It records the screen, webcam, and mic with ScreenCaptureKit and draws a floating webcam preview.

## Install (portable)

Build from source with Rust. On Linux, a static musl build gives one binary with no runtime dependencies beyond ffmpeg:

```sh
cd portable
cargo build --release                                      # host build
cargo build --release --target x86_64-unknown-linux-musl   # static Linux binary
cp target/release/rec ~/.local/bin/rec
```

Export needs `ffmpeg` on `PATH`. The `x11` source also needs `Xvfb`. On Ubuntu:

```sh
sudo apt-get install -y ffmpeg xvfb
```

Run `rec doctor` to see what is available and what is missing.

## Record a terminal program

The `tty` source runs a command in a pseudo-terminal that `rec` owns and records its output stream. No display or GPU is needed, and recording costs almost nothing.

```sh
rec start --tty --for 9:16 -- claude     # --for picks a grid that fills a 9:16 frame
rec wait --idle 1                        # wait until the screen settles
rec type '/auto-mode-setup'
rec key Return
rec wait --text 'Looks good'             # wait for output instead of sleeping
rec screen                               # what is on screen now, as text
rec stop                                 # prints the finished take.json
```

`--size 120x36` sets the grid directly. The grid is fixed at record time, because the program lays out its output for it, so record with `--for 9:16` when you want vertical video.

## Record a desktop app (Linux)

The `x11` source starts a virtual display, launches an app on it, and records the screen.

```sh
rec start --x11 --for 9:16 -- ghostty --font-size=26
rec wait --idle 1.5
rec type 'echo hello'
rec key Return
rec move 300 1500
rec click 500 900
rec screen --png shot.png                # check the work mid-take
rec stop
```

`rec` sizes the app's window to fill the screen, since there is no window manager. The mouse pointer is not baked into the recording; it is stored in the timeline and drawn at export.

## Export

```sh
rec export <take>                                # both 16:9 and 9:16
rec export <take> --layout 9:16 --tighten        # cut the dead air
rec export <take> --layout 9:16 --border         # add the risograph attention border
rec export <take> --upload blob:private          # upload to Vercel Blob
```

- 9:16 always fills the frame. A take recorded for 9:16 fits whole. A wide take gets a camera that pans to follow the cursor, the changing text, or the mouse.
- `--tighten` retimes a `tty` take so its pacing follows the program rather than the agent driving it. Typing plays at 1x, a working spinner shrinks to about 2 seconds, and a settled screen holds for a reading time before cutting to the next input. `--tighten --plan-out plan.json` writes the edit as a plan, and `--plan plan.json` renders an edited one.
- `portable/scripts/jev_tighten.py` edits a plan with [TypeSafe](https://typesafe.ai) Jev judgments: it holds important screens longer, gives boilerplate less time, and cuts mistakes that were undone. It reads `TYPESAFE_API_KEY`.
- `--cursor auto` (the default) shows the pointer only while it moves and around clicks, with a ripple on each click. `always` and `never` are the alternatives.
- `--upload blob` or `blob:private` uploads each export to Vercel Blob using `BLOB_READ_WRITE_TOKEN`. Any `https://` URL is treated as a presigned PUT (S3, R2, GCS, Mux direct uploads) and takes one layout.

## The take folder

```
take.json        manifest: source, tracks, per-track sync offsets, status
term.cast        tty: the terminal output stream (asciicast v2)
screen.mp4       x11: H.264 screen capture, no pointer
timeline.json    typed text, keys, clicks, cursor samples, and markers on the take clock
markers.jsonl    markers appended by `rec mark` while recording
recorder.log     the background recorder's output
export-16x9.mp4  written by `rec export`
export-9x16.mp4
```

Takes land in `~/.local/share/rec/` on Linux and `~/Movies/rec/` on macOS unless `--out` names another folder.

## The macOS recorder (Swift)

Requires macOS 14 or later and Xcode 16.

```sh
swift build -c release
cp .build/release/rec /usr/local/bin/rec
rec start                                  # main display, webcam, and mic
rec start --app "Google Chrome" --no-cam
rec stop
rec export ~/Movies/rec/take-20261002-213501 --layout 9:16
```

macOS asks for Screen Recording, Camera, and Microphone permission for the app that runs `rec`, usually your terminal. A missing permission fails with `{"error": {"code": "permission_denied", ...}}` naming it.

## Development

```sh
cd portable && cargo build --release && cargo test && cargo clippy --all-targets
scripts/smoke.sh          # tty end to end
scripts/smoke-x11.sh      # x11 end to end, Linux only
scripts/upload-check.sh   # upload against a local server
python3 -m unittest scripts/test_jev_tighten.py
cd ..

swift test                                  # macOS recorder
scripts/verify-export.sh                    # macOS export from a synthesized take
```
