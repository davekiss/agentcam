//! Rasterizes a vt100 screen onto an RGBA canvas.

use super::layout::{CellMetrics, Fit, Rect};
use super::theme::{Rgb, Theme};
use fontdue::{Font, FontSettings};
use std::collections::HashMap;

const REGULAR: &[u8] = include_bytes!("../../assets/JetBrainsMono-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../../assets/JetBrainsMono-Bold.ttf");
/// Tried in order for characters JetBrains Mono lacks: arrows, dingbats and misc symbols,
/// then braille and media controls, then the misc technical shapes Claude Code draws (⎿).
const FALLBACKS: [&[u8]; 3] = [
    include_bytes!("../../assets/DejaVuSansMono.ttf"),
    include_bytes!("../../assets/NotoSansSymbols2-Regular.ttf"),
    include_bytes!("../../assets/NotoSansSymbols-Regular.ttf"),
];

pub struct Fonts {
    regular: Font,
    bold: Font,
    fallbacks: Vec<Font>,
}

/// Where a character's outline lives. Index 0 is .notdef, so it never appears here.
struct Resolved<'a> {
    font: &'a Font,
    index: u16,
    fallback: bool,
}

impl Fonts {
    pub fn load() -> Fonts {
        let load = |bytes| Font::from_bytes(bytes, FontSettings::default()).expect("embedded font");
        Fonts {
            regular: load(REGULAR),
            bold: load(BOLD),
            fallbacks: FALLBACKS.into_iter().map(load).collect(),
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

    fn resolve(&self, c: char, bold: bool) -> Option<Resolved<'_>> {
        let primary = if bold { &self.bold } else { &self.regular };
        std::iter::once(primary)
            .chain(&self.fallbacks)
            .enumerate()
            .find_map(|(i, font)| {
                let index = font.lookup_glyph_index(c);
                (index != 0).then_some(Resolved {
                    font,
                    index,
                    fallback: i > 0,
                })
            })
    }
}

/// A rasterized glyph positioned relative to the top-left corner of its cell.
struct Glyph {
    x: i32,
    y: i32,
    width: usize,
    height: usize,
    coverage: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    c: char,
    bold: bool,
    span: u32,
}

pub struct Renderer<'a> {
    fonts: &'a Fonts,
    theme: &'a Theme,
    /// What to draw, each panel at the same cell size.
    panels: Vec<Fit>,
    dividers: Vec<Rect>,
    font_px: f32,
    cell_w: u32,
    cell_h: u32,
    width: u32,
    height: u32,
    baseline: i32,
    /// `None` for characters no embedded font has; they draw blank instead of as tofu.
    glyphs: HashMap<GlyphKey, Option<Glyph>>,
}

impl<'a> Renderer<'a> {
    pub fn new(fonts: &'a Fonts, theme: &'a Theme, fit: Fit, width: u32, height: u32) -> Self {
        Renderer::panels(fonts, theme, vec![fit], Vec::new(), width, height)
    }

