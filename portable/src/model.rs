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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    Tty { command: Vec<String>, size: Size },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Track {
    Term { file: String, offset: f64 },
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
    fn size_parses_cols_by_rows() {
        assert_eq!("120x36".parse::<Size>(), Ok(Size { cols: 120, rows: 36 }));
        assert!("120".parse::<Size>().is_err());
        assert!("0x10".parse::<Size>().is_err());
    }
}
