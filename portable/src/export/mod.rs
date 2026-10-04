//! `rec export`: compose a take's frames at 30 fps into each layout and encode with ffmpeg.
//! A tty take replays term.cast through vt100; an x11 take decodes screen.mp4.

mod border;
mod camera;
pub mod pointer;
pub mod layout;
mod render;
mod screen;
pub mod tighten;
pub mod theme;
mod view;

use crate::error::{RecError, Result};
use crate::model::{self, Source, Take};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

pub const FPS: u32 = 30;

pub struct ExportOptions {
    pub layouts: Vec<layout::Preset>,
    pub theme: &'static theme::Theme,
    pub font_px: Option<f32>,
    pub border: bool,
    pub cursor: pointer::CursorMode,
    pub pacing: Pacing,
}

/// How export maps video time to take time.
pub enum Pacing {
    /// Play the take as recorded.
    Raw,
    /// `--tighten`: the policy's edit.
    Tighten,
    /// `--plan`: an edit written by `--plan-out` and changed by a caller.
    Plan(tighten::Plan),
}

#[derive(Debug, Serialize)]
pub struct Export {
    pub layout: &'static str,
    /// `fit` shows the whole source; `follow` crops to a window that pans with the action.
    pub viewport: &'static str,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tightened: Option<tighten::Tightened>,
}

/// A finished tty take's terminal stream.
struct TtyTake {
    cast: crate::cast::Cast,
    size: model::Size,
}

impl TtyTake {
    fn read(dir: &Path) -> Result<TtyTake> {
        let cast_path = dir.join(model::CAST_FILE);
        let cast_file = std::fs::File::open(&cast_path)
            .map_err(|e| RecError::io(&cast_path.display().to_string(), e))?;
        let cast = crate::cast::read(std::io::BufReader::new(cast_file))?;
        let size = model::Size {
            cols: cast.header.width,
            rows: cast.header.height,
        };
        Ok(TtyTake { cast, size })
    }

    fn segments(&self, dir: &Path, duration: f64) -> Result<Vec<tighten::Segment>> {
        let timeline = model::Timeline::read(dir)?;
        let points = tighten::analyze(&self.cast.output, &timeline.events, self.size, duration);
        Ok(tighten::segment(&points, duration))
    }
}

/// A finished take, read for export.
struct Loaded {
    take: Take,
    duration: f64,
    recording: Recording,
}

enum Recording {
    Tty(TtyTake),
    Screen(screen::ScreenTake),
}

impl Loaded {
    fn read(dir: &Path) -> Result<Loaded> {
        let take = Take::read(dir)?;
        let duration = take.duration.ok_or_else(|| {
            RecError::new(
                "not_finished",
                format!("{} has no duration yet; stop it first", take.id),
            )
        })?;
        let recording = match take.source {
            Source::Tty { .. } => Recording::Tty(TtyTake::read(dir)?),
            Source::X11 { .. } => Recording::Screen(screen::ScreenTake::read(dir, &take)?),
        };
        Ok(Loaded {
            take,
            duration,
            recording,
        })
    }

    /// Tighten reads the terminal stream; screen takes need a pixel-diff pass that comes later.
    fn tty(&self) -> Result<&TtyTake> {
        match &self.recording {
            Recording::Tty(tty) => Ok(tty),
            Recording::Screen(_) => Err(crate::recorder::not_supported(
                "--tighten and --plan",
                "x11",
            )),
        }
    }
}

/// `--plan-out`: the tighten plan for the take at `dir`, without rendering.
pub fn plan(dir: &Path) -> Result<tighten::Plan> {
    let loaded = Loaded::read(dir)?;
    let segments = loaded.tty()?.segments(dir, loaded.duration)?;
    Ok(tighten::Plan::new(
        &loaded.take.id,
        &segments,
        &tighten::POLICY,
        loaded.duration,
    ))
}

/// One layout's pictures, frame by frame.
trait Frames {
    /// Draws frame `f` into `canvas` if it differs from what was last drawn there, and says
    /// whether it did.
    fn draw(&mut self, f: u64, canvas: &mut [u8]) -> Result<bool>;
}

struct TtyFrames<'a> {
    cast: &'a crate::cast::Cast,
    map: &'a tighten::TimeMap,
    parser: vt100::Parser,
    view: view::View<'a>,
    next: usize,
}

impl Frames for TtyFrames<'_> {
    fn draw(&mut self, f: u64, canvas: &mut [u8]) -> Result<bool> {
        // Each frame shows the screen as of the end of its interval.
        let t = self.map.take_time((f + 1) as f64 / FPS as f64);
        let mut changed = f == 0;
        while let Some((_, data)) = self.cast.output.get(self.next).filter(|(at, _)| *at <= t) {
            self.parser.process(data.as_bytes());
            self.next += 1;
            changed = true;
        }
        Ok(self.view.draw(self.parser.screen(), changed, canvas))
    }
}