    /// Draws several regions of the screen, each in its own panel, with a line in the
    /// theme's divider color across each of `dividers`.
    pub fn panels(
        fonts: &'a Fonts,
        theme: &'a Theme,
        panels: Vec<Fit>,
        dividers: Vec<Rect>,
        width: u32,
        height: u32,
    ) -> Self {
        let first = panels[0];
        let cell = |p: &Fit| (p.font_px, p.cell_w, p.cell_h);
        assert!(
            panels.iter().all(|p| cell(p) == cell(&first)),
            "panels share one cell size"
        );
        let line = fonts
            .regular
            .horizontal_line_metrics(first.font_px)
            .expect("horizontal metrics");
        let slack = first.cell_h as f32 - (line.ascent - line.descent);
        Renderer {
            fonts,
            theme,
            panels,
            dividers,
            font_px: first.font_px,
            cell_w: first.cell_w,
            cell_h: first.cell_h,
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
        for &d in &self.dividers {
            fill(buf, self.width, d, t.divider);
        }
        for i in 0..self.panels.len() {
            self.panel(screen, self.panels[i], buf);
        }
    }

    /// Draws the cells of `fit.region` and nothing outside it.
    fn panel(&mut self, screen: &vt100::Screen, fit: Fit, buf: &mut [u8]) {
        let t = self.theme;
        fill(buf, self.width, fit.panel, t.background);
        let cursor = (!screen.hide_cursor()).then(|| screen.cursor_position());
        let (cw, ch) = (self.cell_w, self.cell_h);
        let r = fit.region;
        for row in r.row..r.row + r.rows {
            for col in r.col..r.col + r.cols {
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
                    x: fit.grid_x + (col - r.col) as u32 * cw,
                    y: fit.grid_y + (row - r.row) as u32 * ch,
                    w: cw * span,
                    h: ch,
                };
                if bg != t.background {
                    fill(buf, self.width, rect, bg);
                }
                if let Some(c) = cell.contents().chars().next().filter(|c| !c.is_whitespace()) {
                    let key = GlyphKey {
                        c,
                        bold: cell.bold(),
                        span,
                    };
                    self.glyph(buf, key, rect, fg);
                }
                if cell.underline() {
                    let y = rect.y as i32 + self.baseline + (self.font_px / 8.0).ceil() as i32;
                    let thickness = (self.font_px / 14.0).ceil() as u32;
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

    fn glyph(&mut self, buf: &mut [u8], key: GlyphKey, cell: Rect, fg: Rgb) {
        if !self.glyphs.contains_key(&key) {
            let g = self.place(key);
            self.glyphs.insert(key, g);
        }
        let Some(g) = &self.glyphs[&key] else {
            return;
        };
        let x0 = cell.x as i32 + g.x;
        let y0 = cell.y as i32 + g.y;
        for gy in 0..g.height {
            let y = y0 + gy as i32;
            if y < 0 || y >= self.height as i32 {
                continue;
            }
            for gx in 0..g.width {
                let x = x0 + gx as i32;
                if x < 0 || x >= self.width as i32 {
                    continue;
                }
                let a = g.coverage[gy * g.width + gx];
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

    /// Primary glyphs sit on the shared baseline as designed. Fallback faces have their own
    /// proportions, so their glyphs shrink to fit the cell (two cells when wide), center
    /// horizontally, and keep the baseline unless that would push them out of the cell.
    fn place(&self, key: GlyphKey) -> Option<Glyph> {
        let Some(r) = self.fonts.resolve(key.c, key.bold) else {
            eprintln!(
                "rec: no embedded font has {:?} (U+{:04X}); drawing it blank",
                key.c, key.c as u32
            );
            return None;
        };
        let px = self.font_px;
        let (mut m, mut coverage) = r.font.rasterize_indexed(r.index, px);
        if !r.fallback {
            return Some(Glyph {
                x: m.xmin,
                y: self.baseline - m.height as i32 - m.ymin,
                width: m.width,
                height: m.height,
                coverage,
            });
        }
        let (box_w, box_h) = ((self.cell_w * key.span) as i32, self.cell_h as i32);
        let shrink = (box_w as f32 / m.width as f32)
            .min(box_h as f32 / m.height as f32)
            .min(1.0);
        if shrink < 1.0 {
            (m, coverage) = r.font.rasterize_indexed(r.index, px * shrink);
        }
        let (w, h) = (m.width as i32, m.height as i32);
        Some(Glyph {
            x: (box_w - w) / 2,
            y: (self.baseline - h - m.ymin).clamp(0, (box_h - h).max(0)),
            width: m.width,
            height: m.height,
            coverage,
        })
    }
}

pub(super) fn full(w: u32, h: u32) -> Rect {
    Rect { x: 0, y: 0, w, h }
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2])]
}

pub(super) fn fill(buf: &mut [u8], stride_px: u32, r: Rect, c: Rgb) {
    let px = [c[0], c[1], c[2], 255];
    for y in r.y..r.y + r.h {
        let start = ((y * stride_px + r.x) * 4) as usize;
        let end = start + (r.w * 4) as usize;
        for chunk in buf[start..end].as_chunks_mut::<4>().0 {
            chunk.copy_from_slice(&px);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{layout, theme};
    use crate::model::Size;

    /// What Claude Code and common TUIs draw that JetBrains Mono alone renders as tofu.
    const SYMBOLS: &str = "✔✗✘→↵⏺⏵⏸⎿⎯●▶★✻✳⣿⠋⠙⠹─│┌┐└┘├┤┬┴┼╭╮╰╯═║";

    #[test]
    fn every_symbol_resolves_to_a_real_glyph() {
        let fonts = Fonts::load();
        let missing: Vec<char> = SYMBOLS
            .chars()
            .filter(|&c| fonts.resolve(c, false).is_none() || fonts.resolve(c, true).is_none())
            .collect();
        assert!(missing.is_empty(), "no glyph for {missing:?}");
    }

    #[test]
    fn characters_no_font_has_resolve_to_nothing() {
        let fonts = Fonts::load();
        assert!(fonts.resolve('\u{E000}', false).is_none());
    }

    #[test]
    fn fallback_glyphs_fit_inside_their_cells() {
        let fonts = Fonts::load();
        let preset = layout::PRESETS[0];
        let grid = Size { cols: 100, rows: 30 };
        let fit = layout::fit(&preset, grid, fonts.cell_metrics(), None).unwrap();
        let theme = theme::by_name("dark").unwrap();
        let renderer = Renderer::new(&fonts, theme, fit, preset.width, preset.height);
        for c in "✔⏺⎿⣿↵".chars() {
            assert!(fonts.resolve(c, false).unwrap().fallback, "{c} should come from a fallback");
            for span in [1, 2] {
                let g = renderer
                    .place(GlyphKey { c, bold: false, span })
                    .expect("resolves");
                assert!(g.coverage.iter().any(|&a| a > 0), "{c} has no ink");
                assert!(
                    g.x >= 0
                        && g.y >= 0
                        && g.x + g.width as i32 <= (fit.cell_w * span) as i32
                        && g.y + g.height as i32 <= fit.cell_h as i32,
                    "{c} at ({}, {}) {}x{} overflows a {}x{} cell span",
                    g.x,
                    g.y,
                    g.width,
                    g.height,
                    fit.cell_w * span,
                    fit.cell_h
                );
            }
        }
    }
}
