//! Rasterizes a vt100 screen onto an RGBA canvas.

use super::layout::{CellMetrics, Fit, Rect};
use super::theme::{Rgb, Theme};
use fontdue::{Font, FontSettings, Metrics};
use std::collections::HashMap;

const REGULAR: &[u8] = include_bytes!("../../assets/JetBrainsMono-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../../assets/JetBrainsMono-Bold.ttf");

pub struct Fonts {
    regular: Font,
    bold: Font,
}

impl Fonts {
    pub fn load() -> Fonts {
        let load = |bytes| Font::from_bytes(bytes, FontSettings::default()).expect("embedded font");
        Fonts {
            regular: load(REGULAR),
            bold: load(BOLD),
        }
    }

    pub fn cell_metrics(&self) -> CellMetrics {
        let line = self
            .regular
            .horizontal_line_metrics(1.0)
            .expect("horizontal metrics");
        CellMetrics {
            advance: self.regular.metrics('M', 1.0).advance_width,
            line_height: line.ascent - line.descent,
        }
    }
}

pub struct Renderer<'a> {
    fonts: &'a Fonts,
    theme: &'a Theme,
    fit: Fit,
    width: u32,
    height: u32,
    baseline: i32,
    glyphs: HashMap<(char, bool), (Metrics, Vec<u8>)>,
}

impl<'a> Renderer<'a> {
    pub fn new(fonts: &'a Fonts, theme: &'a Theme, fit: Fit, width: u32, height: u32) -> Self {
        let line = fonts
            .regular
            .horizontal_line_metrics(fit.font_px)
            .expect("horizontal metrics");
        let slack = fit.cell_h as f32 - (line.ascent - line.descent);
        Renderer {
            fonts,
            theme,
            fit,
            width,
            height,
            baseline: (line.ascent + slack / 2.0).round() as i32,
            glyphs: HashMap::new(),
        }
    }

    pub fn frame_len(&self) -> usize {
        (self.width * self.height * 4) as usize
    }

    pub fn render(&mut self, screen: &vt100::Screen, buf: &mut [u8]) {
        let t = self.theme;
        fill(buf, self.width, full(self.width, self.height), t.canvas);
        fill(buf, self.width, self.fit.panel, t.background);

        let (rows, cols) = screen.size();
        let cursor = (!screen.hide_cursor()).then(|| screen.cursor_position());
        let (cw, ch) = (self.fit.cell_w, self.fit.cell_h);
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = t.color(cell.fgcolor(), t.foreground);
                let mut bg = t.color(cell.bgcolor(), t.background);
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cursor == Some((row, col)) {
                    bg = t.cursor;
                    fg = t.background;
                }
                if cell.dim() {
                    fg = mix(bg, fg, 0.5);
                }
                let span = if cell.is_wide() { 2 } else { 1 };
                let rect = Rect {
                    x: self.fit.grid_x + col as u32 * cw,
                    y: self.fit.grid_y + row as u32 * ch,
                    w: cw * span,
                    h: ch,
                };
                if bg != t.background {
                    fill(buf, self.width, rect, bg);
                }
                if let Some(c) = cell.contents().chars().next().filter(|c| !c.is_whitespace()) {
                    self.glyph(buf, c, cell.bold(), rect, fg);
                }
                if cell.underline() {
                    let y = rect.y as i32 + self.baseline + (self.fit.font_px / 8.0).ceil() as i32;
                    let thickness = (self.fit.font_px / 14.0).ceil() as u32;
                    if y >= 0 {
                        let line = Rect {
                            y: (y as u32).min(rect.y + rect.h - thickness),
                            h: thickness,
                            ..rect
                        };
                        fill(buf, self.width, line, fg);
                    }
                }
            }
        }
    }

    fn glyph(&mut self, buf: &mut [u8], c: char, bold: bool, cell: Rect, fg: Rgb) {
        let fonts = self.fonts;
        let px = self.fit.font_px;
        let (m, bitmap) = self.glyphs.entry((c, bold)).or_insert_with(|| {
            let font = if bold { &fonts.bold } else { &fonts.regular };
            font.rasterize(c, px)
        });
        let x0 = cell.x as i32 + m.xmin;
        let y0 = cell.y as i32 + self.baseline - m.height as i32 - m.ymin;
        for gy in 0..m.height {
            let y = y0 + gy as i32;
            if y < 0 || y >= self.height as i32 {
                continue;
            }
            for gx in 0..m.width {
                let x = x0 + gx as i32;
                if x < 0 || x >= self.width as i32 {
                    continue;
                }
                let a = bitmap[gy * m.width + gx];
                if a == 0 {
                    continue;
                }
                let i = (y as usize * self.width as usize + x as usize) * 4;
                let under = [buf[i], buf[i + 1], buf[i + 2]];
                let px = mix(under, fg, a as f32 / 255.0);
                buf[i..i + 3].copy_from_slice(&px);
            }
        }
    }
}

fn full(w: u32, h: u32) -> Rect {
    Rect { x: 0, y: 0, w, h }
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2])]
}

fn fill(buf: &mut [u8], stride_px: u32, r: Rect, c: Rgb) {
    let px = [c[0], c[1], c[2], 255];
    for y in r.y..r.y + r.h {
        let start = ((y * stride_px + r.x) * 4) as usize;
        let end = start + (r.w * 4) as usize;
        for chunk in buf[start..end].chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }
}
