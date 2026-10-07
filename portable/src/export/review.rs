//! Every export reviews itself as it encodes: how much of the picture changes each second,
//! where nothing moves, whether it is blank or frozen, and a contact sheet to look at.
//! `rec review` runs the same reviewer over an MP4 decoded by ffmpeg.

use super::render::Fonts;
use super::theme::Rgb;
use super::{tighten, FPS};
use crate::error::{RecError, Result};
use crate::model::round_t;
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A second is idle when no frame in it changes this fraction of the canvas.
pub const IDLE_CHANGE: f64 = 0.0001;
/// Idle seconds in a row that make an `idle` span.
const IDLE_RUN: usize = 3;
/// An idle span this long is a `long_idle`. It outlasts tighten's longest reading hold, so a
/// tightened export only has one when its plan held a screen past the policy.
pub const LONG_IDLE: f64 = tighten::POLICY.reading.max + 1.0;
/// `static`: nothing changes after this many seconds.
const STATIC_AFTER: usize = 1;
/// Frames are checked for blank this often, in seconds.
const BLANK_EVERY: f64 = 0.5;
/// Blank on more than this fraction of the samples is an error.
const BLANK_MOST: f64 = 0.5;
/// A blank run this long is a warning.
const BLANK_RUN: f64 = 1.0;
/// Channels this close are the same color, so a decoded video's encoder noise is no change.
const NOISE: u8 = 10;
/// A sheet tile's shorter side, in pixels.
const TILE_SHORT: f64 = 270.0;
/// A sheet has about this many tiles, plus the last frame.
const TILES: f64 = 24.0;
const SHEET_GAP: u32 = 8;
const LABEL_H: u32 = 30;
const LABEL_PX: f32 = 20.0;
const SHEET_BG: Rgb = [0x1a, 0x1b, 0x20];
const LABEL_FG: Rgb = [0xe6, 0xe6, 0xe6];

