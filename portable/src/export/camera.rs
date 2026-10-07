//! The pan camera for a cropped viewport: where the window onto a wide terminal or screen sits
//! each frame. Its position is a pure function of the frames it has seen, so re-exports match.
//! It works in surface units, whatever the surface is: columns for a terminal, pixels for a
//! screen recording.

use super::FPS;

/// What one frame says is worth looking at, as `(left, right)` spans of the surface.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Focus {
    /// What changed since the previous frame.
    pub ink: Option<(f64, f64)>,
    /// Where the cursor or pointer is, when it shows.
    pub cursor: Option<(f64, f64)>,
}

/// Grid column `c` spans surface columns `c + 1 .. c + 2`: the panel pads the grid by a column.
fn cols(lo: u16, hi: u16) -> (f64, f64) {
    (lo as f64 + 1.0, hi as f64 + 2.0)
}

impl Focus {
    /// Compares two terminal screens of the same size. With no previous screen, all ink is new.
    pub fn between(prev: Option<&vt100::Screen>, now: &vt100::Screen) -> Focus {
        let (rows, width) = now.size();
        let mut ink: Option<(u16, u16)> = None;
        for row in 0..rows {
            for col in 0..width {
                let Some(cell) = now.cell(row, col) else {
                    continue;
                };
                let inked = cell.contents().chars().any(|c| !c.is_whitespace());
                if inked && prev.and_then(|p| p.cell(row, col)) != Some(cell) {
                    ink = Some(ink.map_or((col, col), |(lo, hi)| (lo.min(col), hi.max(col))));
                }
            }
        }
        let cursor = (!now.hide_cursor()).then(|| now.cursor_position().1);
        Focus {
            ink: ink.map(|(lo, hi)| cols(lo, hi)),
            cursor: cursor.map(|c| cols(c, c)),
        }
    }
}

/// Angular frequency of the critically damped spring: it settles in about half a second.
const OMEGA: f64 = 8.0;
const SETTLED: f64 = 0.01;

/// Positions are in surface units.
#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    /// How many surface units the window shows.
    window: f64,
    /// The largest left edge, which puts the window against the right edge of the surface.
    max: f64,
    /// Focus within this many units of the window edge counts as leaving the window.
    margin: f64,
    target: f64,
    x: f64,
    velocity: f64,
}

impl Camera {
    pub fn new(window: f64, surface: f64) -> Camera {
        Camera {
            window,
            max: (surface - window).max(0.0),
            margin: (window / 6.0).floor(),
            target: 0.0,
            x: 0.0,
            velocity: 0.0,
        }
    }

    /// The window's left edge on the surface.
    pub fn x(&self) -> f64 {
        self.x
    }

    /// Puts the window where `focus` aims it, with no glide, for a take's first frame.
    pub fn place(&mut self, focus: Focus) {
        self.target = self.aim(focus);
        self.x = self.target;
        self.velocity = 0.0;
    }

    /// Advances one frame.
    pub fn step(&mut self, focus: Focus) {
        self.target = self.aim(focus);
        let dt = 1.0 / FPS as f64;
        let d = self.x - self.target;
        let b = self.velocity + OMEGA * d;
        let decay = (-OMEGA * dt).exp();
        let d = (d + b * dt) * decay;
        self.velocity = (b - OMEGA * (d / decay)) * decay;
        self.x = (self.target + d).clamp(0.0, self.max);
        if d.abs() < SETTLED && self.velocity.abs() < SETTLED {
            self.x = self.target;
            self.velocity = 0.0;
        }
    }

    /// Keeps the current target while new ink and the cursor sit comfortably inside it. When
    /// ink leaves, the window centers on it, or starts at its left end when it is wider than
    /// the window. The cursor wins over ink, and the window recenters on it when it leaves.
    fn aim(&self, focus: Focus) -> f64 {
        let mut target = self.target;
        if let Some((lo, hi)) = focus.ink {
            if !self.comfortable(target, lo, hi) {
                target = if hi - lo <= self.window - 2.0 * self.margin {
                    (lo + hi - self.window) / 2.0
                } else {
                    lo - self.margin
                };
                target = target.clamp(0.0, self.max);
            }
        }
        if let Some((lo, hi)) = focus.cursor {
            if !self.comfortable(target, lo, hi) {
                target = ((lo + hi - self.window) / 2.0).clamp(0.0, self.max);
            }
        }
        target
    }

    /// Whether `lo..hi` sits inside the window at `x`, clear of its margins; against an edge of
    /// the surface there is nothing further to reveal, so that side has no margin.
    fn comfortable(&self, x: f64, lo: f64, hi: f64) -> bool {
        let left = x <= 0.0 || lo >= x + self.margin;
        let right = x >= self.max || hi <= x + self.window - self.margin;
        left && right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 100-column grid seen through a 55-column window: max left edge is 47.
    fn camera() -> Camera {
        Camera::new(55.0, 102.0)
    }

    fn cursor(c: u16) -> Focus {
        Focus {
            ink: None,
            cursor: Some(cols(c, c)),
        }
    }

    fn ink(lo: u16, hi: u16) -> Option<(f64, f64)> {
        Some(cols(lo, hi))
    }

    fn run(cam: &mut Camera, focus: Focus, frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|_| {
                cam.step(focus);
                cam.x()
            })
            .collect()
    }

