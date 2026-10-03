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
}

pub const PRESETS: &[Preset] = &[
    Preset {
        aspect: "16:9",
        slug: "16x9",
        width: 1920,
        height: 1080,
        margin: 72,
    },
    Preset {
        aspect: "9:16",
        slug: "9x16",
        width: 1080,
        height: 1920,
        margin: 40,
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

/// Picks the largest whole-pixel cell height whose panel fits inside the canvas margin,
/// or uses `font_px` if given, and centers the panel.
pub fn fit(preset: &Preset, grid: Size, m: CellMetrics, font_px: Option<f32>) -> Result<Fit> {
    let avail_w = preset.width.saturating_sub(2 * preset.margin);
    let avail_h = preset.height.saturating_sub(2 * preset.margin);
    let fits = |px: f32| {
        let (cw, ch) = m.cell(px);
        let (pw, ph) = panel_size(grid, cw, ch);
        pw <= avail_w && ph <= avail_h
    };

    let font_px = match font_px {
        Some(px) if fits(px) => px,
        Some(px) => {
            return Err(RecError::new(
                "font_too_large",
                format!(
                    "a {}x{} grid at {px}px does not fit the {} layout",
                    grid.cols, grid.rows, preset.aspect
                ),
            ))
        }
        None => (1..=avail_h)
            .rev()
            .map(|cell_h| cell_h as f32 / m.line_height)
            .find(|&px| fits(px))
            .ok_or_else(|| {
                RecError::new(
                    "grid_too_large",
                    format!(
                        "a {}x{} grid cannot fit the {} layout",
                        grid.cols, grid.rows, preset.aspect
                    ),
                )
            })?,
    };

    let (cell_w, cell_h) = m.cell(font_px);
    let (pw, ph) = panel_size(grid, cell_w, cell_h);
    let panel = Rect {
        x: (preset.width - pw) / 2,
        y: (preset.height - ph) / 2,
        w: pw,
        h: ph,
    };
    Ok(Fit {
        font_px,
        cell_w,
        cell_h,
        grid_x: panel.x + cell_w,
        grid_y: panel.y + cell_h / 2,
        panel,
    })
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
}
