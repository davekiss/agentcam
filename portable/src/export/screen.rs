//! Frames for a screen recording: screen.mp4 decoded by ffmpeg to RGBA and scaled into the
//! layout. A cropped 9:16 window pans with the action, using the same camera as a terminal:
//! the pointer is the cursor, and what changes on screen right after an input is the ink.

use super::camera::{Camera, Focus};
use super::layout::{Preset, Rect, ScreenViewport};
use super::render;
use super::theme::Rgb;
use super::{Frames, FPS};
use crate::error::{RecError, Result};
use crate::model::{self, Event, Take, Track};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

/// A finished screen take, read for export.
pub struct ScreenTake {
    pub path: PathBuf,
    /// When screen.mp4's first frame was captured, on the take clock.
    pub offset: f64,
    pub width: u32,
    pub height: u32,
    /// Where the pointer was, as `(t, x)` with `x` normalized, from cursor and click events.
    pointer: Vec<(f64, f64)>,
    /// Take-time spans just after each `type` and `key`, when what changes on screen is the
    /// input's echo. Outside them a change, such as a blinking cursor, is not followed. Within
    /// one, the ink is everything changed since it began, so a title bar redrawn a frame after
    /// the prompt widens the ink instead of pulling the window away from the prompt.
    echoes: Vec<(f64, f64)>,
}

/// How long after an input the screen's changes count as its echo, plus per typed character.
const ECHO: f64 = 0.5;
const ECHO_PER_CHAR: f64 = 0.05;

impl ScreenTake {
    pub fn read(dir: &Path, take: &Take) -> Result<ScreenTake> {
        let (file, offset, width, height) = take
            .tracks
            .iter()
            .find_map(|t| match t {
                Track::Screen {
                    file,
                    offset,
                    width,
                    height,
                } => Some((file, *offset, *width, *height)),
                _ => None,
            })
            .ok_or_else(|| RecError::new("bad_take", format!("{} has no screen track", take.id)))?;
        let events = model::Timeline::read(dir)?.events;
        let pointer = events
            .iter()
            .filter_map(|e| match e.event {
                Event::Cursor { x, .. } | Event::Click { x, .. } => Some((e.t, x)),
                _ => None,
            })
            .collect();
        let echoes = events
            .iter()
            .filter_map(|e| match &e.event {
                Event::Type { text, .. } => {
                    let chars = text.as_deref().map_or(10, |t| t.chars().count());
                    Some((e.t, e.t + ECHO + ECHO_PER_CHAR * chars as f64))
                }
                Event::Key { .. } => Some((e.t, e.t + ECHO)),
                _ => None,
            })
            .collect();
        Ok(ScreenTake {
            path: dir.join(file),
            offset,
            width,
            height,
            pointer,
            echoes,
        })
    }

    /// Export opens at the screen track's first frame.
    pub fn frames(&self, duration: f64) -> u64 {
        (((duration - self.offset) * FPS as f64).round() as u64).max(1)
    }
}

/// screen.mp4 read one RGBA frame at a time.
struct Decoder {
    child: Child,
    out: ChildStdout,
    frame: Vec<u8>,
    next: Vec<u8>,
    /// Frames read so far; once the stream ends, the last frame repeats.
    read: u64,
    ended: bool,
}

impl Decoder {
    fn spawn(ffmpeg: &Path, take: &ScreenTake) -> Result<Decoder> {
        let mut child = Command::new(ffmpeg)
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(&take.path)
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| RecError::new("missing_dependency", format!("cannot run ffmpeg: {e}")))?;
        let len = (take.width * take.height * 4) as usize;
        Ok(Decoder {
            out: child.stdout.take().expect("piped"),
            child,
            frame: vec![0; len],
            next: vec![0; len],
            read: 0,
            ended: false,
        })
    }

    /// Advances to frame `k` and returns the columns that changed, if any did.
    fn seek(&mut self, k: u64, width: u32) -> Option<(u32, u32)> {
        let mut changed: Option<(u32, u32)> = None;
        while self.read <= k && !self.ended {
            if self.out.read_exact(&mut self.next).is_err() {
                self.ended = true;
                break;
            }
            self.read += 1;
            let cols = if self.read == 1 {
                Some((0, width - 1))
            } else {
                changed_columns(&self.frame, &self.next, width)
            };
            if let Some((lo, hi)) = cols {
                std::mem::swap(&mut self.frame, &mut self.next);
                changed = Some(changed.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
            }
        }
        changed
    }
}

type EchoState = Option<(usize, Option<(u32, u32)>)>;

/// The columns changed since echo span `span` began, on frames where something changed.
fn echo_ink(
    state: &mut EchoState,
    span: Option<usize>,
    change: Option<(u32, u32)>,
) -> Option<(u32, u32)> {
    let Some(span) = span else {
        *state = None;
        return None;
    };
    let union = match *state {
        Some((current, union)) if current == span => union,
        _ => None,
    };
    let union = match (union, change) {
        (Some((a, b)), Some((lo, hi))) => Some((a.min(lo), b.max(hi))),
        (union, change) => union.or(change),
    };
    *state = Some((span, union));
    change.and(union)
}

