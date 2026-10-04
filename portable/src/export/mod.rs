//! `rec export`: replay term.cast through vt100 at 30 fps and encode with ffmpeg.

mod border;
mod camera;
pub mod layout;
mod render;
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
    pub tighten: bool,
}

#[derive(Debug, Serialize)]
pub struct Export {
    pub layout: &'static str,
    /// `fit` shows the whole grid; `follow` crops to a window that pans with the action.
    pub viewport: &'static str,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tightened: Option<tighten::Tightened>,
}

pub fn export(dir: &Path, opts: &ExportOptions) -> Result<Vec<Export>> {
    let take = Take::read(dir)?;
    let Source::Tty { .. } = take.source;
    let duration = take.duration.ok_or_else(|| {
        RecError::new(
            "not_finished",
            format!("{} has no duration yet; stop it first", take.id),
        )
    })?;
    let cast_path = dir.join(model::CAST_FILE);
    let cast_file = std::fs::File::open(&cast_path)
        .map_err(|e| RecError::io(&cast_path.display().to_string(), e))?;
    let cast = crate::cast::read(std::io::BufReader::new(cast_file))?;
    let size = model::Size {
        cols: cast.header.width,
        rows: cast.header.height,
    };
    let ffmpeg = find_ffmpeg()?;
    let fonts = render::Fonts::load();
    let (map, tightened) = if opts.tighten {
        let timeline = model::Timeline::read(dir)?;
        let points = tighten::analyze(&cast.output, &timeline.events, size, duration);
        let segments = tighten::segment(&points, duration);
        let (map, report) = tighten::plan(&segments, &tighten::POLICY, duration);
        (map, Some(report))
    } else {
        (tighten::TimeMap::identity(duration), None)
    };
    let frames = ((map.duration() * FPS as f64).round() as u64).max(1);

    let mut out = Vec::new();
    for preset in &opts.layouts {
        let viewport = layout::viewport(preset, size, fonts.cell_metrics(), opts.font_px)?;
        let mut view = view::View::new(&fonts, opts.theme, viewport, preset);
        let path = dir.join(format!("export-{}.mp4", preset.slug));
        eprintln!(
            "rec: exporting {} ({frames} frames, {}px font, {} viewport)",
            path.display(),
            viewport.font_px(),
            viewport.kind()
        );
        let mut enc = Encoder::spawn(&ffmpeg, &path, preset.width, preset.height)?;
        let mut parser = vt100::Parser::new(size.rows, size.cols, 0);
        let ring = opts.border.then(|| border::Target::screen(viewport.panel()));
        let mut term = vec![0u8; (preset.width * preset.height * 4) as usize];
        let mut framed = if ring.is_some() { term.clone() } else { Vec::new() };
        let mut painted: Option<(border::Look, border::Overlay)> = None;
        let mut next = 0;
        for f in 0..frames {
            // Each frame shows the screen as of the end of its interval.
            let t = map.take_time((f + 1) as f64 / FPS as f64);
            let mut changed = f == 0;
            while let Some((_, data)) = cast.output.get(next).filter(|(at, _)| *at <= t) {
                parser.process(data.as_bytes());
                next += 1;
                changed = true;
            }
            let drawn = view.draw(parser.screen(), changed, &mut term);
            let Some(ring) = &ring else {
                enc.write(&term)?;
                continue;
            };
            let look = border::look(f as f64 / FPS as f64);
            let new_look = !matches!(&painted, Some((shown, _)) if *shown == look);
            if new_look {
                painted = Some((look, ring.paint(&look, preset.width, preset.height)));
            }
            if drawn || new_look {
                framed.copy_from_slice(&term);
                painted.as_ref().expect("painted above").1.apply(&mut framed);
            }
            enc.write(&framed)?;
        }
        enc.finish()?;
        out.push(Export {
            layout: preset.aspect,
            viewport: viewport.kind(),
            path,
            width: preset.width,
            height: preset.height,
            duration: model::round_t(frames as f64 / FPS as f64),
            tightened: tightened.clone(),
        });
    }
    Ok(out)
}

/// The grid that fills `layout` at a legible size in the embedded font, for `--for`.
pub fn grid_for(layout: &str) -> Result<model::Size> {
    let preset = layout::preset(layout)?;
    Ok(layout::grid_for(&preset, render::Fonts::load().cell_metrics()))
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