pub fn export(dir: &Path, opts: &ExportOptions) -> Result<Vec<Export>> {
    let loaded = Loaded::read(dir)?;
    let duration = loaded.duration;
    let (map, tightened) = match &opts.pacing {
        Pacing::Raw => (tighten::TimeMap::identity(duration), None),
        Pacing::Tighten => {
            let segments = loaded.tty()?.segments(dir, duration)?;
            let (map, report) = tighten::plan(&segments, &tighten::POLICY, duration);
            (map, Some(report))
        }
        Pacing::Plan(plan) => {
            let segments = loaded.tty()?.segments(dir, duration)?;
            let cuts = plan.cuts(&segments)?;
            let (map, report) =
                tighten::retime(&segments, &cuts, tighten::POLICY.preroll, duration);
            (map, Some(report))
        }
    };
    let ffmpeg = find_ffmpeg()?;
    let fonts = render::Fonts::load();

    let mut out = Vec::new();
    for preset in &opts.layouts {
        let path = dir.join(format!("export-{}.mp4", preset.slug));
        let (frames, kind) = match &loaded.recording {
            Recording::Tty(tty) => {
                let frames = ((map.duration() * FPS as f64).round() as u64).max(1);
                let viewport =
                    layout::viewport(preset, tty.size, fonts.cell_metrics(), opts.font_px)?;
                eprintln!(
                    "rec: exporting {} ({frames} frames, {}px font, {} viewport)",
                    path.display(),
                    viewport.font_px(),
                    viewport.kind()
                );
                let mut source = TtyFrames {
                    cast: &tty.cast,
                    map: &map,
                    parser: vt100::Parser::new(tty.size.rows, tty.size.cols, 0),
                    view: view::View::new(&fonts, opts.theme, viewport, preset),
                    next: 0,
                };
                let ring = opts.border.then(|| viewport.panel());
                encode(&mut source, frames, preset, ring, &ffmpeg, &path)?;
                (frames, viewport.kind())
            }
            Recording::Screen(take) => {
                let frames = take.frames(duration);
                let viewport = layout::screen_viewport(preset, take.width, take.height);
                eprintln!(
                    "rec: exporting {} ({frames} frames, {} viewport)",
                    path.display(),
                    viewport.kind()
                );
                let mut source =
                    screen::ScreenFrames::new(
                    &ffmpeg,
                    take,
                    preset,
                    viewport,
                    opts.theme.canvas,
                    opts.cursor,
                )?;
                let ring = opts.border.then(|| viewport.panel());
                encode(&mut source, frames, preset, ring, &ffmpeg, &path)?;
                (frames, viewport.kind())
            }
        };
        out.push(Export {
            layout: preset.aspect,
            viewport: kind,
            path,
            width: preset.width,
            height: preset.height,
            duration: model::round_t(frames as f64 / FPS as f64),
            tightened: tightened.clone(),
        });
    }
    Ok(out)
}

/// Encodes `count` frames of `source`, with the attention border around `ring` if given.
fn encode(
    source: &mut dyn Frames,
    count: u64,
    preset: &layout::Preset,
    ring: Option<layout::Rect>,
    ffmpeg: &Path,
    path: &Path,
) -> Result<()> {
    let mut enc = Encoder::spawn(ffmpeg, path, preset.width, preset.height)?;
    let ring = ring.map(border::Target::screen);
    let mut canvas = vec![0u8; (preset.width * preset.height * 4) as usize];
    let mut framed = if ring.is_some() {
        canvas.clone()
    } else {
        Vec::new()
    };
    let mut painted: Option<(border::Look, border::Overlay)> = None;
    for f in 0..count {
        let drawn = source.draw(f, &mut canvas)?;
        let Some(ring) = &ring else {
            enc.write(&canvas)?;
            continue;
        };
        let look = border::look(f as f64 / FPS as f64);
        let new_look = !matches!(&painted, Some((shown, _)) if *shown == look);
        if new_look {
            painted = Some((look, ring.paint(&look, preset.width, preset.height)));
        }
        if drawn || new_look {
            framed.copy_from_slice(&canvas);
            painted
                .as_ref()
                .expect("painted above")
                .1
                .apply(&mut framed);
        }
        enc.write(&framed)?;
    }
    enc.finish()
}

/// The grid that fills `layout` at a legible size in the embedded font, for `--for`.
pub fn grid_for(layout: &str) -> Result<model::Size> {
    let preset = layout::preset(layout)?;
    Ok(layout::grid_for(
        &preset,
        render::Fonts::load().cell_metrics(),
    ))
}

/// The x11 screen that fills `layout` pixel for pixel inside its margin, for `--for`.
pub fn screen_for(layout: &str) -> Result<model::Frame> {
    let (w, h) = layout::screen_for(&layout::preset(layout)?);
    Ok(model::Frame {
        x: 0,
        y: 0,
        width: w,
        height: h,
    })
}

pub fn find_on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

fn find_ffmpeg() -> Result<PathBuf> {
    find_on_path("ffmpeg").ok_or_else(|| {
        RecError::new(
            "missing_dependency",
            "ffmpeg not found on PATH; install it (apt-get install ffmpeg, brew install ffmpeg)",
        )
    })
}

struct Encoder {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl Encoder {
    fn spawn(ffmpeg: &Path, out: &Path, w: u32, h: u32) -> Result<Encoder> {
        let mut child = Command::new(ffmpeg)
            .args(["-y", "-hide_banner", "-loglevel", "error"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{w}x{h}"), "-r", &FPS.to_string(), "-i", "-"])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"])
            .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RecError::new("missing_dependency", format!("cannot run ffmpeg: {e}")))?;
        let mut stderr = child.stderr.take().expect("piped");
        let stderr = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });
        Ok(Encoder {
            stdin: child.stdin.take(),
            child,
            stderr: Some(stderr),
        })
    }

    fn write(&mut self, frame: &[u8]) -> Result<()> {
        let stdin = self.stdin.as_mut().expect("open until finish");
        if stdin.write_all(frame).is_err() {
            return Err(self.failure());
        }
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        drop(self.stdin.take());
        match self.child.wait() {
            Ok(s) if s.success() => Ok(()),
            _ => Err(self.failure()),
        }
    }

    fn failure(&mut self) -> RecError {
        drop(self.stdin.take());
        let _ = self.child.wait();
        let log = self
            .stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        RecError::new("encode_failed", format!("ffmpeg failed: {}", log.trim()))
    }
}