/// The leftmost and rightmost columns where two RGBA frames of `width` differ.
fn changed_columns(a: &[u8], b: &[u8], width: u32) -> Option<(u32, u32)> {
    let stride = width as usize * 4;
    let mut cols: Option<(u32, u32)> = None;
    for (ra, rb) in a.chunks_exact(stride).zip(b.chunks_exact(stride)) {
        if ra == rb {
            continue;
        }
        let differs = |x: &usize| ra[x * 4..x * 4 + 4] != rb[x * 4..x * 4 + 4];
        let lo = (0..width as usize).find(differs).expect("rows differ") as u32;
        let hi = (0..width as usize).rfind(differs).expect("rows differ") as u32;
        cols = Some(cols.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
    }
    cols
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct ScreenFrames<'a> {
    take: &'a ScreenTake,
    decoder: Decoder,
    viewport: ScreenViewport,
    /// For `Follow`: the window's left edge on the scaled surface, in canvas pixels.
    camera: Option<Camera>,
    next_event: usize,
    /// The echo span now playing, and the columns changed since it began.
    echo: EchoState,
    canvas_w: u32,
    canvas_color: Rgb,
    shown: Option<f64>,
}

impl<'a> ScreenFrames<'a> {
    pub fn new(
        ffmpeg: &Path,
        take: &'a ScreenTake,
        preset: &Preset,
        viewport: ScreenViewport,
        canvas_color: Rgb,
    ) -> Result<ScreenFrames<'a>> {
        let camera = match viewport {
            ScreenViewport::Follow {
                panel, surface_w, ..
            } => Some(Camera::new(panel.w as f64, surface_w)),
            ScreenViewport::Fit { .. } => None,
        };
        Ok(ScreenFrames {
            take,
            decoder: Decoder::spawn(ffmpeg, take)?,
            viewport,
            camera,
            next_event: 0,
            echo: None,
            canvas_w: preset.width,
            canvas_color,
            shown: None,
        })
    }

    fn echo_ink(&mut self, t: f64, change: Option<(u32, u32)>) -> Option<(u32, u32)> {
        let span = self.take.echoes.iter().rposition(|&(a, b)| a <= t && t < b);
        echo_ink(&mut self.echo, span, change)
    }

    /// The pointer's latest position up to take time `t`, if it moved since the last call.
    fn pointer_since_last(&mut self, t: f64) -> Option<f64> {
        let mut x = None;
        while let Some(&(at, px)) = self.take.pointer.get(self.next_event) {
            if at > t {
                break;
            }
            x = Some(px);
            self.next_event += 1;
        }
        x
    }
}

impl Frames for ScreenFrames<'_> {
    fn draw(&mut self, f: u64, canvas: &mut [u8]) -> Result<bool> {
        let (w, h) = (self.take.width, self.take.height);
        let change = self.decoder.seek(f, w);
        let changed = change.is_some();
        let t = self.take.offset + f as f64 / FPS as f64;
        let moved = self.pointer_since_last(t);
        let ink = self.echo_ink(t, change);
        let (panel, left, step_x) = match self.viewport {
            ScreenViewport::Fit { panel } => (panel, 0.0, w as f64 / panel.w as f64),
            ScreenViewport::Follow {
                panel,
                scale,
                surface_w,
            } => {
                let cam = self.camera.as_mut().expect("follow has a camera");
                let focus = Focus {
                    ink: ink.map(|(lo, hi)| (lo as f64 * scale, (hi + 1) as f64 * scale)),
                    cursor: moved.map(|x| (x * surface_w, x * surface_w)),
                };
                if self.shown.is_none() {
                    // With no pointer event yet, start on the middle of the screen.
                    let mid = surface_w / 2.0;
                    cam.place(Focus {
                        cursor: focus.cursor.or(Some((mid, mid))),
                        ..focus
                    });
                } else {
                    cam.step(focus);
                }
                (panel, cam.x().round() / scale, 1.0 / scale)
            }
        };
        if !changed && self.shown == Some(left) {
            return Ok(false);
        }
        if self.shown.is_none() {
            let height = canvas.len() as u32 / 4 / self.canvas_w;
            render::fill(
                canvas,
                self.canvas_w,
                render::full(self.canvas_w, height),
                self.canvas_color,
            );
        }
        let src = Source {
            rgba: &self.decoder.frame,
            w,
            h,
        };
        scale_into(&src, left, step_x, canvas, self.canvas_w, panel);
        self.shown = Some(left);
        Ok(true)
    }
}

struct Source<'a> {
    rgba: &'a [u8],
    w: u32,
    h: u32,
}

/// For each output pixel along one axis: the two source pixels it blends and the second's
/// weight out of 256.
fn taps(start: f64, step: f64, n: u32, limit: u32) -> Vec<(usize, usize, u32)> {
    (0..n)
        .map(|i| {
            let at = (start + (i as f64 + 0.5) * step - 0.5).clamp(0.0, (limit - 1) as f64);
            let lo = at.floor();
            let hi = (lo as u32 + 1).min(limit - 1);
            (lo as usize, hi as usize, ((at - lo) * 256.0).round() as u32)
        })
        .collect()
}

