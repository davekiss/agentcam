//! The on-disk contract from SPEC.md: take.json v2, timeline.json v2, markers.jsonl, active.json.

use crate::error::{RecError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const TAKE_FILE: &str = "take.json";
pub const CAST_FILE: &str = "term.cast";
pub const TIMELINE_FILE: &str = "timeline.json";
pub const MARKERS_FILE: &str = "markers.jsonl";
pub const LOG_FILE: &str = "recorder.log";
pub const SOCKET_FILE: &str = "control.sock";
pub const SCREEN_FILE: &str = "screen.mp4";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl std::str::FromStr for Size {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let (c, r) = s
            .split_once(['x', 'X'])
            .ok_or_else(|| format!("expected COLSxROWS, got {s:?}"))?;
        let cols: u16 = c.parse().map_err(|_| format!("bad cols in {s:?}"))?;
        let rows: u16 = r.parse().map_err(|_| format!("bad rows in {s:?}"))?;
        if cols < 2 || rows < 2 {
            return Err(format!("size {s:?} is too small"));
        }
        Ok(Size { cols, rows })
    }
}

/// `WIDTHxHEIGHT` as typed on the command line: a tty grid or an x11 screen in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dims {
    pub w: u32,
    pub h: u32,
}

impl std::str::FromStr for Dims {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let (w, h) = s
            .split_once(['x', 'X'])
            .ok_or_else(|| format!("expected WIDTHxHEIGHT, got {s:?}"))?;
        let w = w.parse().map_err(|_| format!("bad width in {s:?}"))?;
        let h = h.parse().map_err(|_| format!("bad height in {s:?}"))?;
        Ok(Dims { w, h })
    }
}

impl TryFrom<Dims> for Size {
    type Error = String;

    fn try_from(d: Dims) -> std::result::Result<Size, String> {
        format!("{}x{}", d.w, d.h).parse()
    }
}