    #[test]
    fn typing_inside_the_dead_zone_never_moves_the_camera() {
        let mut cam = camera();
        for c in 2..40 {
            let typed = Focus {
                ink: ink(c - 1, c - 1),
                cursor: cursor(c).cursor,
            };
            assert_eq!(
                run(&mut cam, typed, 3),
                vec![0.0; 3],
                "moved while typing at column {c}"
            );
        }
    }

    #[test]
    fn converges_on_the_cursor_once_it_leaves_and_then_holds_still() {
        let mut cam = camera();
        let xs = run(&mut cam, cursor(90), 60);
        let target: f64 = (91.0 + 92.0 - 55.0) / 2.0;
        assert!(
            xs[0] > 0.0 && xs[0] < 5.0,
            "eases out instead of jumping: {}",
            xs[0]
        );
        assert!(
            xs.windows(2).all(|w| w[1] >= w[0]),
            "no overshoot or wobble: {xs:?}"
        );
        assert_eq!(xs[59], target.min(47.0));
        assert_eq!(
            run(&mut cam, cursor(90), 30),
            vec![xs[59]; 30],
            "idle frames do not jitter"
        );
        assert_eq!(run(&mut cam, Focus::default(), 30), vec![xs[59]; 30]);
    }

    #[test]
    fn never_leaves_the_surface() {
        let mut cam = camera();
        let foci = [
            cursor(99),
            cursor(0),
            cursor(99),
            Focus {
                ink: ink(0, 99),
                cursor: None,
            },
            cursor(60),
        ];
        for (i, f) in foci.iter().cycle().take(40).enumerate() {
            for x in run(&mut cam, *f, 1 + i % 7) {
                assert!((0.0..=47.0).contains(&x), "left edge {x} outside 0..=47");
            }
        }
    }

    #[test]
    fn hidden_cursor_follows_ink_and_wide_ink_shows_its_left_end() {
        let mut cam = camera();
        let far = Focus {
            ink: ink(80, 85),
            cursor: None,
        };
        let x = *run(&mut cam, far, 60).last().unwrap();
        assert!(
            x + 9.0 <= 81.0 && 87.0 <= x + 55.0 - 9.0,
            "ink 80..85 centered in window at {x}"
        );
        let wide = Focus {
            ink: ink(3, 90),
            cursor: None,
        };
        assert_eq!(*run(&mut cam, wide, 90).last().unwrap(), 0.0);
    }

    #[test]
    fn same_frames_give_the_same_path() {
        let script: Vec<Focus> = (0..200u16)
            .map(|i| match i % 50 {
                0 => cursor(95),
                20 => Focus {
                    ink: ink(10, 12),
                    cursor: None,
                },
                35 => Focus {
                    ink: ink(0, 99),
                    cursor: cursor(4).cursor,
                },
                _ => Focus::default(),
            })
            .collect();
        let path = |mut cam: Camera| -> Vec<u64> {
            script
                .iter()
                .map(|f| {
                    cam.step(*f);
                    cam.x().to_bits()
                })
                .collect()
        };
        assert_eq!(path(camera()), path(camera()));
    }

    #[test]
    fn a_pixel_surface_follows_the_pointer_with_the_same_dead_zone() {
        // A 1920-wide screen scaled to 3271px, seen through a 1000px window.
        let mut cam = Camera::new(1000.0, 3271.0);
        let at = |x: f64| Focus {
            ink: None,
            cursor: Some((x, x)),
        };
        cam.place(at(1635.0));
        assert_eq!(
            cam.x(),
            1135.0,
            "first frame centers on the pointer with no glide"
        );
        assert_eq!(
            run(&mut cam, at(1400.0), 30),
            vec![1135.0; 30],
            "inside the dead zone"
        );
        let x = *run(&mut cam, at(3200.0), 90).last().unwrap();
        assert_eq!(x, 2271.0, "pinned to the right edge of the surface");
    }

    #[test]
    fn a_grid_no_wider_than_the_window_stays_put() {
        let mut cam = Camera::new(55.0, 50.0);
        assert_eq!(run(&mut cam, cursor(45), 10), vec![0.0; 10]);
    }

    #[test]
    fn focus_reports_new_ink_and_a_visible_cursor() {
        let mut p = vt100::Parser::new(5, 40, 0);
        p.process(b"hello");
        let before = p.screen().clone();
        assert_eq!(
            Focus::between(None, &before),
            Focus {
                ink: ink(0, 4),
                cursor: cursor(5).cursor,
            }
        );
        p.process(b"\x1b[3;20Hxy\x1b[?25l");
        assert_eq!(
            Focus::between(Some(&before), p.screen()),
            Focus {
                ink: ink(19, 20),
                cursor: None
            }
        );
        assert_eq!(
            Focus::between(Some(p.screen()), p.screen()),
            Focus::default()
        );
        p.process(b"\x1b[2J");
        assert_eq!(
            Focus::between(Some(&before), p.screen()).ink,
            None,
            "erasing adds no ink"
        );
    }
}
