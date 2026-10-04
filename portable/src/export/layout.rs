//! Layout presets as data, and the math that fits a terminal grid onto a canvas.

use crate::error::{RecError, Result};
use crate::model::Size;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// What users pass to `--layout`.
    pub aspect: &'static str,
    /// Used in the output file name, `export-<slug>.mp4`.
    pub slug: &'static str,
    pub width: u32,
    pub height: u32,
    pub margin: u32,
    /// The width a take recorded with `--for <aspect>` gets; its rows fill the rest.
    pub record_cols: u16,
    /// Whether a grid too wide to fill this canvas is cropped to a window that follows the
    /// action, instead of shrunk until the whole grid fits.
    pub pans: bool,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        aspect: "16:9",
        slug: "16x9",
        width: 1920,
        height: 1080,
        margin: 72,
        record_cols: 120,
        pans: false,
    },
    Preset {
        aspect: "9:16",
        slug: "9x16",
        width: 1080,
        height: 1920,
        margin: 40,
        record_cols: 52,
        pans: true,
    },
];

pub fn preset(aspect: &str) -> Result<Preset> {
    PRESETS
        .iter()
        .copied()
        .find(|p| p.aspect == aspect || p.slug == aspect)
        .ok_or_else(|| {
            let known: Vec<_> = PRESETS.iter().map(|p| p.aspect).collect();
            RecError::new(
                "bad_layout",
                format!("unknown layout {aspect:?}; known: {}", known.join(", ")),
            )
        })
}

/// Font metrics for a 1px font: multiply by the pixel size to get real cell dimensions.
#[derive(Debug, Clone, Copy)]
pub struct CellMetrics {
    pub advance: f32,
    pub line_height: f32,
}