/// Bilinear: fills `panel` of the canvas from the source, starting at source column `left` and
/// advancing `step_x` source pixels per canvas pixel; rows span the source's full height.
fn scale_into(src: &Source, left: f64, step_x: f64, canvas: &mut [u8], canvas_w: u32, panel: Rect) {
    let cols = taps(left, step_x, panel.w, src.w);
    let rows = taps(0.0, src.h as f64 / panel.h as f64, panel.h, src.h);
    let stride = src.w as usize * 4;
    for (y, &(y0, y1, wy)) in rows.iter().enumerate() {
        let (r0, r1) = (&src.rgba[y0 * stride..], &src.rgba[y1 * stride..]);
        let dst = ((panel.y as usize + y) * canvas_w as usize + panel.x as usize) * 4;
        let out = &mut canvas[dst..dst + panel.w as usize * 4];
        for (px, &(x0, x1, wx)) in out.as_chunks_mut::<4>().0.iter_mut().zip(&cols) {
            for c in 0..3 {
                let top = r0[x0 * 4 + c] as u32 * (256 - wx) + r0[x1 * 4 + c] as u32 * wx;
                let bottom = r1[x0 * 4 + c] as u32 * (256 - wx) + r1[x1 * 4 + c] as u32 * wx;
                px[c] = ((top * (256 - wy) + bottom * wy + (1 << 15)) >> 16) as u8;
            }
            px[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Vec<u8> {
        (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [x as u8, y as u8, 7, 255]))
            .collect()
    }

    #[test]
    fn ink_grows_within_an_echo_and_ignores_changes_outside_one() {
        let mut state = None;
        assert_eq!(
            echo_ink(&mut state, None, Some((900, 980))),
            None,
            "a blink is not ink"
        );
        assert_eq!(
            echo_ink(&mut state, Some(0), Some((0, 240))),
            Some((0, 240)),
            "the prompt"
        );
        assert_eq!(
            echo_ink(&mut state, Some(0), None),
            None,
            "no change, no ink"
        );
        assert_eq!(
            echo_ink(&mut state, Some(0), Some((900, 980))),
            Some((0, 980)),
            "a title redrawn after the prompt widens the ink instead of replacing it"
        );
        assert_eq!(
            echo_ink(&mut state, Some(1), Some((300, 310))),
            Some((300, 310)),
            "a new input starts over"
        );
    }

    #[test]
    fn changed_columns_bound_every_differing_pixel() {
        let a = gradient(8, 4);
        assert_eq!(changed_columns(&a, &a, 8), None);
        let mut b = a.clone();
        b[(8 + 2) * 4] ^= 1;
        b[(3 * 8 + 6) * 4 + 1] ^= 1;
        assert_eq!(changed_columns(&a, &b, 8), Some((2, 6)));
    }

    #[test]
    fn unit_scale_copies_pixels_exactly() {
        let rgba = gradient(8, 4);
        let src = Source {
            rgba: &rgba,
            w: 8,
            h: 4,
        };
        let mut canvas = vec![0u8; 10 * 6 * 4];
        let panel = Rect {
            x: 1,
            y: 1,
            w: 8,
            h: 4,
        };
        scale_into(&src, 0.0, 1.0, &mut canvas, 10, panel);
        for y in 0..4 {
            for x in 0..8 {
                let i = (((y + 1) * 10 + x + 1) * 4) as usize;
                assert_eq!(&canvas[i..i + 4], &[x as u8, y as u8, 7, 255], "({x}, {y})");
            }
        }
        assert_eq!(
            &canvas[0..4],
            &[0, 0, 0, 0],
            "outside the panel is untouched"
        );
    }

    #[test]
    fn a_window_shows_the_source_columns_from_its_left_edge() {
        let rgba = gradient(8, 4);
        let src = Source {
            rgba: &rgba,
            w: 8,
            h: 4,
        };
        let mut canvas = vec![0u8; 4 * 4 * 4];
        scale_into(
            &src,
            3.0,
            1.0,
            &mut canvas,
            4,
            Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 4,
            },
        );
        let reds: Vec<u8> = canvas.chunks(4).take(4).map(|p| p[0]).collect();
        assert_eq!(reds, [3, 4, 5, 6]);
    }

    #[test]
    fn doubling_blends_between_neighbours() {
        let rgba = gradient(4, 2);
        let src = Source {
            rgba: &rgba,
            w: 4,
            h: 2,
        };
        let mut canvas = vec![0u8; 8 * 4 * 4];
        scale_into(
            &src,
            0.0,
            0.5,
            &mut canvas,
            8,
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 4,
            },
        );
        let reds: Vec<u8> = canvas.chunks(4).take(8).map(|p| p[0]).collect();
        assert_eq!(reds, [0, 0, 1, 1, 2, 2, 3, 3]);
    }
}
