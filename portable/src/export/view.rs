//! Turns emulator screens into canvas frames for one viewport.

use super::camera::{Camera, Focus};
use super::layout::{Preset, Rect, Viewport};
use super::render::{self, Fonts, Renderer};
use super::theme::Theme;

pub enum View<'a> {
    Fit(Renderer<'a>),
    Follow(Box<Follow<'a>>),
}

pub struct Follow<'a> {
    renderer: Renderer<'a>,
    canvas_color: [u8; 3],
    canvas_w: u32,
    /// The whole terminal at full scale, redrawn when the screen changes.
    surface: Vec<u8>,
    surface_w: u32,
    cell_w: u32,
    panel: Rect,
    camera: Camera,
    prev: Option<vt100::Screen>,
    /// The surface column, in pixels, at the window's left edge in the frame last drawn.
    shown: Option<u32>,
}

impl<'a> View<'a> {
    pub fn new(fonts: &'a Fonts, theme: &'a Theme, viewport: Viewport, preset: &Preset) -> Self {
        match viewport {
            Viewport::Fit(fit) => View::Fit(Renderer::new(
                fonts,
                theme,
                fit,
                preset.width,
                preset.height,
            )),
            Viewport::Stack(s) => View::Fit(Renderer::panels(
                fonts,
                theme,
                s.panels,
                s.dividers,
                preset.width,
                preset.height,
            )),
            Viewport::Follow(f) => {
                let s = f.surface;
                let renderer = Renderer::new(fonts, theme, s, s.panel.w, s.panel.h);
                View::Follow(Box::new(Follow {
                    surface: vec![0; renderer.frame_len()],
                    renderer,
                    canvas_color: theme.canvas,
                    canvas_w: preset.width,
                    surface_w: s.panel.w,
                    cell_w: s.cell_w,
                    panel: f.panel,
                    camera: Camera::new(
                        f.panel.w as f64 / s.cell_w as f64,
                        s.panel.w as f64 / s.cell_w as f64,
                    ),
                    prev: None,
                    shown: None,
                }))
            }
        }
    }

    /// Draws this frame into `canvas` if it differs from the last one drawn there, and says
    /// whether it did. `dirty` means the screen changed since the previous call.
    pub fn draw(&mut self, screen: &vt100::Screen, dirty: bool, canvas: &mut [u8]) -> bool {
        match self {
            View::Fit(renderer) => {
                if dirty {
                    renderer.render(screen, canvas);
                }
                dirty
            }
            View::Follow(f) => f.draw(screen, dirty, canvas),
        }
    }
}

impl Follow<'_> {
    fn draw(&mut self, screen: &vt100::Screen, dirty: bool, canvas: &mut [u8]) -> bool {
        let focus = if dirty {
            self.renderer.render(screen, &mut self.surface);
            let focus = Focus::between(self.prev.as_ref(), screen);
            self.prev = Some(screen.clone());
            focus
        } else {
            Focus::default()
        };
        self.camera.step(focus);
        let left = ((self.camera.x() * self.cell_w as f64).round() as u32)
            .min(self.surface_w - self.panel.w);
        if !dirty && self.shown == Some(left) {
            return false;
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
        let row_len = self.panel.w as usize * 4;
        for y in 0..self.panel.h {
            let src = ((y * self.surface_w + left) * 4) as usize;
            let dst = (((self.panel.y + y) * self.canvas_w + self.panel.x) * 4) as usize;
            canvas[dst..dst + row_len].copy_from_slice(&self.surface[src..src + row_len]);
        }
        self.shown = Some(left);
        true
    }
}