#[derive(Debug, Clone, Serialize)]
pub struct Review {
    /// The contact sheet, next to the video.
    pub sheet: PathBuf,
    /// Seconds between sheet tiles.
    pub every: f64,
    /// For each second of video, the largest fraction of the canvas any one frame changed.
    pub activity: Vec<f64>,
    /// Runs of at least `IDLE_RUN` idle seconds.
    pub idle: Vec<Span>,
    /// Empty when nothing is wrong.
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Span {
    pub from: f64,
    pub to: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub code: CheckCode,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckCode {
    Blank,
    Static,
    LongIdle,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// What the reviewer found, before it is graded.
#[derive(Clone, Copy)]
enum Finding {
    Frozen,
    BlankEnd,
    BlankMost,
    BlankRun,
    LongIdle,
}

impl Finding {
    /// The one table of how each finding is reported and how bad it is.
    fn grade(self) -> (CheckCode, Severity) {
        match self {
            Finding::Frozen => (CheckCode::Static, Severity::Error),
            Finding::BlankEnd | Finding::BlankMost => (CheckCode::Blank, Severity::Error),
            Finding::BlankRun => (CheckCode::Blank, Severity::Warning),
            Finding::LongIdle => (CheckCode::LongIdle, Severity::Warning),
        }
    }

    fn check(self, message: String, span: Option<Span>) -> Check {
        let (code, severity) = self.grade();
        Check {
            code,
            severity,
            message,
            span,
        }
    }
}

/// What a `long_idle` message suggests, which depends on how the video was made.
#[derive(Clone, Copy)]
pub enum Advice {
    /// An untightened tty export: `--tighten` cuts dead air.
    Tighten,
    /// A `--tighten` or `--plan` export.
    Tightened,
    /// An x11 export or any MP4.
    Cut,
}

impl Review {
    pub fn errors(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| c.severity == Severity::Error)
    }
}

struct Tile {
    t: f64,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

/// Watches each frame of one video as it is written.
pub struct Reviewer {
    width: u32,
    height: u32,
    every: f64,
    /// Frame indexes of the tiles still to capture, soonest last.
    tile_frames: Vec<u64>,
    /// The last frame, and the distinct picture before it.
    prev: Vec<u8>,
    before: Vec<u8>,
    seen: u64,
    activity: Vec<f64>,
    blank: Vec<bool>,
    tiles: Vec<Tile>,
}

impl Reviewer {
    /// For a video of about `frames` frames at `FPS`.
    pub fn new(width: u32, height: u32, frames: u64) -> Reviewer {
        let every = every(frames as f64 / FPS as f64);
        let mut tile_frames = tile_frames(frames, every);
        tile_frames.reverse();
        Reviewer {
            width,
            height,
            every,
            tile_frames,
            prev: Vec::new(),
            before: Vec::new(),
            seen: 0,
            activity: Vec::new(),
            blank: Vec::new(),
            tiles: Vec::new(),
        }
    }

    /// Takes the next RGBA frame. `same` says the caller knows it is the previous frame again.
    pub fn observe(&mut self, frame: &[u8], same: bool) {
        let f = self.seen;
        self.seen += 1;
        let change = if f == 0 {
            self.prev = frame.to_vec();
            0.0
        } else if same {
            0.0
        } else {
            self.change(frame) as f64 / (self.width as f64 * self.height as f64)
        };
        let second = (f / FPS as u64) as usize;
        if self.activity.len() <= second {
            self.activity.resize(second + 1, 0.0);
        }
        self.activity[second] = self.activity[second].max(change);
        if f.is_multiple_of(blank_stride()) {
            self.blank.push(blank(frame, self.width, self.height));
        }
        if self.tile_frames.last() == Some(&f) {
            self.tile_frames.pop();
            self.tiles.push(self.tile(frame, f));
        }
    }

    /// Pixels that differ from both of the last two distinct pictures, so a cursor blinking
    /// between two pictures, as `rec wait --idle` also allows, is no change.
    fn change(&mut self, frame: &[u8]) -> u64 {
        let n = changed(&self.prev, frame, self.width);
        if n == 0 {
            self.prev.copy_from_slice(frame);
            return 0;
        }
        let n = if self.before.is_empty() {
            self.before = self.prev.clone();
            n
        } else {
            let n = n.min(changed(&self.before, frame, self.width));
            std::mem::swap(&mut self.before, &mut self.prev);
            n
        };
        self.prev.copy_from_slice(frame);
        n
    }

    fn tile(&self, frame: &[u8], f: u64) -> Tile {
        let scale = (TILE_SHORT / self.width.min(self.height) as f64).min(1.0);
        let w = ((self.width as f64 * scale).round() as u32).max(1);
        let h = ((self.height as f64 * scale).round() as u32).max(1);
        Tile {
            t: f as f64 / FPS as f64,
            w,
            h,
            rgba: shrink(frame, (self.width, self.height), (w, h)),
        }
    }

    /// The finished review, with its contact sheet written to `sheet`.
    pub fn finish(mut self, sheet: PathBuf, fonts: &Fonts, advice: Advice) -> Result<Review> {
        if self.seen == 0 {
            return Err(RecError::new("bad_video", "no frames to review"));
        }
        let last = self.seen - 1;
        if self.tiles.last().map(|t| t.t) != Some(last as f64 / FPS as f64) {
            let tile = self.tile(&self.prev, last);
            self.tiles.push(tile);
        }
        let duration = self.seen as f64 / FPS as f64;
        let activity: Vec<f64> = self
            .activity
            .iter()
            .map(|a| (a * 1e4).round() / 1e4)
            .collect();
        let idle = idle_spans(&activity, duration);
        let final_blank = blank(&self.prev, self.width, self.height);
        let checks = checks(&activity, &idle, &self.blank, final_blank, duration, advice);
        write_sheet(&sheet, &self.tiles, fonts)?;
        Ok(Review {
            sheet,
            every: self.every,
            activity,
            idle,
            checks,
        })
    }
}

fn blank_stride() -> u64 {
    (BLANK_EVERY * FPS as f64).round() as u64
}

/// Seconds between tiles: a second, or more for a long video so the sheet keeps about
/// `TILES` tiles, rounded up to a half second.
fn every(duration: f64) -> f64 {
    ((duration / TILES).max(1.0) * 2.0).ceil() / 2.0
}

/// The frames tiled at 0, `every`, 2 `every`, ..., and the last frame.
fn tile_frames(frames: u64, every: f64) -> Vec<u64> {
    let last = frames.max(1) - 1;
    let mut out: Vec<u64> = (0..)
        .map(|k| (k as f64 * every * FPS as f64).round() as u64)
        .take_while(|&f| f < last)
        .collect();
    out.push(last);
    out
}

fn differs(a: &[u8; 4], b: &[u8; 4]) -> bool {
    (0..3).any(|i| a[i].abs_diff(b[i]) > NOISE)
}

/// How many pixels of two RGBA frames differ by more than encoder noise.
fn changed(a: &[u8], b: &[u8], width: u32) -> u64 {
    let stride = width as usize * 4;
    a.chunks_exact(stride)
        .zip(b.chunks_exact(stride))
        .filter(|(ra, rb)| ra != rb)
        .map(|(ra, rb)| {
            let (pa, pb) = (ra.as_chunks::<4>().0, rb.as_chunks::<4>().0);
            pa.iter().zip(pb).filter(|(p, q)| differs(p, q)).count() as u64
        })
        .sum()
}

/// A frame is blank when its picture is one flat color: every pixel is the corner's color
/// (the canvas) or the center's (the panel, which is always centered).
fn blank(frame: &[u8], width: u32, height: u32) -> bool {
    let px = frame.as_chunks::<4>().0;
    let corner = px[0];
    let center = px[(height / 2 * width + width / 2) as usize];
    px.iter()
        .all(|p| !differs(p, &corner) || !differs(p, &center))
}

/// Box-filters an RGBA frame of `from` down to `to`.
fn shrink(frame: &[u8], from: (u32, u32), to: (u32, u32)) -> Vec<u8> {
    let span = |i: u32, n: u32, src: u32| {
        let lo = (i as u64 * src as u64 / n as u64) as u32;
        let hi = (((i + 1) as u64 * src as u64 / n as u64) as u32).max(lo + 1);
        lo..hi.min(src)
    };
    let mut out = Vec::with_capacity((to.0 * to.1 * 4) as usize);
    for ty in 0..to.1 {
        let ys = span(ty, to.1, from.1);
        for tx in 0..to.0 {
            let xs = span(tx, to.0, from.0);
            let mut sum = [0u64; 3];
            for y in ys.clone() {
                let row = (y * from.0) as usize * 4;
                for x in xs.clone() {
                    let i = row + x as usize * 4;
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += frame[i + c] as u64;
                    }
                }
            }
            let n = (ys.len() * xs.len()) as u64;
            out.extend(sum.map(|s| ((s + n / 2) / n) as u8));
            out.push(255);
        }
    }
    out
}

fn idle_spans(activity: &[f64], duration: f64) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut start = None;
    for (s, &a) in activity.iter().chain([&f64::INFINITY]).enumerate() {
        match (a < IDLE_CHANGE, start) {
            (true, None) => start = Some(s),
            (false, Some(from)) => {
                if s - from >= IDLE_RUN {
                    spans.push(Span {
                        from: from as f64,
                        to: round_t((s as f64).min(duration)),
                    });
                }
                start = None;
            }
            _ => {}
        }
    }
    spans
}

fn checks(
    activity: &[f64],
    idle: &[Span],
    blank: &[bool],
    final_blank: bool,
    duration: f64,
    advice: Advice,
) -> Vec<Check> {
    let mut out = Vec::new();
    let later = activity.get(STATIC_AFTER..).unwrap_or_default();
    let frozen = !later.is_empty() && later.iter().all(|&a| a < IDLE_CHANGE);
    if frozen {
        out.push(Finding::Frozen.check(
            "nothing changes after the first second: the program drew once and froze, or never ran"
                .into(),
            Some(Span {
                from: STATIC_AFTER as f64,
                to: round_t(duration),
            }),
        ));
    }
    out.extend(blank_checks(blank, final_blank, duration));
    if !frozen {
        for span in idle.iter().filter(|s| s.to - s.from >= LONG_IDLE) {
            let advice = match advice {
                Advice::Tighten => "re-export with --tighten to cut dead air",
                Advice::Tightened => {
                    "a tightened export should not hold a screen this long; check the plan"
                }
                Advice::Cut => "cut the wait, or drive the take without it",
            };
            out.push(Finding::LongIdle.check(
                format!(
                    "nothing moves from {}s to {}s ({:.1}s); {advice}",
                    span.from,
                    span.to,
                    span.to - span.from
                ),
                Some(*span),
            ));
        }
    }
    out
}

fn blank_checks(samples: &[bool], final_blank: bool, duration: f64) -> Vec<Check> {
    const DREW_NOTHING: &str = "one flat color, so the app drew nothing";
    let runs = blank_runs(samples, duration);
    if final_blank {
        return vec![Finding::BlankEnd.check(
            format!("the last frame is blank: {DREW_NOTHING}"),
            runs.last().copied(),
        )];
    }
    let blank = samples.iter().filter(|&&b| b).count();
    if blank as f64 > samples.len() as f64 * BLANK_MOST {
        let pct = (100 * blank / samples.len()) as u32;
        return vec![Finding::BlankMost.check(
            format!("{pct}% of the video is blank: {DREW_NOTHING}"),
            None,
        )];
    }
    runs.into_iter()
        .filter(|r| r.to - r.from >= BLANK_RUN)
        .map(|r| {
            Finding::BlankRun.check(
                format!("blank from {}s to {}s: {DREW_NOTHING}", r.from, r.to),
                Some(r),
            )
        })
        .collect()
}

/// Spans of consecutive blank samples, each lasting until the next sample.
fn blank_runs(samples: &[bool], duration: f64) -> Vec<Span> {
    let at = |i: usize| round_t((i as f64 * BLANK_EVERY).min(duration));
    let mut runs = Vec::new();
    let mut start = None;
    for (i, &b) in samples.iter().chain([&false]).enumerate() {
        match (b, start) {
            (true, None) => start = Some(i),
            (false, Some(from)) => {
                runs.push(Span {
                    from: at(from),
                    to: at(i),
                });
                start = None;
            }
            _ => {}
        }
    }
    runs
}

/// Lays the tiles out in rows, each with its time under it, and writes a PNG.
fn write_sheet(path: &Path, tiles: &[Tile], fonts: &Fonts) -> Result<()> {
    let (tw, th) = (tiles[0].w, tiles[0].h);
    let per_row = if th > tw { 6 } else { 4 }.min(tiles.len() as u32);
    let rows = (tiles.len() as u32).div_ceil(per_row);
    let size = (
        per_row * tw + (per_row + 1) * SHEET_GAP,
        rows * (th + LABEL_H) + (rows + 1) * SHEET_GAP,
    );
    let mut sheet = Vec::with_capacity((size.0 * size.1 * 4) as usize);
    for _ in 0..size.0 * size.1 {
        sheet.extend(SHEET_BG.into_iter().chain([255]));
    }
    for (i, tile) in tiles.iter().enumerate() {
        let (col, row) = (i as u32 % per_row, i as u32 / per_row);
        let x = SHEET_GAP + col * (tw + SHEET_GAP);
        let y = SHEET_GAP + row * (th + LABEL_H + SHEET_GAP);
        let stride = tile.w as usize * 4;
        for (ty, line) in tile.rgba.chunks_exact(stride).enumerate() {
            let at = (((y as usize + ty) * size.0 as usize) + x as usize) * 4;
            sheet[at..at + stride].copy_from_slice(line);
        }
        let label = format!("{:.1}s", tile.t);
        let top = (y + th) as i32 + (LABEL_H as f32 - LABEL_PX * 1.3).max(0.0) as i32 / 2;
        fonts.text(
            &label,
            LABEL_PX,
            LABEL_FG,
            &mut sheet,
            size,
            (x as i32 + 4, top),
        );
    }
    write_png(path, &sheet, size)
}

pub fn write_png(path: &Path, rgba: &[u8], size: (u32, u32)) -> Result<()> {
    let file =
        std::fs::File::create(path).map_err(|e| RecError::io(&path.display().to_string(), e))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), size.0, size.1);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let err = |e: png::EncodingError| RecError::new("io", format!("{}: {e}", path.display()));
    enc.write_header()
        .map_err(err)?
        .write_image_data(rgba)
        .map_err(err)
}

