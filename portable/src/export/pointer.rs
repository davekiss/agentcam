//! The pointer drawn over an x11 screen at export, from the timeline's cursor and click events.
//! What it looks like at take time t is a pure function of the timeline, so re-exports match:
//! where it is, how opaque it is, and the ripple of a recent click.

use super::layout::Rect;
use crate::model::{Event, TimedEvent};

/// `--cursor`: when the pointer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum CursorMode {
    /// Only while it moves and around clicks.
    #[default]
    Auto,
    Always,
    Never,
}

/// Seconds.
pub struct Timing {
    /// How long the pointer takes to appear once it starts moving.
    pub fade_in: f64,
    /// How long before a click the pointer starts to appear.
    pub lead: f64,
    /// How long it stays after the last move or click.
    pub hold: f64,
    /// How long it takes to disappear after the hold.
    pub fade_out: f64,
    /// A move from rest (a `rec move` or `rec click` jumps in one step) is drawn as a glide
    /// this long that arrives when the pointer did. Continuous motion is interpolated between
    /// samples instead.
    pub glide: f64,
    /// How long a click's ripple expands and fades.
    pub ripple: f64,
}

pub const TIMING: Timing = Timing {
    fade_in: 0.12,
    lead: 0.4,
    hold: 1.0,
    fade_out: 0.25,
    glide: 0.25,
    ripple: 0.4,
};

/// The arrow's height as a fraction of the canvas height: 30px on 16:9, 54px on 9:16.
const HEIGHT_FRAC: f64 = 0.028;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    t: f64,
    x: f64,
    y: f64,
}

