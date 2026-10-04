//! The risograph attention border: one grainy ring per spot ink, each on its own misregistered
//! plate, multiplied where they overlap. A port of the macOS `Border.swift` and the ring in
//! `Compositor.swift`; the timing constants are the same.

use super::layout::Rect;
use std::f64::consts::PI;

pub const INTRO_DURATION: f64 = 1.5;
pub const CALM_WIDTH: f64 = 0.022;
pub const CALM_OPACITY: f64 = 0.95;
pub const CALM_SPREAD: f64 = 0.007;
pub const BOIL_FPS: f64 = 10.0;
/// Per-step plate wobble, independent of spread.
pub const BOIL_JITTER: f64 = 0.004;

const INTRO_WIDTH: f64 = 0.05;
const PULSE_WIDTH: f64 = 0.03;
const PULSE_CENTER: f64 = 0.95;
const PULSE_HALF_WIDTH: f64 = 0.25;
const INTRO_SPREAD: f64 = 0.08;
const REGISTER_BY: f64 = 0.75;
const KICK_SPREAD: f64 = 0.025;
const KICK_HALF_WIDTH: f64 = 0.2;

/// The ring's look at one instant. Lengths are fractions of the target's `unit`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// Stroke width of each plate's ring.
    pub width: f64,
    /// How far each plate sits from true registration.
    pub spread: f64,
    pub opacity: f64,
    /// Riso animation steps at a low frame rate; this indexes the step for jitter and grain.
    pub boil_frame: i64,
}

/// One spot ink. `drift` is the direction its plate slides when out of register, +y up.
pub struct Ink {
    pub rgb: [f32; 3],
    pub drift: f64,
}

/// Fluorescent pink, riso blue, yellow: three plates drifting 120 degrees apart.
pub const INKS: [Ink; 3] = [
    Ink {
        rgb: [1.00, 0.28, 0.69],
        drift: 200.0 * PI / 180.0,
    },
    Ink {
        rgb: [0.00, 0.47, 0.75],
        drift: 320.0 * PI / 180.0,
    },
    Ink {
        rgb: [1.00, 0.91, 0.00],
        drift: 80.0 * PI / 180.0,
    },
];

pub fn look(t: f64) -> Look {
    let t = t.max(0.0);
    let fade_in = ease_out(clamp01(t / 0.15));
    let settle = smoothstep(0.6, INTRO_DURATION, t);
    let pulse = bump(t, PULSE_CENTER, PULSE_HALF_WIDTH);
    Look {
        width: CALM_WIDTH + (INTRO_WIDTH - CALM_WIDTH) * (1.0 - settle) + PULSE_WIDTH * pulse,
        spread: CALM_SPREAD
            + (INTRO_SPREAD - CALM_SPREAD) * (1.0 - ease_out(clamp01(t / REGISTER_BY)))
            + KICK_SPREAD * bump(t, PULSE_CENTER, KICK_HALF_WIDTH),
        opacity: fade_in * (1.0 + (CALM_OPACITY - 1.0) * smoothstep(1.0, INTRO_DURATION, t)),
        boil_frame: (t * BOIL_FPS).floor() as i64,
    }
}

/// Where a plate's ring sits relative to the target, in units, +x right and +y up.
pub fn plate_offset(plate: usize, look: &Look) -> (f64, f64) {
    let drift = INKS[plate].drift;
    let salt = plate as i64 * 2;
    (
        look.spread * drift.cos() + BOIL_JITTER * noise(look.boil_frame, salt),
        look.spread * drift.sin() + BOIL_JITTER * noise(look.boil_frame, salt + 1),
    )
}

/// Deterministic hash noise in -1..=1, so a re-export draws the same wobble.
fn noise(frame: i64, salt: i64) -> f64 {
    let h = mix64((frame.wrapping_mul(73_856_093) ^ salt.wrapping_mul(19_349_663)) as u64);
    (h % 20_001) as f64 / 10_000.0 - 1.0
}

