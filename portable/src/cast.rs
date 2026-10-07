//! asciicast v2: a JSON header line, then one `[t, "o", data]` line per output chunk.

use crate::error::{RecError, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Header {
    pub version: u32,
    pub width: u16,
    pub height: u16,
    pub timestamp: i64,
}

/// Turns arbitrary PTY reads into valid UTF-8 strings without splitting a
/// multi-byte character that straddles two reads.
#[derive(Default)]
pub struct Utf8Buffer {
    pending: Vec<u8>,
}

impl Utf8Buffer {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        let mut rest: &[u8] = &self.pending;
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    rest = &[];
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(valid).expect("validated prefix"));
                    match e.error_len() {
                        Some(n) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[n..];
                        }
                        None => {
                            rest = after;
                            break;
                        }
                    }
                }
            }
        }
        self.pending = rest.to_vec();
        out
    }

    /// Whatever is left at EOF can never complete.
    pub fn finish(&mut self) -> String {
        let s = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        s
    }
}

pub fn header_line(h: &Header) -> String {
    serde_json::to_string(h).expect("serializable")
}

pub fn output_line(t: f64, data: &str) -> String {
    serde_json::to_string(&(crate::model::round_t(t), "o", data)).expect("serializable")
}

pub struct CastWriter<W: Write> {
    out: W,
}

impl<W: Write> CastWriter<W> {
    pub fn new(mut out: W, header: &Header) -> std::io::Result<Self> {
        writeln!(out, "{}", header_line(header))?;
        out.flush()?;
        Ok(CastWriter { out })
    }

    /// Flushed per line so a crashed recorder still leaves everything it saw.
    pub fn output(&mut self, t: f64, data: &str) -> std::io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        writeln!(self.out, "{}", output_line(t, data))?;
        self.out.flush()
    }
}

pub struct Cast {
    pub header: Header,
    pub output: Vec<(f64, String)>,
}

pub fn read(input: impl BufRead) -> Result<Cast> {
    let bad = |n: usize, why: String| RecError::new("bad_cast", format!("line {n}: {why}"));
    let mut lines = input.lines().enumerate();
    let (_, first) = lines
        .next()
        .ok_or_else(|| RecError::new("bad_cast", "empty cast"))?;
    let first = first.map_err(|e| bad(1, e.to_string()))?;
    let header: Header = serde_json::from_str(&first).map_err(|e| bad(1, e.to_string()))?;
    let mut output = Vec::new();
    for (i, line) in lines {
        let line = line.map_err(|e| bad(i + 1, e.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        // A recorder killed mid-write can leave a torn last line; keep what parsed.
        let Ok((t, kind, data)) = serde_json::from_str::<(f64, String, String)>(&line) else {
            break;
        };
        if kind == "o" {
            output.push((t, data));
        }
    }
    Ok(Cast { header, output })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_split_across_reads_is_reassembled() {
        let bytes = "héllo ✓ 🎬".as_bytes();
        for split in 0..bytes.len() {
            let mut buf = Utf8Buffer::default();
            let mut s = buf.push(&bytes[..split]);
            s += &buf.push(&bytes[split..]);
            s += &buf.finish();
            assert_eq!(s, "héllo ✓ 🎬", "split at {split}");
        }
    }

    #[test]
    fn utf8_one_byte_at_a_time_never_emits_partial_chars() {
        let mut buf = Utf8Buffer::default();
        let emitted: Vec<String> = "✓🎬".bytes().map(|b| buf.push(&[b])).collect();
        assert_eq!(emitted, ["", "", "✓", "", "", "", "🎬"]);
    }

    #[test]
    fn utf8_invalid_bytes_become_replacement_and_do_not_stall() {
        let mut buf = Utf8Buffer::default();
        assert_eq!(buf.push(b"a\xffb"), "a\u{FFFD}b");
        assert_eq!(buf.push(b"\xe2\x9c"), "");
        assert_eq!(buf.finish(), "\u{FFFD}");
    }

    #[test]
    fn output_lines_are_asciicast_v2() {
        assert_eq!(output_line(1.5, "hi\r\n"), r#"[1.5,"o","hi\r\n"]"#);
        assert_eq!(
            output_line(0.1234567, "\x1b[1m"),
            r#"[0.123457,"o","\u001b[1m"]"#
        );
        let h = Header {
            version: 2,
            width: 100,
            height: 30,
            timestamp: 1_700_000_000,
        };
        assert_eq!(
            header_line(&h),
            r#"{"version":2,"width":100,"height":30,"timestamp":1700000000}"#
        );
    }

    #[test]
    fn written_cast_reads_back_and_tolerates_a_torn_tail() {
        let h = Header {
            version: 2,
            width: 80,
            height: 24,
            timestamp: 0,
        };
        let mut w = CastWriter::new(Vec::new(), &h).unwrap();
        w.output(0.25, "a\"b").unwrap();
        w.output(0.5, "").unwrap();
        w.output(1.0, "c").unwrap();
        let mut bytes = w.out;
        bytes.extend_from_slice(b"[1.2,\"o\",\"unterminat");
        let cast = read(&bytes[..]).unwrap();
        assert_eq!(cast.header, h);
        assert_eq!(
            cast.output,
            vec![(0.25, "a\"b".to_string()), (1.0, "c".to_string())]
        );
    }
}
