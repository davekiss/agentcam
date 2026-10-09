//! Layout presets as data, and the math that fits a terminal grid or a screen recording onto a
//! canvas.

use crate::error::{RecError, Result};
use crate::model::Size;
use serde::Serialize;

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
    /// The cells this panel shows.
    pub region: Region,
    pub font_px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    /// Top-left of the first cell.
    pub grid_x: u32,
    pub grid_y: u32,
    /// The terminal panel: the grid plus padding, drawn in the theme background.
    pub panel: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A rectangle of grid cells, as `--region COL,ROW,COLS,ROWS` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Region {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
}

impl Region {
    pub fn whole(grid: Size) -> Region {
        Region {
            col: 0,
            row: 0,
            cols: grid.cols,
            rows: grid.rows,
        }
    }

    fn size(&self) -> Size {
        Size {
            cols: self.cols,
            rows: self.rows,
        }
    }

    fn check(&self, grid: Size) -> Result<()> {
        let fits = |start: u16, len: u16, max: u16| start as u32 + len as u32 <= max as u32;
        if fits(self.col, self.cols, grid.cols) && fits(self.row, self.rows, grid.rows) {
            return Ok(());
        }
        let Region {
            col,
            row,
            cols,
            rows,
        } = *self;
        Err(RecError::new(
            "bad_args",
            format!(
                "region {col},{row},{cols},{rows} reaches past the {}x{} grid",
                grid.cols, grid.rows
            ),
        ))
    }
}

impl std::str::FromStr for Region {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let parts: Vec<u16> = s
            .split(',')
            .map(|p| p.trim().parse())
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| format!("expected COL,ROW,COLS,ROWS in cells, got {s:?}"))?;
        let [col, row, cols, rows] = parts[..] else {
            return Err(format!("expected COL,ROW,COLS,ROWS in cells, got {s:?}"));
        };
        if cols == 0 || rows == 0 {
            return Err(format!("region {s:?} is empty"));
        }
        Ok(Region {
            col,
            row,
            cols,
            rows,
        })
    }
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
        Region::whole(grid),
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
    fn at(region: Region, font_px: f32, cell_w: u32, cell_h: u32, panel: Rect) -> Fit {
        Fit {
            region,
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
#[derive(Debug, Clone, PartialEq)]
pub enum Viewport {
    /// The whole grid, scaled to fit and centered.
    Fit(Fit),
    /// Every row at full height, through a window narrower than the grid that pans across it.
    Follow(Follow),
    /// Chosen regions of the grid, one panel each, stacked top to bottom at one cell size.
    Stack(Stack),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stack {
    /// Top to bottom, every one at the same font size.
    pub panels: Vec<Fit>,
    /// A thin line centered in each gap between panels.
    pub dividers: Vec<Rect>,
}

/// Canvas pixels between stacked panels.
pub const STACK_GAP: u32 = 28;
const DIVIDER: u32 = 2;

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
            Viewport::Stack(s) => bounds(s.panels.iter().map(|p| p.panel)),
        }
    }

    /// How `agentcam export` reports it.
    pub fn kind(&self) -> &'static str {
        match self {
            Viewport::Fit(_) => "fit",
            Viewport::Follow(_) => "follow",
            Viewport::Stack(_) => "stack",
        }
    }

    pub fn font_px(&self) -> f32 {
        match self {
            Viewport::Fit(f) => f.font_px,
            Viewport::Follow(f) => f.surface.font_px,
            Viewport::Stack(s) => s.panels[0].font_px,
        }
    }
}

fn bounds(rects: impl Iterator<Item = Rect>) -> Rect {
    let [l, t, r, b] = rects.fold([u32::MAX, u32::MAX, 0, 0], |[l, t, r, b], p| {
        [l.min(p.x), t.min(p.y), r.max(p.x + p.w), b.max(p.y + p.h)]
    });
    Rect {
        x: l,
        y: t,
        w: r - l,
        h: b - t,
    }
}