/// A frame `rec review --at` wrote at full size.
#[derive(Debug, Serialize)]
pub struct Still {
    pub t: f64,
    pub png: PathBuf,
}

/// `rec review FILE.mp4`: the review of an existing video, plus any stills asked for.
#[derive(Debug, Serialize)]
pub struct FileReview {
    pub path: PathBuf,
    #[serde(flatten)]
    pub review: Review,
    pub frames: Vec<Still>,
}

/// Decodes `path` at `FPS` and reviews it as export would, writing `<name>.sheet.png` and,
/// for each of `at`, `<name>.at-<t>.png` next to it.
pub fn review_file(path: &Path, at: &[f64]) -> Result<FileReview> {
    let ffmpeg = &super::find_ffmpeg()?;
    let probe = probe(ffmpeg, path)?;
    let mut reviewer = Reviewer::new(
        probe.width,
        probe.height,
        (probe.duration * FPS as f64).round() as u64,
    );
    let mut child = Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vf", &format!("fps={FPS}")])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| RecError::new("missing_dependency", format!("cannot run ffmpeg: {e}")))?;
    let mut out = child.stdout.take().expect("piped");
    let mut frame = vec![0u8; (probe.width * probe.height * 4) as usize];
    let wanted: Vec<u64> = at
        .iter()
        .map(|t| (t.max(0.0) * FPS as f64).round() as u64)
        .collect();
    let still_path = |t: f64| sibling(path, &format!("at-{t}.png"));
    let mut stills: Vec<Option<Still>> = at.iter().map(|_| None).collect();
    let mut f = 0u64;
    while out.read_exact(&mut frame).is_ok() {
        for (i, _) in wanted.iter().enumerate().filter(|(_, &w)| w == f) {
            let png = still_path(at[i]);
            write_png(&png, &frame, (probe.width, probe.height))?;
            stills[i] = Some(Still {
                t: round_t(f as f64 / FPS as f64),
                png,
            });
        }
        reviewer.observe(&frame, false);
        f += 1;
    }
    let _ = child.wait();
    if f == 0 {
        return Err(RecError::new(
            "bad_video",
            format!("ffmpeg decoded no frames from {}", path.display()),
        ));
    }
    let last = &reviewer.prev;
    let mut frames = Vec::new();
    for (i, still) in stills.into_iter().enumerate() {
        let still = match still {
            Some(s) => s,
            None => {
                let png = still_path(at[i]);
                write_png(&png, last, (probe.width, probe.height))?;
                Still {
                    t: round_t((f - 1) as f64 / FPS as f64),
                    png,
                }
            }
        };
        frames.push(still);
    }
    let review = reviewer.finish(sibling(path, "sheet.png"), &Fonts::load(), Advice::Cut)?;
    Ok(FileReview {
        path: path.to_path_buf(),
        review,
        frames,
    })
}