/// The pointer's path and activity, from a take's timeline. Positions are normalized 0..1.
#[derive(Debug, Default)]
pub struct Pointer {
    /// Each position the pointer reached, at the time it got there, in time order.
    path: Vec<Sample>,
    clicks: Vec<Sample>,
    /// Disjoint spans when the pointer was moving or about to click, in time order.
    active: Vec<(f64, f64)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub x: f64,
    pub y: f64,
    pub opacity: f64,
    pub ripple: Option<Ripple>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ripple {
    pub x: f64,
    pub y: f64,
    /// 0 at the click, 1 when the ripple is gone.
    pub progress: f64,
}

fn smoothstep(u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

impl Pointer {
    pub fn new(events: &[TimedEvent]) -> Pointer {
        let mut path: Vec<Sample> = Vec::new();
        let mut clicks = Vec::new();
        for e in events {
            let (x, y) = match e.event {
                Event::Cursor { x, y } => (x, y),
                // A drag ripples at its press; the cursor samples carry the motion after it.
                Event::Click { x, y, .. } | Event::Drag { x1: x, y1: y, .. } => {
                    clicks.push(Sample { t: e.t, x, y });
                    (x, y)
                }
                _ => continue,
            };
            if path.last().is_none_or(|s| (s.x, s.y) != (x, y)) {
                path.push(Sample { t: e.t, x, y });
            }
        }
        let mut spans: Vec<(f64, f64)> = path
            .windows(2)
            .map(|w| (w[1].t - Self::glide(w[0], w[1]), w[1].t))
            .chain(clicks.iter().map(|c| (c.t - TIMING.lead, c.t)))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut active: Vec<(f64, f64)> = Vec::new();
        for (a, b) in spans {
            match active.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => active.push((a, b)),
            }
        }
        Pointer {
            path,
            clicks,
            active,
        }
    }

    /// How long the move into `to` is drawn over.
    fn glide(from: Sample, to: Sample) -> f64 {
        TIMING.glide.min(to.t - from.t)
    }

    fn position(&self, t: f64) -> Option<(f64, f64)> {
        let k = self.path.partition_point(|s| s.t <= t);
        let from = *self.path.get(k.checked_sub(1)?)?;
        let Some(&to) = self.path.get(k) else {
            return Some((from.x, from.y));
        };
        let g = Self::glide(from, to);
        let u = smoothstep((t - (to.t - g)) / g);
        Some((from.x + (to.x - from.x) * u, from.y + (to.y - from.y) * u))
    }

    /// Auto mode: ramps up over `fade_in` from the start of each active span, holds for `hold`
    /// after its end, then fades over `fade_out`.
    fn auto_opacity(&self, t: f64) -> f64 {
        let tail = TIMING.hold + TIMING.fade_out;
        let k = self.active.partition_point(|&(a, _)| a <= t);
        self.active[..k]
            .iter()
            .rev()
            .take_while(|&&(_, b)| b + tail >= t)
            .map(|&(a, b)| {
                let rise = (t - a) / TIMING.fade_in;
                let fall = 1.0 - (t - b - TIMING.hold) / TIMING.fade_out;
                rise.min(fall).clamp(0.0, 1.0)
            })
            .fold(0.0, f64::max)
    }

    fn ripple(&self, t: f64) -> Option<Ripple> {
        let k = self.clicks.partition_point(|c| c.t <= t);
        let c = self.clicks.get(k.checked_sub(1)?)?;
        let progress = (t - c.t) / TIMING.ripple;
        (progress < 1.0).then_some(Ripple {
            x: c.x,
            y: c.y,
            progress,
        })
    }

    /// What to draw at take time `t`, or nothing.
    pub fn look(&self, t: f64, mode: CursorMode) -> Option<Look> {
        let (opacity, ripple) = match mode {
            CursorMode::Never => return None,
            CursorMode::Always => (1.0, self.ripple(t)),
            CursorMode::Auto => (self.auto_opacity(t), self.ripple(t)),
        };
        let (x, y) = self.position(t)?;
        (opacity > 0.0 || ripple.is_some()).then_some(Look {
            x,
            y,
            opacity,
            ripple,
        })
    }
}

/// The arrow, tip at the origin, in units where it is 19.4 tall.
const ARROW: [(f64, f64); 7] = [
    (0.0, 0.0),
    (0.0, 17.0),
    (4.2, 13.2),
    (7.0, 19.4),
    (9.6, 18.2),
    (6.9, 12.2),
    (12.4, 12.2),
];
const ARROW_H: f64 = 19.4;
const ARROW_W: f64 = 12.4;
/// The dark outline around the white fill, and the drop shadow's offset and blur, in units.
const OUTLINE: f64 = 1.3;
const SHADOW: (f64, f64) = (0.5, 1.2);
const BLUR: f64 = 1.6;
const FILL: [f32; 3] = [255.0, 255.0, 255.0];
const EDGE: [f32; 3] = [17.0, 17.0, 20.0];

/// Signed distance from `p` to the arrow, negative inside.
fn arrow_sdf(px: f64, py: f64) -> f64 {
    let mut d = f64::INFINITY;
    let mut inside = false;
    for i in 0..ARROW.len() {
        let (ax, ay) = ARROW[i];
        let (bx, by) = ARROW[(i + 1) % ARROW.len()];
        let (ex, ey) = (bx - ax, by - ay);
        let (wx, wy) = (px - ax, py - ay);
        let h = ((wx * ex + wy * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
        let (dx, dy) = (wx - ex * h, wy - ey * h);
        d = d.min(dx * dx + dy * dy);
        if (ay > py) != (by > py) && px < ax + (py - ay) * ex / ey {
            inside = !inside;
        }
    }
    if inside {
        -d.sqrt()
    } else {
        d.sqrt()
    }
}

/// How much of a pixel a shape covers, from its signed distance in pixels.
fn coverage(d: f64) -> f32 {
    (0.5 - d).clamp(0.0, 1.0) as f32
}

fn blend(px: &mut [u8], color: [f32; 3], alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    for c in 0..3 {
        let v = px[c] as f32;
        px[c] = (v + (color[c] - v) * alpha).round() as u8;
    }
}

/// Where the pointer goes on the canvas: canvas pixel = `origin + source pixel * scale`.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub origin: (f64, f64),
    pub scale: (f64, f64),
    /// The source's size in pixels, to turn normalized positions into source pixels.
    pub source: (u32, u32),
    /// Nothing is drawn outside this rect.
    pub clip: Rect,
}

impl Placement {
    /// The center of the pointer's pixel, on the canvas.
    fn at(&self, x: f64, y: f64) -> (f64, f64) {
        let sx = x * self.source.0 as f64 + 0.5;
        let sy = y * self.source.1 as f64 + 0.5;
        (
            self.origin.0 + sx * self.scale.0,
            self.origin.1 + sy * self.scale.1,
        )
    }
}

/// The canvas pixels `x0..x1, y0..y1` inside `clip`.
fn clipped(clip: Rect, x0: f64, y0: f64, x1: f64, y1: f64) -> Option<(u32, u32, u32, u32)> {
    let lo_x = (x0.floor().max(clip.x as f64)) as u32;
    let lo_y = (y0.floor().max(clip.y as f64)) as u32;
    let hi_x = (x1.ceil().min((clip.x + clip.w) as f64)) as u32;
    let hi_y = (y1.ceil().min((clip.y + clip.h) as f64)) as u32;
    (lo_x < hi_x && lo_y < hi_y).then_some((lo_x, lo_y, hi_x, hi_y))
}

/// Draws `look` into an RGBA canvas `canvas_w` wide, sized for a canvas `canvas_h` tall.
pub fn draw(look: &Look, at: &Placement, canvas: &mut [u8], canvas_w: u32, canvas_h: u32) {
    let height = HEIGHT_FRAC * canvas_h as f64;
    if let Some(r) = look.ripple {
        draw_ripple(&r, at.at(r.x, r.y), height, at.clip, canvas, canvas_w);
    }
    if look.opacity > 0.0 {
        let tip = at.at(look.x, look.y);
        draw_arrow(tip, height, look.opacity as f32, at.clip, canvas, canvas_w);
    }
}

/// The canvas box the arrow can touch with its tip at `tip`, before clipping.
fn arrow_bounds(tip: (f64, f64), height: f64) -> (f64, f64, f64, f64) {
    let unit = height / ARROW_H;
    let pad = (OUTLINE + BLUR + 1.0) * unit;
    (
        tip.0 - pad,
        tip.1 - pad,
        tip.0 + ARROW_W * unit + pad + SHADOW.0 * unit,
        tip.1 + height + pad + SHADOW.1 * unit,
    )
}

fn draw_arrow(
    tip: (f64, f64),
    height: f64,
    opacity: f32,
    clip: Rect,
    canvas: &mut [u8],
    canvas_w: u32,
) {
    let unit = height / ARROW_H;
    let (x0, y0, x1, y1) = arrow_bounds(tip, height);
    let Some((lo_x, lo_y, hi_x, hi_y)) = clipped(clip, x0, y0, x1, y1) else {
        return;
    };
    for y in lo_y..hi_y {
        for x in lo_x..hi_x {
            let ux = (x as f64 + 0.5 - tip.0) / unit;
            let uy = (y as f64 + 0.5 - tip.1) / unit;
            let d = arrow_sdf(ux, uy) * unit;
            let ds = arrow_sdf(ux - SHADOW.0, uy - SHADOW.1);
            let shadow = 0.35 * smoothstep(1.0 - (ds + BLUR / 2.0) / BLUR) as f32;
            let i = ((y * canvas_w + x) * 4) as usize;
            let px = &mut canvas[i..i + 4];
            blend(px, [0.0; 3], shadow * opacity);
            blend(px, EDGE, coverage(d - OUTLINE * unit) * opacity);
            blend(px, FILL, coverage(d) * opacity);
        }
    }
}

/// A ring that expands from the click point and fades: white with a dark edge, so it reads on
/// light and dark screens.
fn draw_ripple(
    r: &Ripple,
    center: (f64, f64),
    height: f64,
    clip: Rect,
    canvas: &mut [u8],
    canvas_w: u32,
) {
    let eased = 1.0 - (1.0 - r.progress).powi(3);
    let radius = height * (0.3 + 0.9 * eased);
    let stroke = height * 0.09;
    let alpha = (1.0 - r.progress).powf(1.5) as f32;
    let reach = radius + stroke + 2.0;
    let Some((lo_x, lo_y, hi_x, hi_y)) = clipped(
        clip,
        center.0 - reach,
        center.1 - reach,
        center.0 + reach,
        center.1 + reach,
    ) else {
        return;
    };
    for y in lo_y..hi_y {
        for x in lo_x..hi_x {
            let (dx, dy) = (x as f64 + 0.5 - center.0, y as f64 + 0.5 - center.1);
            let dist = (dx * dx + dy * dy).sqrt();
            let ring = (dist - radius).abs() - stroke / 2.0;
            let i = ((y * canvas_w + x) * 4) as usize;
            let px = &mut canvas[i..i + 4];
            blend(px, FILL, 0.18 * coverage(dist - radius) * alpha);
            blend(px, EDGE, 0.6 * coverage(ring - 1.5) * alpha);
            blend(px, FILL, 0.9 * coverage(ring) * alpha);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Button;

    fn cursor(t: f64, x: f64, y: f64) -> TimedEvent {
        TimedEvent {
            t,
            event: Event::Cursor { x, y },
        }
    }

    fn click(t: f64, x: f64, y: f64) -> TimedEvent {
        TimedEvent {
            t,
            event: Event::Click {
                x,
                y,
                button: Button::Left,
            },
        }
    }

    #[test]
    fn a_drag_ripples_at_its_press_and_follows_the_samples_after() {
        let p = Pointer::new(&[
            cursor(0.0, 0.5, 0.5),
            TimedEvent {
                t: 4.0,
                event: Event::Drag {
                    x1: 0.1,
                    y1: 0.2,
                    x2: 0.9,
                    y2: 0.2,
                    button: Button::Left,
                },
            },
            cursor(4.03, 0.5, 0.2),
            cursor(4.1, 0.9, 0.2),
        ]);
        let at_press = p.look(4.0, CursorMode::Auto).unwrap();
        assert_eq!((at_press.x, at_press.y), (0.1, 0.2));
        assert_eq!(at_press.opacity, 1.0);
        let ripple = at_press.ripple.expect("a ripple at the press");
        assert_eq!((ripple.x, ripple.y, ripple.progress), (0.1, 0.2, 0.0));
        let after = p.look(4.2, CursorMode::Auto).unwrap();
        assert_eq!((after.x, after.y), (0.9, 0.2));
        assert!(
            p.look(3.0, CursorMode::Auto).is_none(),
            "idle before the lead-in"
        );
    }

    /// Idle at the middle from t0, a move at 5s, a click at 10s, idle after.
    fn take() -> Pointer {
        Pointer::new(&[
            cursor(0.0, 0.5, 0.5),
            cursor(5.0, 0.9, 0.8),
            click(10.0, 0.2, 0.3),
            cursor(10.02, 0.2, 0.3),
        ])
    }

    fn opacity(p: &Pointer, t: f64) -> f64 {
        p.look(t, CursorMode::Auto).map_or(0.0, |l| l.opacity)
    }

    #[test]
    fn an_idle_pointer_is_hidden() {
        let p = take();
        for t in [0.0, 1.0, 3.0, 4.7] {
            assert_eq!(p.look(t, CursorMode::Auto), None, "t={t}");
        }
        assert_eq!(
            opacity(&p, 8.0),
            0.0,
            "gone again between the move and the click"
        );
    }

    #[test]
    fn a_move_fades_in_quickly_and_glides_to_where_the_pointer_went() {
        let p = take();
        let start = 5.0 - TIMING.glide;
        assert_eq!(opacity(&p, start), 0.0);
        let half = opacity(&p, start + TIMING.fade_in / 2.0);
        assert!(
            (half - 0.5).abs() < 1e-9,
            "half way through the fade: {half}"
        );
        assert_eq!(opacity(&p, start + TIMING.fade_in), 1.0);
        let mid = p.look(5.0 - TIMING.glide / 2.0, CursorMode::Auto).unwrap();
        assert!(
            (mid.x - 0.7).abs() < 1e-9 && (mid.y - 0.65).abs() < 1e-9,
            "{mid:?}"
        );
        let there = p.look(5.0, CursorMode::Auto).unwrap();
        assert_eq!((there.x, there.y), (0.9, 0.8));
    }

    #[test]
    fn the_pointer_holds_then_fades_out_after_the_last_move() {
        let p = take();
        assert_eq!(opacity(&p, 5.0 + TIMING.hold), 1.0);
        let fading = opacity(&p, 5.0 + TIMING.hold + TIMING.fade_out / 2.0);
        assert!((fading - 0.5).abs() < 1e-9, "{fading}");
        assert_eq!(opacity(&p, 5.0 + TIMING.hold + TIMING.fade_out), 0.0);
        assert_eq!(
            p.look(5.0 + TIMING.hold + TIMING.fade_out + 0.01, CursorMode::Auto),
            None
        );
    }

    #[test]
    fn a_click_shows_the_pointer_before_it_lands() {
        let p = take();
        let lead = 10.0 - TIMING.lead;
        assert_eq!(opacity(&p, lead - 0.01), 0.0);
        assert!(opacity(&p, lead + 0.01) > 0.0);
        assert_eq!(opacity(&p, lead + TIMING.fade_in + 1e-6), 1.0);
        let before = p.look(lead + 0.01, CursorMode::Auto).unwrap();
        assert_eq!((before.x, before.y), (0.9, 0.8), "still where it rested");
        let landed = p.look(10.0, CursorMode::Auto).unwrap();
        assert_eq!((landed.x, landed.y), (0.2, 0.3));
        assert_eq!(opacity(&p, 10.0 + TIMING.hold), 1.0);
        assert_eq!(
            opacity(&p, 10.0 + TIMING.hold + TIMING.fade_out),
            0.0,
            "the sample echoing the click is not a move that extends the hold"
        );
    }

    #[test]
    fn a_click_ripples_for_its_window_in_auto_and_always() {
        let p = take();
        for mode in [CursorMode::Auto, CursorMode::Always] {
            assert_eq!(p.look(9.99, mode).and_then(|l| l.ripple), None);
            let r = p
                .look(10.0 + TIMING.ripple / 2.0, mode)
                .unwrap()
                .ripple
                .unwrap();
            assert_eq!((r.x, r.y), (0.2, 0.3));
            assert!((r.progress - 0.5).abs() < 1e-9);
            assert_eq!(
                p.look(10.0 + TIMING.ripple, mode).and_then(|l| l.ripple),
                None
            );
        }
        assert_eq!(p.look(10.1, CursorMode::Never), None);
    }

    #[test]
    fn always_shows_the_pointer_from_the_first_sample() {
        let p = take();
        let l = p.look(1.0, CursorMode::Always).unwrap();
        assert_eq!((l.x, l.y, l.opacity), (0.5, 0.5, 1.0));
        assert_eq!(Pointer::new(&[]).look(1.0, CursorMode::Always), None);
    }

    #[test]
    fn continuous_motion_interpolates_between_samples() {
        let p = Pointer::new(&[
            cursor(1.0, 0.1, 0.1),
            cursor(1.1, 0.2, 0.1),
            cursor(1.2, 0.3, 0.1),
        ]);
        let l = p.look(1.15, CursorMode::Auto).unwrap();
        assert!((l.x - 0.25).abs() < 1e-9, "{l:?}");
    }

    fn canvas(w: u32, h: u32) -> Vec<u8> {
        [40u8, 80, 120, 255].repeat((w * h) as usize)
    }

    fn touched(before: &[u8], after: &[u8], w: u32) -> Vec<(u32, u32)> {
        before
            .chunks(4)
            .zip(after.chunks(4))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| (i as u32 % w, i as u32 / w))
            .collect()
    }

    #[test]
    fn the_arrow_stays_inside_its_bounds_and_reads_white_on_dark_edge() {
        let (w, h) = (200, 200);
        let clip = Rect { x: 0, y: 0, w, h };
        let before = canvas(w, h);
        let mut after = before.clone();
        let tip = (50.3, 60.7);
        draw_arrow(tip, 54.0, 1.0, clip, &mut after, w);
        let hit = touched(&before, &after, w);
        let (x0, y0, x1, y1) = arrow_bounds(tip, 54.0);
        assert!(!hit.is_empty());
        for &(x, y) in &hit {
            assert!(x as f64 >= x0.floor() && (x as f64) < x1.ceil(), "x={x}");
            assert!(y as f64 >= y0.floor() && (y as f64) < y1.ceil(), "y={y}");
        }
        let px = |x: u32, y: u32| &after[((y * w + x) * 4) as usize..][..3];
        assert_eq!(px(54, 85), [255, 255, 255], "inside the arrow is white");
        assert!(
            px(49, 75).iter().all(|&c| c < 40),
            "the left edge is dark: {:?}",
            px(49, 75)
        );
        assert_eq!(px(120, 30), [40, 80, 120], "far away is untouched");
        let max_x = hit.iter().map(|p| p.0).max().unwrap();
        let max_y = hit.iter().map(|p| p.1).max().unwrap();
        assert!((max_x as f64) < tip.0 + 54.0 * 0.85, "{max_x}");
        assert!((max_y as f64) < tip.1 + 54.0 * 1.2, "{max_y}");
    }

    #[test]
    fn nothing_is_drawn_outside_the_clip() {
        let (w, h) = (200, 200);
        let clip = Rect {
            x: 60,
            y: 0,
            w: 100,
            h: 80,
        };
        let before = canvas(w, h);
        let mut after = before.clone();
        let look = Look {
            x: 0.28,
            y: 0.3,
            opacity: 1.0,
            ripple: Some(Ripple {
                x: 0.28,
                y: 0.3,
                progress: 0.3,
            }),
        };
        let at = Placement {
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            source: (w, h),
            clip,
        };
        draw(&look, &at, &mut after, w, 1080);
        let hit = touched(&before, &after, w);
        assert!(!hit.is_empty());
        assert!(hit
            .iter()
            .all(|&(x, y)| (60..160).contains(&x) && (0..80).contains(&y)));
        let mut faded = before.clone();
        draw(
            &Look {
                opacity: 0.0,
                ripple: None,
                ..look
            },
            &at,
            &mut faded,
            w,
            1080,
        );
        assert_eq!(faded, before, "a hidden pointer draws nothing");
    }
}