/// An x11 screen. Always at the origin: `rec` owns the whole virtual display.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Frame {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl TryFrom<Dims> for Frame {
    type Error = String;

    /// H.264 in yuv420p needs even sides.
    fn try_from(d: Dims) -> std::result::Result<Frame, String> {
        if d.w < 64 || d.h < 64 || d.w > 7680 || d.h > 7680 {
            return Err(format!("screen {}x{} must be 64..=7680 pixels a side", d.w, d.h));
        }
        if d.w % 2 == 1 || d.h % 2 == 1 {
            return Err(format!("screen {}x{} needs even sides for H.264", d.w, d.h));
        }
        Ok(Frame {
            x: 0,
            y: 0,
            width: d.w,
            height: d.h,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    Tty {
        command: Vec<String>,
        size: Size,
    },
    X11 {
        /// The app `rec` launched into the display, if any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        command: Vec<String>,
        frame: Frame,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Track {
    Term {
        file: String,
        offset: f64,
    },
    Screen {
        file: String,
        offset: f64,
        width: u32,
        height: u32,
        /// Takes from before this field captured the pointer into the video.
        #[serde(default)]
        pointer: PointerCapture,
    },
}

/// Where a screen track's pointer is: drawn into the video by the capture, or only in the
/// timeline, for export to draw.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PointerCapture {
    #[default]
    Baked,
    Timeline,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    /// The X11 core protocol button number.
    pub fn x11(self) -> u8 {
        match self {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 3,
        }
    }
}

impl std::str::FromStr for Button {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "left" => Ok(Button::Left),
            "middle" => Ok(Button::Middle),
            "right" => Ok(Button::Right),
            _ => Err(format!("expected left, middle or right, got {s:?}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TakeStatus {
    Recording,
    Finished,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Take {
    pub version: u32,
    pub id: String,
    pub created_at: String,
    pub status: TakeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    pub source: Source,
    pub tracks: Vec<Track>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RecError>,
}

impl Take {
    pub fn read(dir: &Path) -> Result<Take> {
        let path = dir.join(TAKE_FILE);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| RecError::new("not_found", format!("{}: {e}", path.display())))?;
        serde_json::from_str(&raw)
            .map_err(|e| RecError::new("bad_take", format!("{}: {e}", path.display())))
    }

    pub fn write(&self, dir: &Path) -> Result<()> {
        write_json_atomic(&dir.join(TAKE_FILE), self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    Type {
        text: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        redacted: bool,
    },
    Key {
        key: String,
    },
    Marker {
        label: String,
    },
    /// `x` and `y` are normalized 0..1 to the source frame, top-left origin.
    Cursor {
        x: f64,
        y: f64,
    },
    Click {
        x: f64,
        y: f64,
        button: Button,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimedEvent {
    pub t: f64,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Timeline {
    pub version: u32,
    pub events: Vec<TimedEvent>,
}

impl Timeline {
    pub fn read(dir: &Path) -> Result<Timeline> {
        let path = dir.join(TIMELINE_FILE);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| RecError::new("not_found", format!("{}: {e}", path.display())))?;
        serde_json::from_str(&raw)
            .map_err(|e| RecError::new("bad_timeline", format!("{}: {e}", path.display())))
    }
}

/// One line of markers.jsonl.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Marker {
    pub t: f64,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Active {
    pub pid: u32,
    pub take: PathBuf,
    pub started_at: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

/// A pixel position as a 0..1 fraction of `extent`, rounded to six places.
pub fn normalize(px: f64, extent: u32) -> f64 {
    round_t(px / extent as f64)
}

/// Seconds rounded to microseconds, so JSON stays short and stable.
pub fn round_t(t: f64) -> f64 {
    (t * 1e6).round() / 1e6
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut body = serde_json::to_string_pretty(value).expect("serializable");
    body.push('\n');
    std::fs::write(&tmp, body).map_err(|e| RecError::io(&tmp.display().to_string(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| RecError::io(&path.display().to_string(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_type_event_keeps_null_text() {
        let e = TimedEvent {
            t: 2.9,
            event: Event::Type {
                text: None,
                redacted: true,
            },
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"t":2.9,"type":"type","text":null,"redacted":true}"#
        );
        let plain = TimedEvent {
            t: 1.0,
            event: Event::Type {
                text: Some("ls".into()),
                redacted: false,
            },
        };
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"{"t":1.0,"type":"type","text":"ls"}"#
        );
    }

    #[test]
    fn x11_source_and_click_serialize_like_the_spec() {
        let src = Source::X11 {
            command: vec![],
            frame: Frame::try_from(Dims { w: 1920, h: 1080 }).unwrap(),
        };
        assert_eq!(
            serde_json::to_string(&src).unwrap(),
            r#"{"kind":"x11","frame":{"x":0,"y":0,"width":1920,"height":1080}}"#
        );
        let click = TimedEvent {
            t: 1.2,
            event: Event::Click {
                x: normalize(797.0, 1920),
                y: normalize(249.0, 1080),
                button: Button::Left,
            },
        };
        assert_eq!(
            serde_json::to_string(&click).unwrap(),
            r#"{"t":1.2,"type":"click","x":0.415104,"y":0.230556,"button":"left"}"#
        );
    }

    #[test]
    fn a_screen_track_without_a_pointer_field_has_it_baked_in() {
        let old = r#"{"kind":"screen","file":"screen.mp4","offset":0.5,"width":1920,"height":1080}"#;
        let Track::Screen { pointer, .. } = serde_json::from_str(old).unwrap() else {
            panic!("a screen track");
        };
        assert_eq!(pointer, PointerCapture::Baked);
        let new = Track::Screen {
            file: SCREEN_FILE.into(),
            offset: 0.5,
            width: 1920,
            height: 1080,
            pointer: PointerCapture::Timeline,
        };
        assert!(serde_json::to_string(&new)
            .unwrap()
            .ends_with(r#""pointer":"timeline"}"#));
    }

    #[test]
    fn normalized_coordinates_span_the_frame() {
        assert_eq!(normalize(0.0, 1920), 0.0);
        assert_eq!(normalize(960.0, 1920), 0.5);
        assert_eq!(normalize(1080.0, 1080), 1.0);
    }

    #[test]
    fn x11_screens_need_even_sides() {
        assert!(Frame::try_from(Dims { w: 1080, h: 1920 }).is_ok());
        assert!(Frame::try_from(Dims { w: 1081, h: 1920 }).is_err());
        assert!(Frame::try_from(Dims { w: 10, h: 10 }).is_err());
    }

    #[test]
    fn size_parses_cols_by_rows() {
        assert_eq!("120x36".parse::<Size>(), Ok(Size { cols: 120, rows: 36 }));
        assert!("120".parse::<Size>().is_err());
        assert!("0x10".parse::<Size>().is_err());
    }
}