/// Stacks `regions` when given. Otherwise fits the whole grid, unless the preset pans and the
/// fitted panel would leave most of the canvas empty; then the rows fill the height and a
/// window crops the columns.
pub fn viewport(
    preset: &Preset,
    grid: Size,
    regions: &[Region],
    m: CellMetrics,
    font_px: Option<f32>,
) -> Result<Viewport> {
    if !regions.is_empty() {
        return stack(preset, grid, regions, m, font_px).map(Viewport::Stack);
    }
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
        surface: Fit::at(
            Region::whole(grid),
            font_px,
            cell_w,
            cell_h,
            Rect {
                x: 0,
                y: 0,
                w: sw,
                h: sh,
            },
        ),
        panel: Rect {
            x: preset.margin,
            y: (preset.height - sh) / 2,
            w: avail_w,
            h: sh,
        },
    }))
}

/// Lays `regions` out top to bottom at the largest cell size where every panel fits the
/// width inside the margin and the panels plus the gaps between them fit the height. Each
/// panel is centered horizontally; the stack is centered vertically.
fn stack(
    preset: &Preset,
    grid: Size,
    regions: &[Region],
    m: CellMetrics,
    font_px: Option<f32>,
) -> Result<Stack> {
    for r in regions {
        r.check(grid)?;
    }
    let (avail_w, avail_h) = avail(preset);
    let gaps = STACK_GAP * (regions.len() as u32 - 1);
    let height = |ch: u32| -> u32 {
        regions
            .iter()
            .map(|r| panel_size(r.size(), 0, ch).1)
            .sum::<u32>()
            + gaps
    };
    let fits = |px: f32| {
        let (cw, ch) = m.cell(px);
        regions
            .iter()
            .all(|r| panel_size(r.size(), cw, ch).0 <= avail_w)
            && height(ch) <= avail_h
    };
    let no_fit = |code, at: Option<f32>| {
        let at = at.map(|px| format!(" at {px}px")).unwrap_or_default();
        RecError::new(
            code,
            format!(
                "these regions{at} do not stack inside the {} layout",
                preset.aspect
            ),
        )
    };
    let font_px = match font_px {
        Some(px) if fits(px) => px,
        Some(px) => return Err(no_fit("font_too_large", Some(px))),
        None => largest(m, avail_h, fits).ok_or_else(|| no_fit("grid_too_large", None))?,
    };
    let (cell_w, cell_h) = m.cell(font_px);
    let mut y = (preset.height - height(cell_h)) / 2;
    let panels: Vec<Fit> = regions
        .iter()
        .map(|&r| {
            let (w, h) = panel_size(r.size(), cell_w, cell_h);
            let panel = Rect {
                x: (preset.width - w) / 2,
                y,
                w,
                h,
            };
            y += h + STACK_GAP;
            Fit::at(r, font_px, cell_w, cell_h, panel)
        })
        .collect();
    let dividers = panels
        .windows(2)
        .map(|pair| {
            let span = bounds(pair.iter().map(|p| p.panel));
            Rect {
                y: pair[0].panel.y + pair[0].panel.h + (STACK_GAP - DIVIDER) / 2,
                h: DIVIDER,
                ..span
            }
        })
        .collect();
    Ok(Stack { panels, dividers })
}

/// The x11 screen, in pixels, that exactly fills the preset's canvas inside its margin, so a
/// take recorded `--for` that layout exports pixel for pixel with no scaling.
pub fn screen_for(preset: &Preset) -> (u32, u32) {
    let (w, h) = avail(preset);
    (w & !1, h & !1)
}

/// What part of a screen recording a layout shows, and where. The same rule as `viewport`:
/// fit whole unless the preset pans and fitting leaves most of the height empty.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScreenViewport {
    /// The whole screen, scaled into `panel`.
    Fit { panel: Rect },
    /// The screen scaled by `scale` so it fills the height, cropped by `panel` to a window
    /// that pans across a surface `surface_w` pixels wide.
    Follow {
        panel: Rect,
        scale: f64,
        surface_w: f64,
    },
}