fn mix64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

fn ease_out(x: f64) -> f64 {
    1.0 - (1.0 - x).powi(3)
}

fn smoothstep(a: f64, b: f64, x: f64) -> f64 {
    let u = clamp01((x - a) / (b - a));
    u * u * (3.0 - 2.0 * u)
}

fn bump(x: f64, center: f64, half_width: f64) -> f64 {
    let u = (x - center) / half_width;
    if u.abs() >= 1.0 {
        return 0.0;
    }
    0.5 * (1.0 + (PI * u).cos())
}

/// The outline a ring hugs from the outside, in canvas pixels with a top-left origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    #[allow(dead_code, reason = "the camera target, once takes carry a camera")]
    Circle { cx: f32, cy: f32, r: f32 },
    RoundedRect {
        cx: f32,
        cy: f32,
        half_w: f32,
        half_h: f32,
        radius: f32,
    },
}

impl Shape {
    /// Signed distance from (x, y) to the outline: negative inside, positive outside.
    pub fn sdf(&self, x: f32, y: f32) -> f32 {
        match *self {
            Shape::Circle { cx, cy, r } => (x - cx).hypot(y - cy) - r,
            Shape::RoundedRect {
                cx,
                cy,
                half_w,
                half_h,
                radius,
            } => {
                let qx = (x - cx).abs() - (half_w - radius);
                let qy = (y - cy).abs() - (half_h - radius);
                qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
            }
        }
    }

    /// The outline's bounding box: (left, top, right, bottom).
    fn bounds(&self) -> [f32; 4] {
        let (cx, cy, hw, hh) = match *self {
            Shape::Circle { cx, cy, r } => (cx, cy, r, r),
            Shape::RoundedRect {
                cx,
                cy,
                half_w,
                half_h,
                ..
            } => (cx, cy, half_w, half_h),
        };
        [cx - hw, cy - hh, cx + hw, cy + hh]
    }

    /// A box whose every point is at least `depth` inside the outline, so the ring never reaches it.
    fn hollow(&self, depth: f32) -> Option<[f32; 4]> {
        let (cx, cy, hw, hh) = match *self {
            Shape::Circle { cx, cy, r } => {
                let h = (r - depth) / std::f32::consts::SQRT_2;
                (cx, cy, h, h)
            }
            Shape::RoundedRect {
                cx,
                cy,
                half_w,
                half_h,
                radius,
            } => (cx, cy, half_w - depth - radius, half_h - depth - radius),
        };
        (hw > 0.0 && hh > 0.0).then_some([cx - hw, cy - hh, cx + hw, cy + hh])
    }
}

/// A border target: the outline and the length that `Look`'s fractions are measured in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub shape: Shape,
    pub unit: f32,
}

/// The diameter of the 16:9 camera circle on macOS, so a screen ring strokes like a camera ring.
const SCREEN_UNIT: f32 = 300.0;

/// Edge softness each side of a ring edge, in pixels, matching the macOS radial gradients.
const FEATHER: f32 = 0.75;

impl Target {
    pub fn screen(panel: Rect) -> Target {
        Target {
            shape: Shape::RoundedRect {
                cx: panel.x as f32 + panel.w as f32 / 2.0,
                cy: panel.y as f32 + panel.h as f32 / 2.0,
                half_w: panel.w as f32 / 2.0,
                half_h: panel.h as f32 / 2.0,
                radius: 0.0,
            },
            unit: SCREEN_UNIT,
        }
    }

    /// Composites the ring at `look` over an RGBA frame of `width` x `height`.
    #[cfg(test)]
    pub fn draw(&self, look: &Look, buf: &mut [u8], width: u32, height: u32) {
        self.paint(look, width, height).apply(buf);
    }