/// `export-9x16.mp4` with `sheet.png` is `export-9x16.sheet.png`.
pub fn sibling(video: &Path, suffix: &str) -> PathBuf {
    video.with_extension(suffix)
}

struct Probe {
    width: u32,
    height: u32,
    duration: f64,
}

/// The size and length ffmpeg prints for the first video stream.
fn probe(ffmpeg: &Path, path: &Path) -> Result<Probe> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| RecError::new("missing_dependency", format!("cannot run ffmpeg: {e}")))?;
    parse_probe(&String::from_utf8_lossy(&out.stderr)).ok_or_else(|| {
        RecError::new(
            "bad_video",
            format!("{} has no video stream ffmpeg can read", path.display()),
        )
    })
}

fn parse_probe(info: &str) -> Option<Probe> {
    let size = regex::Regex::new(r"Stream #.*Video: .*?\b(\d{2,5})x(\d{2,5})\b").expect("regex");
    let length = regex::Regex::new(r"Duration: (\d+):(\d\d):(\d\d(?:\.\d+)?)").expect("regex");
    let s = size.captures(info)?;
    let d = length.captures(info)?;
    let n = |i: usize| d[i].parse::<f64>().ok();
    Some(Probe {
        width: s[1].parse().ok()?,
        height: s[2].parse().ok()?,
        duration: n(1)? * 3600.0 + n(2)? * 60.0 + n(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 16;
    const H: u32 = 16;

    /// A gray frame with a black dot near the corner and a white square at `pos`, so it is
    /// never one flat color.
    fn frame(pos: u32) -> Vec<u8> {
        let mut f: Vec<u8> = (0..W * H).flat_map(|_| [128, 128, 128, 255]).collect();
        let mut set = |x: u32, y: u32, c: u8| {
            let i = ((y * W + x) * 4) as usize;
            f[i..i + 3].copy_from_slice(&[c, c, c]);
        };
        set(1, 1, 0);
        for y in 0..4 {
            for x in 0..4 {
                set((pos + x) % W, 8 + y, 255);
            }
        }
        f
    }

    fn uniform(c: u8) -> Vec<u8> {
        (0..W * H).flat_map(|_| [c, c, c, 255]).collect()
    }

    fn review(frames: &[Vec<u8>], advice: Advice, name: &str) -> Review {
        let mut r = Reviewer::new(W, H, frames.len() as u64);
        for f in frames {
            r.observe(f, false);
        }
        let sheet = std::env::temp_dir().join(format!(
            "rec-review-{name}-{}.sheet.png",
            std::process::id()
        ));
        r.finish(sheet, &Fonts::load(), advice).unwrap()
    }

    fn codes(r: &Review) -> Vec<(CheckCode, Severity)> {
        r.checks.iter().map(|c| (c.code, c.severity)).collect()
    }

    fn seconds(n: u32, mut f: impl FnMut(u32) -> Vec<u8>) -> Vec<Vec<u8>> {
        (0..n * FPS).map(&mut f).collect()
    }

    #[test]
    fn a_video_that_stops_changing_after_its_first_second_is_static() {
        let frames = seconds(4, |i| frame(i.min(FPS - 1)));
        let r = review(&frames, Advice::Tighten, "static");
        assert_eq!(codes(&r), [(CheckCode::Static, Severity::Error)]);
        assert_eq!(r.activity[1..], [0.0, 0.0, 0.0]);
    }

    #[test]
    fn uniform_frames_are_blank() {
        let frames = seconds(4, |i| {
            if i % 2 == 0 {
                uniform(20)
            } else {
                uniform(200)
            }
        });
        let r = review(&frames, Advice::Cut, "blank");
        assert!(
            codes(&r).contains(&(CheckCode::Blank, Severity::Error)),
            "{:?}",
            r.checks
        );
    }

    #[test]
    fn a_short_blank_run_is_only_a_warning() {
        let frames = seconds(6, |i| match i / FPS {
            1 | 2 => uniform(0),
            _ => frame(i),
        });
        let r = review(&frames, Advice::Cut, "blank-run");
        assert_eq!(codes(&r), [(CheckCode::Blank, Severity::Warning)]);
        assert_eq!(r.checks[0].span, Some(Span { from: 1.0, to: 3.0 }));
    }

    #[test]
    fn a_long_still_stretch_is_idle_and_warns() {
        let frames = seconds(12, |i| match i / FPS {
            2..10 => frame(0),
            _ => frame(i),
        });
        let r = review(&frames, Advice::Tighten, "idle");
        // Second 2 opens on the change into the still picture, so it is not idle.
        assert_eq!(
            r.idle,
            [Span {
                from: 3.0,
                to: 10.0
            }]
        );
        assert_eq!(codes(&r), [(CheckCode::LongIdle, Severity::Warning)]);
        assert!(
            r.checks[0].message.contains("--tighten"),
            "{}",
            r.checks[0].message
        );
    }

    #[test]
    fn a_short_still_stretch_is_idle_without_a_warning() {
        let frames = seconds(8, |i| match i / FPS {
            2..6 => frame(0),
            _ => frame(i),
        });
        let r = review(&frames, Advice::Tighten, "short-idle");
        assert_eq!(r.idle, [Span { from: 3.0, to: 6.0 }]);
        assert!(r.checks.is_empty(), "{:?}", r.checks);
    }

    #[test]
    fn a_blinking_cursor_on_a_still_screen_is_no_change() {
        let frames = seconds(5, |i| frame(if i / 15 % 2 == 0 { 0 } else { 6 }));
        let r = review(&frames, Advice::Cut, "blink");
        // Only the first blink is new; after it the two pictures just alternate.
        assert_eq!(r.activity[1..], [0.0; 4]);
        assert_eq!(codes(&r), [(CheckCode::Static, Severity::Error)]);
    }

    #[test]
    fn a_video_that_keeps_changing_passes() {
        let frames = seconds(5, frame);
        let r = review(&frames, Advice::Tighten, "moving");
        assert!(r.checks.is_empty(), "{:?}", r.checks);
        assert!(r.idle.is_empty());
        assert_eq!(r.activity.len(), 5);
    }

    #[test]
    fn activity_is_the_fraction_of_pixels_that_changed() {
        let a = frame(0);
        let mut b = a.clone();
        for p in [0usize, 5, 200] {
            b[p * 4] = b[p * 4].wrapping_add(100);
        }
        let mut faint = b.clone();
        faint[100 * 4] = faint[100 * 4].wrapping_add(NOISE);
        let r = review(&[a, b, faint], Advice::Cut, "activity");
        assert_eq!(r.activity, [(3.0 / 256.0 * 1e4_f64).round() / 1e4]);
    }

    #[test]
    fn tiles_fall_every_second_or_half_second_and_on_the_last_frame() {
        assert_eq!(every(10.0), 1.0);
        assert_eq!(every(60.0), 2.5);
        assert_eq!(every(100.0), 4.5);
        let frames = 10 * FPS as u64;
        let tiles = tile_frames(frames, every(10.0));
        let times: Vec<f64> = tiles.iter().map(|&f| f as f64 / FPS as f64).collect();
        assert_eq!(
            times[..10],
            [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]
        );
        assert_eq!(tiles.last(), Some(&(frames - 1)));
        assert_eq!(tiles.len(), 11);
        assert!(tile_frames(100 * FPS as u64, every(100.0)).len() <= 25);
    }

    #[test]
    fn the_sheet_is_a_png_with_one_tile_per_timestamp() {
        let frames = seconds(3, frame);
        let r = review(&frames, Advice::Cut, "sheet");
        let decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&r.sheet).unwrap(),
        ));
        let info = decoder.read_info().unwrap().info().clone();
        // Four 16x16 tiles (0s, 1s, 2s, and the last frame) in one row: a square video gets the
        // landscape sheet's four columns.
        assert_eq!(info.width, 4 * W + 5 * SHEET_GAP);
        assert_eq!(info.height, H + LABEL_H + 2 * SHEET_GAP);
    }

    #[test]
    fn ffmpeg_stream_info_gives_size_and_length() {
        let info = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'export-9x16.mp4':\n  Duration: 00:01:02.50, start: 0.000000, bitrate: 157 kb/s\n  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, progressive), 1080x1920 [SAR 1:1 DAR 9:16], 151 kb/s, 30 fps\n";
        let p = parse_probe(info).unwrap();
        assert_eq!((p.width, p.height, p.duration), (1080, 1920, 62.5));
    }
}