impl CellMetrics {
    fn cell(&self, font_px: f32) -> (u32, u32) {
        (
            (font_px * self.advance).round().max(1.0) as u32,
            (font_px * self.line_height).ceil().max(1.0) as u32,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub font_px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    /// Top-left of the first cell.
    pub grid_x: u32,
    pub grid_y: u32,
    /// The terminal panel: the grid plus padding, drawn in the theme background.
    pub panel: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Padding around the grid inside the panel, in cells: one column each side, half a row
/// top and bottom.
fn panel_size(grid: Size, cell_w: u32, cell_h: u32) -> (u32, u32) {
    (
        (grid.cols as u32 + 2) * cell_w,
        grid.rows as u32 * cell_h + cell_h,
    )
}

/// A fitted panel shorter than this share of the available height leaves the frame mostly
/// empty, so a panning preset crops instead.
const MIN_FIT_FILL: f32 = 0.75;

fn avail(preset: &Preset) -> (u32, u32) {
    (
        preset.width.saturating_sub(2 * preset.margin),
        preset.height.saturating_sub(2 * preset.margin),
    )
}

/// The font size of the tallest whole-pixel cell, at most `max_cell_h`, that `fits`.
fn largest(m: CellMetrics, max_cell_h: u32, fits: impl Fn(f32) -> bool) -> Option<f32> {
    (1..=max_cell_h)
        .rev()
        .map(|cell_h| cell_h as f32 / m.line_height)
        .find(|&px| fits(px))
}

fn too_large(code: &str, grid: Size, preset: &Preset, at: Option<f32>) -> RecError {
    let at = at.map(|px| format!(" at {px}px")).unwrap_or_default();
    RecError::new(
        code,
        format!(
            "a {}x{} grid{at} does not fit the {} layout",
            grid.cols, grid.rows, preset.aspect
        ),
    )
}

/// The grid whose panel fills the preset's canvas inside the margin, at the cell size that
/// makes it `record_cols` wide. Exporting it to the same preset fits with no crop.
pub fn grid_for(preset: &Preset, m: CellMetrics) -> Size {
    let (avail_w, avail_h) = avail(preset);
    let px = largest(m, avail_h, |px| {
        (preset.record_cols as u32 + 2) * m.cell(px).0 <= avail_w
    })
    .expect("every preset fits its own record_cols");
    let (cw, ch) = m.cell(px);
    Size {
        cols: (avail_w / cw - 2) as u16,
        rows: (avail_h / ch - 1) as u16,
    }
}

/// Picks the largest whole-pixel cell height whose panel fits inside the canvas margin,
/// or uses `font_px` if given, and centers the panel.
pub fn fit(preset: &Preset, grid: Size, m: CellMetrics, font_px: Option<f32>) -> Result<Fit> {
    let (avail_w, avail_h) = avail(preset);
    let fits = |px: f32| {
        let (cw, ch) = m.cell(px);
        let (pw, ph) = panel_size(grid, cw, ch);
        pw <= avail_w && ph <= avail_h
    };
    let font_px = match font_px {
        Some(px) if fits(px) => px,
        Some(px) => return Err(too_large("font_too_large", grid, preset, Some(px))),
        None => largest(m, avail_h, fits)
            .ok_or_else(|| too_large("grid_too_large", grid, preset, None))?,
    };
    let (cell_w, cell_h) = m.cell(font_px);
    let (pw, ph) = panel_size(grid, cell_w, cell_h);
    Ok(Fit::at(
        font_px,
        cell_w,
        cell_h,
        Rect {
            x: (preset.width - pw) / 2,
            y: (preset.height - ph) / 2,
            w: pw,
            h: ph,
        },
    ))
}

impl Fit {
    fn at(font_px: f32, cell_w: u32, cell_h: u32, panel: Rect) -> Fit {
        Fit {
            font_px,
            cell_w,
            cell_h,
            grid_x: panel.x + cell_w,
            grid_y: panel.y + cell_h / 2,
            panel,
        }
    }
}

/// What part of the terminal a layout shows, and where.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Viewport {
    /// The whole grid, scaled to fit and centered.
    Fit(Fit),
    /// Every row at full height, through a window narrower than the grid that pans across it.
    Follow(Follow),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Follow {
    /// The whole terminal laid out on a surface of its own, panel at the origin.
    pub surface: Fit,
    /// The window onto the surface, in canvas pixels: as tall as the surface, narrower than it.
    pub panel: Rect,
}

impl Viewport {
    /// The terminal panel as it appears on the canvas.
    pub fn panel(&self) -> Rect {
        match self {
            Viewport::Fit(f) => f.panel,
            Viewport::Follow(f) => f.panel,
        }
    }

    /// How `rec export` reports it.
    pub fn kind(&self) -> &'static str {
        match self {
            Viewport::Fit(_) => "fit",
            Viewport::Follow(_) => "follow",
        }
    }

    pub fn font_px(&self) -> f32 {
        match self {
            Viewport::Fit(f) => f.font_px,
            Viewport::Follow(f) => f.surface.font_px,
        }
    }
}

/// Fits the whole grid, unless the preset pans and the fitted panel would leave most of the
/// canvas empty. Then the rows fill the height and a window crops the columns.
pub fn viewport(
    preset: &Preset,
    grid: Size,
    m: CellMetrics,
    font_px: Option<f32>,
) -> Result<Viewport> {
    let whole = fit(preset, grid, m, font_px);
    if !preset.pans {
        return whole.map(Viewport::Fit);
    }
    let (avail_w, avail_h) = avail(preset);
    if let Ok(f) = whole {
        if font_px.is_some() || f.panel.h as f32 >= MIN_FIT_FILL * avail_h as f32 {
            return Ok(Viewport::Fit(f));
        }
    }
    let rows_fit = |px: f32| panel_size(grid, 0, m.cell(px).1).1 <= avail_h;
    let font_px = match font_px {
        Some(px) if rows_fit(px) => px,
        Some(px) => return Err(too_large("font_too_large", grid, preset, Some(px))),
        None => largest(m, avail_h, rows_fit)
            .ok_or_else(|| too_large("grid_too_large", grid, preset, None))?,
    };
    let (cell_w, cell_h) = m.cell(font_px);
    let (sw, sh) = panel_size(grid, cell_w, cell_h);
    if sw <= avail_w {
        return fit(preset, grid, m, Some(font_px)).map(Viewport::Fit);
    }
    Ok(Viewport::Follow(Follow {
        surface: Fit::at(font_px, cell_w, cell_h, Rect { x: 0, y: 0, w: sw, h: sh }),
        panel: Rect {
            x: preset.margin,
            y: (preset.height - sh) / 2,
            w: avail_w,
            h: sh,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const JBM: CellMetrics = CellMetrics {
        advance: 0.6,
        line_height: 1.32,
    };

    fn grid(cols: u16, rows: u16) -> Size {
        Size { cols, rows }
    }

    fn assert_inside_margin(p: &Preset, f: &Fit) {
        let r = f.panel;
        assert!(r.x >= p.margin && r.y >= p.margin, "{f:?}");
        assert!(r.x + r.w <= p.width - p.margin, "{f:?}");
        assert!(r.y + r.h <= p.height - p.margin, "{f:?}");
    }

    #[test]
    fn panel_is_centered_inside_the_margin_for_every_preset() {
        for p in PRESETS {
            for g in [grid(100, 30), grid(60, 40), grid(200, 50), grid(80, 24)] {
                let f = fit(p, g, JBM, None).unwrap();
                assert_inside_margin(p, &f);
                let r = f.panel;
                assert!((r.x as i64 - (p.width - r.x - r.w) as i64).abs() <= 1);
                assert!((r.y as i64 - (p.height - r.y - r.h) as i64).abs() <= 1);
                assert!(f.grid_x + g.cols as u32 * f.cell_w <= r.x + r.w);
                assert!(f.grid_y + g.rows as u32 * f.cell_h <= r.y + r.h);
            }
        }
    }

    #[test]
    fn auto_fit_is_the_largest_that_fits() {
        let p = preset("16:9").unwrap();
        let f = fit(&p, grid(100, 30), JBM, None).unwrap();
        let bigger = (f.cell_h + 1) as f32 / JBM.line_height;
        assert!(fit(&p, grid(100, 30), JBM, Some(bigger)).is_err());
        assert!(f.cell_w >= 10, "100 cols on 1920 should get >= 10px cells: {f:?}");
    }

    #[test]
    fn narrow_grids_get_bigger_cells_on_vertical_layouts() {
        let p = preset("9:16").unwrap();
        let wide = fit(&p, grid(120, 36), JBM, None).unwrap();
        let narrow = fit(&p, grid(60, 40), JBM, None).unwrap();
        assert!(narrow.cell_w > wide.cell_w);
    }

    #[test]
    fn explicit_font_size_is_honored_or_rejected() {
        let p = preset("16:9").unwrap();
        let f = fit(&p, grid(80, 24), JBM, Some(20.0)).unwrap();
        assert_eq!((f.cell_w, f.cell_h), (12, 27));
        assert_eq!(
            fit(&p, grid(80, 24), JBM, Some(80.0)).unwrap_err().code,
            "font_too_large"
        );
    }

    #[test]
    fn layout_lookup_accepts_aspect_or_slug() {
        assert_eq!(preset("16:9").unwrap().width, 1920);
        assert_eq!(preset("9x16").unwrap().height, 1920);
        assert_eq!(preset("4:3").unwrap_err().code, "bad_layout");
    }

    #[test]
    fn grid_for_an_aspect_fills_that_canvas_within_one_cell_and_fits_uncropped() {
        for p in PRESETS {
            let g = grid_for(p, JBM);
            assert!(g.cols >= p.record_cols, "{} got {g:?}", p.aspect);
            let Viewport::Fit(f) = viewport(p, g, JBM, None).unwrap() else {
                panic!("{} grid {g:?} should fit {} uncropped", p.aspect, p.aspect);
            };
            assert_inside_margin(p, &f);
            let (aw, ah) = avail(p);
            assert!(aw - f.panel.w < f.cell_w, "{}: {f:?} leaves a column empty", p.aspect);
            assert!(ah - f.panel.h < f.cell_h, "{}: {f:?} leaves a row empty", p.aspect);
        }
    }

    #[test]
    fn vertical_crops_wide_grids_and_fits_tall_ones() {
        let p = preset("9:16").unwrap();
        let (aw, ah) = avail(&p);
        for g in [grid(100, 30), grid(80, 24), grid(200, 50)] {
            let Viewport::Follow(f) = viewport(&p, g, JBM, None).unwrap() else {
                panic!("{g:?} should pan on 9:16");
            };
            assert_eq!((f.panel.x, f.panel.w), (p.margin, aw), "{g:?} window spans the width");
            assert_eq!(f.panel.h, f.surface.panel.h, "{g:?} window shows every row");
            assert!(ah - f.panel.h < f.surface.cell_h, "{g:?} rows fill the height: {f:?}");
            assert!(f.surface.panel.w > f.panel.w, "{g:?} surface is wider than its window");
            assert!(f.surface.grid_y + g.rows as u32 * f.surface.cell_h <= f.surface.panel.h);
        }
        for g in [grid(60, 40), grid(52, 45), grid(40, 60)] {
            assert!(matches!(viewport(&p, g, JBM, None).unwrap(), Viewport::Fit(_)), "{g:?} should fit");
        }
    }

    #[test]
    fn landscape_never_crops() {
        let p = preset("16:9").unwrap();
        for g in [grid(100, 30), grid(300, 30), grid(52, 45)] {
            assert_eq!(viewport(&p, g, JBM, None).unwrap(), Viewport::Fit(fit(&p, g, JBM, None).unwrap()));
        }
    }

    #[test]
    fn explicit_font_size_on_vertical_crops_when_only_the_rows_fit() {
        let p = preset("9:16").unwrap();
        let Viewport::Follow(f) = viewport(&p, grid(100, 30), JBM, Some(30.0)).unwrap() else {
            panic!("100 columns at 30px cannot fit 1000px");
        };
        assert_eq!(f.surface.font_px, 30.0);
        assert!(matches!(viewport(&p, grid(30, 20), JBM, Some(30.0)).unwrap(), Viewport::Fit(_)));
        assert_eq!(viewport(&p, grid(100, 30), JBM, Some(80.0)).unwrap_err().code, "font_too_large");
    }
}