    /// The ring at `look` for a frame of `width` x `height`, ready to composite over any
    /// number of frames that share that look.
    pub fn paint(&self, look: &Look, width: u32, height: u32) -> Overlay {
        let mut overlay = Overlay(Vec::new());
        if look.opacity <= 0.001 {
            return overlay;
        }
        let u = self.unit;
        let outer = look.width as f32 * u;
        let wander = (look.spread + BOIL_JITTER * std::f64::consts::SQRT_2) as f32 * u + 2.0;
        let plates: [(f32, f32, [f32; 3]); 3] = std::array::from_fn(|i| {
            let (dx, dy) = plate_offset(i, look);
            (
                dx as f32 * u,
                -dy as f32 * u,
                INKS[i].rgb.map(srgb_to_linear),
            )
        });
        let grains: [Grain; 3] = std::array::from_fn(|i| Grain::new(i, look.boil_frame));
        let fade = look.opacity as f32;

        let [l, t, r, b] = self.shape.bounds();
        let reach = outer + wander;
        let clip = |v: f32, max: u32| v.floor().clamp(0.0, max as f32) as u32;
        let (x0, x1) = (clip(l - reach, width), clip(r + reach + 1.0, width));
        let (y0, y1) = (clip(t - reach, height), clip(b + reach + 1.0, height));
        let hollow = self.shape.hollow(wander);

        for y in y0..y1 {
            let py = y as f32 + 0.5;
            let row = (y * width) as usize * 4;
            let mut x = x0;
            while x < x1 {
                let px = x as f32 + 0.5;
                if let Some([hl, ht, hr, hb]) = hollow {
                    if py > ht && py < hb && px > hl && px < hr {
                        x = (hr.floor() as u32).max(x + 1);
                        continue;
                    }
                }
                let mut ink = [1.0f32; 3];
                let mut coverage = 0.0f32;
                for (i, &(dx, dy, rgb)) in plates.iter().enumerate() {
                    let d = self.shape.sdf(px - dx, py - dy);
                    let ring = ramp((outer + FEATHER - d) / (2.0 * FEATHER))
                        * ramp((d + FEATHER) / (2.0 * FEATHER));
                    if ring <= 0.0 {
                        continue;
                    }
                    let m = ring * grains[i].at(px, py);
                    for c in 0..3 {
                        ink[c] *= 1.0 - m * (1.0 - rgb[c]);
                    }
                    coverage = coverage.max(m);
                }
                let a = coverage * fade;
                if a > 0.0 {
                    overlay.0.push(Dab {
                        at: row + x as usize * 4,
                        ink,
                        a,
                    });
                }
                x += 1;
            }
        }
        overlay
    }
}

/// The inked pixels of one ring look: where each falls in an RGBA frame and how it blends.
pub struct Overlay(Vec<Dab>);

struct Dab {
    at: usize,
    /// Linear-light color of the multiplied plates.
    ink: [f32; 3],
    a: f32,
}

impl Overlay {
    pub fn apply(&self, buf: &mut [u8]) {
        let lin = Linear::get();
        for d in &self.0 {
            for c in 0..3 {
                let under = lin.decode[buf[d.at + c] as usize];
                buf[d.at + c] = lin.encode(under + (d.ink[c] - under) * d.a);
            }
        }
    }
}

/// Core Image composites in linear light, so the inks multiply and blend in linear here too;
/// in gamma space, half-covered grain reads as half-dark instead of mostly inked.
struct Linear {
    decode: [f32; 256],
    encode: Vec<u8>,
}

const ENCODE_STEPS: usize = 4096;

