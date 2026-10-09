---
name: agentcam
description: Record a demo video of a terminal program, TUI, CLI, or X11 app with the agentcam CLI, drive it through the demo, and export 16:9 or 9:16 MP4s with a link. Use when asked to record, film, screen-capture, or make a demo, clip, GIF-style video, or vertical/social video of something running in this machine or VM.
---

# agentcam

agentcam records a program it runs itself, takes your input while recording, and exports finished video. Every command prints one JSON object to stdout. Failures print `{"error": {"code", "message"}}` and exit nonzero, so check the exit code of every step.

## Before the first take

Run `agentcam doctor`. If `agentcam` is missing, install it:

```sh
curl -fsSL https://raw.githubusercontent.com/davekiss/agentcam/main/install.sh | sh
```

Export needs `ffmpeg`. The x11 source also needs `Xvfb` (`apt-get install -y ffmpeg xvfb`). Fix what `doctor` lists under `missing` before you record.

## Pick the source

- A terminal program (a CLI, a TUI, Claude Code, htop): `--tty`. No display needed, almost no CPU.
- Anything with a window (a browser, an Electron or GTK app, a GUI terminal): `--x11`, Linux only.

Record with `--for 9:16` when the video is for social or phones, `--for 16:9` otherwise. The size is fixed at record time; a wide take exported to 9:16 crops to a panning window.

## The take

```sh
agentcam start --tty --for 9:16 -- <command>    # or: --x11 --for 9:16 -- <app>
agentcam wait --idle 1                          # let it draw before you act
agentcam type 'some input'                      # human-paced typing
agentcam key Return                             # keys and chords: ctrl+c, Escape, Up, F2
agentcam wait --text 'expected output' --new    # tty: block until it appears
agentcam wait --idle 1                          # x11: wait for the screen to settle
agentcam click <x> <y>                          # x11 pixels; tty cells when the app has mouse reporting
agentcam screen                                 # check what is on screen now
agentcam stop                                   # prints take.json; .id is the take
```

Rules for a good take:

- Never `sleep`. Wait on the screen with `agentcam wait --text` (tty) or `agentcam wait --idle` (both), so the video paces itself on the app.
- After each step, check it worked with `agentcam screen` before going on. A mistake you notice later means a new take.
- Plan the demo before `start`. Keep it short: show the one thing, then stop.
- Leave the final screen up for a second (`agentcam wait --idle 1`) before `agentcam stop`.

## Export

```sh
agentcam export <take> --layout 9:16 --tighten              # tty takes: cut dead air
agentcam export <take> --layout 9:16                        # x11 takes: no --tighten
agentcam export <take> --layout 9:16 --tighten --upload blob   # also upload; needs BLOB_READ_WRITE_TOKEN
```

`--upload` also takes any `https://` presigned PUT URL (S3, R2, GCS, Mux). With no `--layout`, both 16:9 and 9:16 are exported.

## Check the video before you hand it over

Every export reviews itself. Each entry in `exports` has a `review` with `checks` and a contact sheet next to the MP4 (`export-9x16.sheet.png`, one labelled frame every second or so).

1. Read `review.checks`. An `error` (`static`, `blank`) means the video is broken: fix the take and record again. `--upload` refuses such a video with `review_failed` and uploads nothing.
2. A `long_idle` warning means dead air. On a tty take, export again with `--tighten`.
3. Open the image at `review.sheet` and look at it. Confirm each step of the demo is visible and legible, the text is big enough, and nothing unexpected is on screen. The JSON cannot tell you the demo is right; the sheet can.
4. To look closer at a moment: `agentcam review <file.mp4> --at <seconds>` writes that frame full size.

Hand back the MP4 path, or the `url` when uploaded, and say what the video shows.
