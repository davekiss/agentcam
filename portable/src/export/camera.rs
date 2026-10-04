//! The pan camera for a cropped viewport: where the window onto a wide terminal sits each
//! frame. Its position is a pure function of the frames it has seen, so re-exports match.

use super::FPS;

/// What one frame says is worth looking at, in grid columns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Focus {
    /// The leftmost and rightmost columns that gained ink since the previous frame.
    pub ink: Option<(u16, u16)>,
    /// The cursor's column, when the program shows it.
    pub cursor: Option<u16>,
}

impl Focus {
    /// Compares two screens of the same size. With no previous screen, all ink is new.
    pub fn between(prev: Option<&vt100::Screen>, now: &vt100::Screen) -> Focus {
        let (rows, cols) = now.size();
        let mut ink: Option<(u16, u16)> = None;
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = now.cell(row, col) else {
                    continue;
                };
                let inked = cell.contents().chars().any(|c| !c.is_whitespace());
                if inked && prev.and_then(|p| p.cell(row, col)) != Some(cell) {
                    ink = Some(ink.map_or((col, col), |(lo, hi)| (lo.min(col), hi.max(col))));
                }
            }
        }
        Focus {
            ink,
            cursor: (!now.hide_cursor()).then(|| now.cursor_position().1),
        }
    }
}

/// Angular frequency of the critically damped spring: it settles in about half a second.
const OMEGA: f64 = 8.0;
const SETTLED: f64 = 0.01;

/// Positions are in surface columns: the grid plus one column of panel padding each side, so
/// grid column `c` spans `c + 1 .. c + 2`.
#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    /// How many surface columns the window shows.
    window: f64,
    /// The largest left edge, which puts the window against the right edge of the surface.
    max: f64,
    /// Focus within this many columns of the window edge counts as leaving the window.
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

    /// The window's left edge in surface columns.
    pub fn x(&self) -> f64 {
        self.x
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
            let (lo, hi) = (lo as f64 + 1.0, hi as f64 + 2.0);
            if !self.comfortable(target, lo, hi) {
                target = if hi - lo <= self.window - 2.0 * self.margin {
                    (lo + hi - self.window) / 2.0
                } else {
                    lo - self.margin
                };
                target = target.clamp(0.0, self.max);
            }
        }
        if let Some(c) = focus.cursor {
            let (lo, hi) = (c as f64 + 1.0, c as f64 + 2.0);
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
            cursor: Some(c),
        }
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
                ink: Some((c - 1, c - 1)),
                cursor: Some(c),
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
                ink: Some((0, 99)),
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
            ink: Some((80, 85)),
            cursor: None,
        };
        let x = *run(&mut cam, far, 60).last().unwrap();
        assert!(
            x + 9.0 <= 81.0 && 87.0 <= x + 55.0 - 9.0,
            "ink 80..85 centered in window at {x}"
        );
        let wide = Focus {
            ink: Some((3, 90)),
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
                    ink: Some((10, 12)),
                    cursor: None,
                },
                35 => Focus {
                    ink: Some((0, 99)),
                    cursor: Some(4),
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
                ink: Some((0, 4)),
                cursor: Some(5)
            }
        );
        p.process(b"\x1b[3;20Hxy\x1b[?25l");
        assert_eq!(
            Focus::between(Some(&before), p.screen()),
            Focus {
                ink: Some((19, 20)),
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