impl Linear {
    fn get() -> &'static Linear {
        static LUT: std::sync::OnceLock<Linear> = std::sync::OnceLock::new();
        LUT.get_or_init(|| Linear {
            decode: std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)),
            encode: (0..=ENCODE_STEPS)
                .map(|i| (linear_to_srgb(i as f32 / ENCODE_STEPS as f32) * 255.0).round() as u8)
                .collect(),
        })
    }

    fn encode(&self, v: f32) -> u8 {
        self.encode[(ramp(v) * ENCODE_STEPS as f32).round() as usize]
    }
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn ramp(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

/// Speckled ink coverage: mostly solid with pinhole voids, reshuffled on every boil step.
/// Bilinear value noise on a 1.6px lattice, contrast-stretched and clamped like the macOS
/// `CIRandomGenerator` chain. That chain, rendered through Core Image, leaves about 4.6% of
/// pixels void and 85% solid; the thresholds below reproduce those fractions for this noise.
struct Grain {
    ox: f32,
    oy: f32,
    salt: u64,
}

const GRAIN_SCALE: f32 = 1.6;
const GRAIN_VOID: f32 = 0.175;
const GRAIN_SOLID: f32 = 0.283;

impl Grain {
    fn new(plate: usize, frame: i64) -> Grain {
        let p = plate as i64;
        Grain {
            ox: (frame * 97 + p * 211) as f32,
            oy: (frame * 31 + p * 157) as f32,
            salt: p as u64,
        }
    }

    fn at(&self, x: f32, y: f32) -> f32 {
        let (gx, gy) = (x / GRAIN_SCALE - self.ox, y / GRAIN_SCALE - self.oy);
        let (fx, fy) = (gx.floor(), gy.floor());
        let (tx, ty) = (gx - fx, gy - fy);
        let (ix, iy) = (fx as i64, fy as i64);
        let top = lerp(self.lattice(ix, iy), self.lattice(ix + 1, iy), tx);
        let bottom = lerp(self.lattice(ix, iy + 1), self.lattice(ix + 1, iy + 1), tx);
        ramp((lerp(top, bottom, ty) - GRAIN_VOID) / (GRAIN_SOLID - GRAIN_VOID))
    }

    fn lattice(&self, ix: i64, iy: i64) -> f32 {
        let h = mix64(
            (ix as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (iy as u64) ^ (self.salt << 56),
        );
        (h >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn starts_invisible_and_far_out_of_register() {
        let l = look(0.0);
        assert_eq!(l.opacity, 0.0);
        assert_eq!(look(-1.0), l);
        assert!(l.spread > CALM_SPREAD * 10.0, "{l:?}");
        assert_eq!(l.width, INTRO_WIDTH);
        assert_eq!(l.boil_frame, 0);
    }

    #[test]
    fn snaps_into_register_by_three_quarters_of_a_second() {
        let l = look(0.75);
        assert!(close(l.spread, CALM_SPREAD, 1e-3), "{l:?}");
        assert!(close(look(0.5).opacity, 1.0, 1e-9));
    }

    #[test]
    fn kicks_apart_once_on_the_beat_with_a_thick_pulse() {
        let beat = look(PULSE_CENTER);
        assert!(beat.spread > look(0.75).spread * 3.0, "{beat:?}");
        assert!(close(beat.spread, CALM_SPREAD + KICK_SPREAD, 1e-12));
        assert!(beat.width > CALM_WIDTH * 2.5);
        assert!(beat.width > look(0.5).width && beat.width > look(1.3).width);
        assert!(
            look(1.2).spread < beat.spread / 3.0,
            "the kick is a single beat"
        );
    }

    #[test]
    fn settles_continuously_at_the_end_of_the_intro() {
        let (a, b) = (look(INTRO_DURATION - 1e-4), look(INTRO_DURATION + 1e-4));
        assert!(close(a.width, b.width, 1e-3));
        assert!(close(a.spread, b.spread, 1e-3));
        assert!(close(a.opacity, b.opacity, 1e-3));
        let calm = look(INTRO_DURATION);
        assert_eq!(
            (calm.width, calm.spread, calm.opacity),
            (CALM_WIDTH, CALM_SPREAD, CALM_OPACITY)
        );
    }

    #[test]
    fn calm_after_the_intro_except_the_boil() {
        let mut t = INTRO_DURATION;
        while t < 120.0 {
            let l = look(t);
            assert_eq!((l.width, l.spread), (CALM_WIDTH, CALM_SPREAD), "t={t}");
            assert!(close(l.opacity, CALM_OPACITY, 1e-12), "t={t}");
            t += 0.37;
        }
    }

    #[test]
    fn boils_at_ten_frames_per_second() {
        assert_eq!(look(2.01), look(2.09), "identical within one 100ms step");
        assert_eq!(look(2.11).boil_frame, look(2.01).boil_frame + 1);
        assert_ne!(look(2.11), look(2.01));
        for plate in 0..3 {
            assert_ne!(
                plate_offset(plate, &look(2.01)),
                plate_offset(plate, &look(2.11)),
                "plate {plate} wobbles between steps"
            );
        }
        let at_fps = |f: u32| look(f as f64 / 30.0);
        assert_eq!(
            at_fps(60),
            at_fps(62),
            "30 fps frames in one boil step look the same"
        );
        assert_ne!(at_fps(62), at_fps(63));
    }

    #[test]
    fn calm_plates_stay_nearly_registered() {
        let limit = CALM_SPREAD + BOIL_JITTER * 2f64.sqrt() + 1e-12;
        for step in 20..300 {
            let l = look(step as f64 / 10.0);
            for plate in 0..3 {
                let (dx, dy) = plate_offset(plate, &l);
                assert!(
                    dx.hypot(dy) <= limit,
                    "t={} plate={plate}",
                    step as f64 / 10.0
                );
            }
        }
    }

    #[test]
    fn plates_start_apart_in_different_directions() {
        let l = look(0.0);
        let o: Vec<_> = (0..3).map(|p| plate_offset(p, &l)).collect();
        for i in 0..3 {
            for j in i + 1..3 {
                assert!((o[i].0 - o[j].0).hypot(o[i].1 - o[j].1) > l.spread);
            }
        }
    }

    #[test]
    fn noise_matches_the_swift_hash() {
        // Values printed by Border.noise in Border.swift.
        assert!(close(noise(7, 3), 0.8122, 1e-12));
        assert!(close(noise(123_456, 5), 0.3573, 1e-12));
        assert!(close(noise(-3, 1), -0.4638, 1e-12));
        for frame in 0..500 {
            assert_eq!(noise(frame, 3), noise(frame, 3));
            assert!(noise(frame, 3).abs() <= 1.0);
        }
    }

    #[test]
    fn rounded_rect_sdf() {
        let s = Shape::RoundedRect {
            cx: 100.0,
            cy: 50.0,
            half_w: 40.0,
            half_h: 20.0,
            radius: 10.0,
        };
        assert_eq!(
            s.sdf(100.0, 50.0),
            -20.0,
            "center is half the short side inside"
        );
        assert_eq!(s.sdf(140.0, 50.0), 0.0, "on the right edge");
        assert_eq!(s.sdf(150.0, 50.0), 10.0);
        assert_eq!(s.sdf(100.0, 25.0), 5.0, "above the top edge");
        assert_eq!(s.sdf(135.0, 50.0), -5.0);
        let corner = s.sdf(150.0, 80.0);
        let expected = (20f32).hypot(20.0) - 10.0;
        assert!((corner - expected).abs() < 1e-4, "rounded corner: {corner}");
        let square = Shape::RoundedRect {
            cx: 100.0,
            cy: 50.0,
            half_w: 40.0,
            half_h: 20.0,
            radius: 0.0,
        };
        assert!(
            (square.sdf(143.0, 74.0) - 5.0).abs() < 1e-4,
            "square corner"
        );
    }

    #[test]
    fn circle_sdf() {
        let s = Shape::Circle {
            cx: 0.0,
            cy: 0.0,
            r: 10.0,
        };
        assert_eq!(s.sdf(0.0, 0.0), -10.0);
        assert_eq!(s.sdf(6.0, 8.0), 0.0);
        assert_eq!(s.sdf(0.0, -15.0), 5.0);
    }

    #[test]
    fn hollow_is_out_of_the_rings_reach() {
        let shapes = [
            Shape::RoundedRect {
                cx: 0.0,
                cy: 0.0,
                half_w: 50.0,
                half_h: 30.0,
                radius: 8.0,
            },
            Shape::Circle {
                cx: 0.0,
                cy: 0.0,
                r: 40.0,
            },
        ];
        for s in shapes {
            let [l, t, r, b] = s.hollow(5.0).unwrap();
            for (x, y) in [(l, t), (r, t), (l, b), (r, b), (l, 0.0), (0.0, t)] {
                assert!(s.sdf(x, y) <= -5.0 + 1e-4, "{s:?} at ({x}, {y})");
            }
        }
    }

    #[test]
    fn grain_is_mostly_solid_with_pinholes_like_core_image() {
        let g = Grain::new(1, 42);
        let samples: Vec<f32> = (0..256 * 256)
            .map(|i| g.at((i % 256) as f32 + 0.5, (i / 256) as f32 + 0.5))
            .collect();
        let share = |f: fn(f32) -> bool| {
            samples.iter().filter(|&&v| f(v)).count() as f32 / samples.len() as f32
        };
        let void = share(|v| v == 0.0);
        let solid = share(|v| v == 1.0);
        assert!((0.03..0.07).contains(&void), "void {void}");
        assert!((0.80..0.90).contains(&solid), "solid {solid}");
    }

    #[test]
    fn linear_light_round_trips_every_byte() {
        let lin = Linear::get();
        for b in 0..=255u8 {
            assert_eq!(lin.encode(lin.decode[b as usize]), b);
        }
        assert!(
            (lin.decode[128] - 0.2158).abs() < 1e-3,
            "sRGB mid-gray is ~21.6% linear"
        );
    }

    fn frame(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        [rgb[0], rgb[1], rgb[2], 255].repeat((w * h) as usize)
    }

    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2]]
    }

    const PANEL: Rect = Rect {
        x: 100,
        y: 100,
        w: 200,
        h: 100,
    };

    #[test]
    fn draws_ink_just_outside_the_panel_and_leaves_the_rest() {
        let (w, h) = (400, 300);
        let bg = [20, 20, 30];
        let mut buf = frame(w, h, bg);
        let target = Target::screen(PANEL);
        let calm = look(5.0);
        target.draw(&calm, &mut buf, w, h);

        assert_eq!(px(&buf, w, 200, 150), bg, "panel center untouched");
        assert_eq!(px(&buf, w, 5, 5), bg, "far canvas untouched");
        assert!(
            buf.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
            "alpha stays opaque"
        );
        let stroke = (CALM_WIDTH as f32 * SCREEN_UNIT) as u32;
        let band: Vec<[u8; 3]> = (PANEL.x + 20..PANEL.x + 180)
            .map(|x| px(&buf, w, x, PANEL.y - stroke / 2))
            .collect();
        let inked = band.iter().filter(|&&p| p != bg).count();
        assert!(
            inked > band.len() * 3 / 4,
            "ring is mostly solid: {inked}/{}",
            band.len()
        );
        let shades: std::collections::HashSet<_> = band.iter().collect();
        assert!(
            shades.len() > 10,
            "grain varies the ink: {} shades",
            shades.len()
        );
    }

    #[test]
    fn same_look_draws_the_same_pixels_and_a_new_boil_step_differs() {
        let (w, h) = (400, 300);
        let target = Target::screen(PANEL);
        let draw = |t: f64| {
            let mut buf = frame(w, h, [0; 3]);
            target.draw(&look(t), &mut buf, w, h);
            buf
        };
        assert!(draw(3.01) == draw(3.09));
        assert!(draw(3.01) != draw(3.11));
    }

    #[test]
    fn invisible_at_the_first_frame() {
        let (w, h) = (400, 300);
        let mut buf = frame(w, h, [9, 9, 9]);
        Target::screen(PANEL).draw(&look(0.0), &mut buf, w, h);
        assert!(buf == frame(w, h, [9, 9, 9]));
    }
}