impl ScreenViewport {
    pub fn panel(&self) -> Rect {
        match self {
            ScreenViewport::Fit { panel } | ScreenViewport::Follow { panel, .. } => *panel,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ScreenViewport::Fit { .. } => "fit",
            ScreenViewport::Follow { .. } => "follow",
        }
    }
}

pub fn screen_viewport(preset: &Preset, width: u32, height: u32) -> ScreenViewport {
    let (avail_w, avail_h) = avail(preset);
    let (w, h) = (width as f64, height as f64);
    let fit_scale = (avail_w as f64 / w).min(avail_h as f64 / h);
    let fit_h = (h * fit_scale).round() as u32;
    let fill_scale = avail_h as f64 / h;
    let pans = preset.pans
        && (fit_h as f32) < MIN_FIT_FILL * avail_h as f32
        && w * fill_scale > avail_w as f64;
    if pans {
        let ph = (h * fill_scale).round() as u32;
        return ScreenViewport::Follow {
            panel: Rect {
                x: preset.margin,
                y: (preset.height - ph) / 2,
                w: avail_w,
                h: ph,
            },
            scale: fill_scale,
            surface_w: w * fill_scale,
        };
    }
    let (pw, ph) = ((w * fit_scale).round() as u32, fit_h);
    ScreenViewport::Fit {
        panel: Rect {
            x: (preset.width - pw) / 2,
            y: (preset.height - ph) / 2,
            w: pw,
            h: ph,
        },
    }
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
        assert!(
            f.cell_w >= 10,
            "100 cols on 1920 should get >= 10px cells: {f:?}"
        );
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
            let Viewport::Fit(f) = viewport(p, g, &[], JBM, None).unwrap() else {
                panic!("{} grid {g:?} should fit {} uncropped", p.aspect, p.aspect);
            };
            assert_inside_margin(p, &f);
            let (aw, ah) = avail(p);
            assert!(
                aw - f.panel.w < f.cell_w,
                "{}: {f:?} leaves a column empty",
                p.aspect
            );
            assert!(
                ah - f.panel.h < f.cell_h,
                "{}: {f:?} leaves a row empty",
                p.aspect
            );
        }
    }

    #[test]
    fn vertical_crops_wide_grids_and_fits_tall_ones() {
        let p = preset("9:16").unwrap();
        let (aw, ah) = avail(&p);
        for g in [grid(100, 30), grid(80, 24), grid(200, 50)] {
            let Viewport::Follow(f) = viewport(&p, g, &[], JBM, None).unwrap() else {
                panic!("{g:?} should pan on 9:16");
            };
            assert_eq!(
                (f.panel.x, f.panel.w),
                (p.margin, aw),
                "{g:?} window spans the width"
            );
            assert_eq!(f.panel.h, f.surface.panel.h, "{g:?} window shows every row");
            assert!(
                ah - f.panel.h < f.surface.cell_h,
                "{g:?} rows fill the height: {f:?}"
            );
            assert!(
                f.surface.panel.w > f.panel.w,
                "{g:?} surface is wider than its window"
            );
            assert!(f.surface.grid_y + g.rows as u32 * f.surface.cell_h <= f.surface.panel.h);
        }
        for g in [grid(60, 40), grid(52, 45), grid(40, 60)] {
            assert!(
                matches!(viewport(&p, g, &[], JBM, None).unwrap(), Viewport::Fit(_)),
                "{g:?} should fit"
            );
        }
    }

    #[test]
    fn landscape_never_crops() {
        let p = preset("16:9").unwrap();
        for g in [grid(100, 30), grid(300, 30), grid(52, 45)] {
            assert_eq!(
                viewport(&p, g, &[], JBM, None).unwrap(),
                Viewport::Fit(fit(&p, g, JBM, None).unwrap())
            );
        }
    }

    #[test]
    fn a_screen_recorded_for_a_layout_fills_it_pixel_for_pixel() {
        for p in PRESETS {
            let (w, h) = screen_for(p);
            assert_eq!((w % 2, h % 2), (0, 0), "H.264 needs even sides");
            let ScreenViewport::Fit { panel } = screen_viewport(p, w, h) else {
                panic!("{} screen {w}x{h} should fit {}", p.aspect, p.aspect);
            };
            assert_eq!((panel.w, panel.h), (w, h), "{}: no scaling", p.aspect);
            assert_eq!(
                (panel.x, panel.y),
                (p.margin, p.margin),
                "{}: inside the margin",
                p.aspect
            );
        }
        assert_eq!(screen_for(&preset("9:16").unwrap()), (1000, 1840));
        assert_eq!(screen_for(&preset("16:9").unwrap()), (1776, 936));
    }

    #[test]
    fn a_landscape_screen_follows_on_vertical_and_fits_on_landscape() {
        let v = preset("9:16").unwrap();
        let ScreenViewport::Follow {
            panel,
            scale,
            surface_w,
        } = screen_viewport(&v, 1920, 1080)
        else {
            panic!("1920x1080 should pan on 9:16");
        };
        assert_eq!((panel.x, panel.w, panel.h), (40, 1000, 1840));
        assert!((scale - 1840.0 / 1080.0).abs() < 1e-9);
        assert!((surface_w - 1920.0 * scale).abs() < 1e-9);
        let l = preset("16:9").unwrap();
        let ScreenViewport::Fit { panel } = screen_viewport(&l, 1920, 1080) else {
            panic!("16:9 never pans");
        };
        assert_eq!((panel.w, panel.h), (1664, 936));
        assert!(matches!(
            screen_viewport(&v, 1080, 1920),
            ScreenViewport::Fit { .. }
        ));
    }

    #[test]
    fn explicit_font_size_on_vertical_crops_when_only_the_rows_fit() {
        let p = preset("9:16").unwrap();
        let Viewport::Follow(f) = viewport(&p, grid(100, 30), &[], JBM, Some(30.0)).unwrap() else {
            panic!("100 columns at 30px cannot fit 1000px");
        };
        assert_eq!(f.surface.font_px, 30.0);
        assert!(matches!(
            viewport(&p, grid(30, 20), &[], JBM, Some(30.0)).unwrap(),
            Viewport::Fit(_)
        ));
        assert_eq!(
            viewport(&p, grid(100, 30), &[], JBM, Some(80.0))
                .unwrap_err()
                .code,
            "font_too_large"
        );
    }

    fn region(col: u16, row: u16, cols: u16, rows: u16) -> Region {
        Region {
            col,
            row,
            cols,
            rows,
        }
    }

    fn stacked(p: &Preset, g: Size, regions: &[Region]) -> Stack {
        let Viewport::Stack(s) = viewport(p, g, regions, JBM, None).unwrap() else {
            panic!("regions should stack");
        };
        s
    }

    #[test]
    fn regions_parse_from_four_cell_counts_and_reject_empty_or_malformed() {
        assert_eq!("84,0,66,44".parse::<Region>(), Ok(region(84, 0, 66, 44)));
        for bad in [
            "84,0,66",
            "84,0,66,44,1",
            "a,0,66,44",
            "-1,0,66,44",
            "0,0,0,44",
            "0,0,66,0",
        ] {
            assert!(bad.parse::<Region>().is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_region_past_the_grid_is_bad_args() {
        let p = preset("9:16").unwrap();
        let g = grid(150, 44);
        for r in [
            region(84, 0, 67, 44),
            region(0, 1, 84, 44),
            region(150, 0, 1, 1),
        ] {
            assert_eq!(
                viewport(&p, g, &[r], JBM, None).unwrap_err().code,
                "bad_args",
                "{r:?}"
            );
        }
        assert!(viewport(
            &p,
            g,
            &[region(84, 0, 66, 44), region(0, 0, 84, 44)],
            JBM,
            None
        )
        .is_ok());
    }

    #[test]
    fn stacked_panels_share_one_cell_size_centered_with_even_gaps() {
        for p in PRESETS {
            let g = grid(150, 44);
            let s = stacked(p, g, &[region(84, 0, 66, 44), region(0, 30, 84, 14)]);
            let first = s.panels[0];
            assert_eq!(first.region, region(84, 0, 66, 44));
            for f in &s.panels {
                assert_eq!(
                    (f.font_px, f.cell_w, f.cell_h),
                    (first.font_px, first.cell_w, first.cell_h)
                );
                assert_inside_margin(p, f);
                assert!(
                    (f.panel.x as i64 - (p.width - f.panel.x - f.panel.w) as i64).abs() <= 1,
                    "{f:?} centered"
                );
                assert_eq!(f.panel.w, (f.region.cols as u32 + 2) * f.cell_w);
                assert_eq!(f.panel.h, (f.region.rows as u32 + 1) * f.cell_h);
            }
            let [a, b] = [s.panels[0].panel, s.panels[1].panel];
            assert_eq!(
                b.y,
                a.y + a.h + STACK_GAP,
                "{}: one gap between panels",
                p.aspect
            );
            let below = p.height - (b.y + b.h);
            assert!(
                (a.y as i64 - below as i64).abs() <= 1,
                "{}: stack centered vertically",
                p.aspect
            );
            let [d] = s.dividers[..] else {
                panic!("one divider")
            };
            assert!(
                d.y > a.y + a.h && d.y + d.h < b.y,
                "{}: divider inside the gap",
                p.aspect
            );
        }
    }

    #[test]
    fn stack_scale_is_the_largest_bound_by_height_or_width() {
        let p = preset("9:16").unwrap();
        let (aw, ah) = avail(&p);
        let tall = stacked(
            &p,
            grid(150, 44),
            &[region(84, 0, 66, 44), region(0, 0, 84, 44)],
        );
        let [a, b] = [tall.panels[0].panel, tall.panels[1].panel];
        assert!(
            ah - (a.h + b.h + STACK_GAP) < 2 * a.h / 45,
            "two 44-row panels fill the height: {tall:?}"
        );
        assert!(a.w.max(b.w) < aw, "height binds, not width");
        let next = (tall.panels[0].cell_h + 1) as f32 / JBM.line_height;
        let regions = [region(84, 0, 66, 44), region(0, 0, 84, 44)];
        assert_eq!(
            viewport(&p, grid(150, 44), &regions, JBM, Some(next))
                .unwrap_err()
                .code,
            "font_too_large"
        );

        let strips = [region(0, 0, 150, 4), region(0, 40, 150, 4)];
        let f = stacked(&p, grid(150, 44), &strips).panels[0];
        assert!(
            f.panel.h * 2 + STACK_GAP < ah / 2,
            "width binds, not height"
        );
        let next = (f.cell_h + 1) as f32 / JBM.line_height;
        let Err(e) = viewport(&p, grid(150, 44), &strips, JBM, Some(next)) else {
            panic!("one pixel taller than {f:?} should overflow the width");
        };
        assert_eq!(e.code, "font_too_large");
    }

    #[test]
    fn stack_reports_the_bounds_of_its_panels() {
        let p = preset("9:16").unwrap();
        let v = viewport(
            &p,
            grid(150, 44),
            &[region(84, 0, 66, 20), region(0, 0, 84, 20)],
            JBM,
            None,
        )
        .unwrap();
        let Viewport::Stack(s) = &v else { panic!() };
        let r = v.panel();
        assert_eq!(v.kind(), "stack");
        assert_eq!(
            (r.x, r.w),
            (s.panels[1].panel.x, s.panels[1].panel.w),
            "widest panel sets the width"
        );
        assert_eq!(
            (r.y, r.h),
            (
                s.panels[0].panel.y,
                s.panels[1].panel.y + s.panels[1].panel.h - s.panels[0].panel.y
            )
        );
    }
}
