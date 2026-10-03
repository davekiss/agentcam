//! Key combos (`Return`, `ctrl+c`, `alt+x`) to the bytes an xterm-compatible terminal sends.

use crate::error::{RecError, Result};

/// Cursor keys have two encodings; full-screen programs switch the terminal into
/// "application cursor" mode (DECCKM) and expect the SS3 form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    Normal,
    Application,
}

enum Seq {
    Fixed(&'static [u8]),
    /// CSI form in normal mode, SS3 form in application mode.
    Cursor(&'static [u8], &'static [u8]),
}

const NAMED: &[(&str, Seq)] = &[
    ("return", Seq::Fixed(b"\r")),
    ("enter", Seq::Fixed(b"\r")),
    ("tab", Seq::Fixed(b"\t")),
    ("escape", Seq::Fixed(b"\x1b")),
    ("esc", Seq::Fixed(b"\x1b")),
    ("backspace", Seq::Fixed(b"\x7f")),
    ("space", Seq::Fixed(b" ")),
    ("up", Seq::Cursor(b"\x1b[A", b"\x1bOA")),
    ("down", Seq::Cursor(b"\x1b[B", b"\x1bOB")),
    ("right", Seq::Cursor(b"\x1b[C", b"\x1bOC")),
    ("left", Seq::Cursor(b"\x1b[D", b"\x1bOD")),
    ("home", Seq::Cursor(b"\x1b[H", b"\x1bOH")),
    ("end", Seq::Cursor(b"\x1b[F", b"\x1bOF")),
    ("pageup", Seq::Fixed(b"\x1b[5~")),
    ("pagedown", Seq::Fixed(b"\x1b[6~")),
    ("delete", Seq::Fixed(b"\x1b[3~")),
    ("f1", Seq::Fixed(b"\x1bOP")),
    ("f2", Seq::Fixed(b"\x1bOQ")),
    ("f3", Seq::Fixed(b"\x1bOR")),
    ("f4", Seq::Fixed(b"\x1bOS")),
    ("f5", Seq::Fixed(b"\x1b[15~")),
    ("f6", Seq::Fixed(b"\x1b[17~")),
    ("f7", Seq::Fixed(b"\x1b[18~")),
    ("f8", Seq::Fixed(b"\x1b[19~")),
    ("f9", Seq::Fixed(b"\x1b[20~")),
    ("f10", Seq::Fixed(b"\x1b[21~")),
    ("f11", Seq::Fixed(b"\x1b[23~")),
    ("f12", Seq::Fixed(b"\x1b[24~")),
];

pub fn encode(combo: &str, mode: CursorMode) -> Result<Vec<u8>> {
    let bad = |why: &str| RecError::new("bad_key", format!("{combo:?}: {why}"));
    // Split off modifiers from the left so a literal "+" key (e.g. "alt++") survives.
    let mut rest = combo;
    let (mut ctrl, mut alt) = (false, false);
    while let Some((head, tail)) = rest.split_once('+') {
        if tail.is_empty() {
            break;
        }
        match head.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "meta" | "option" => alt = true,
            other => return Err(bad(&format!("unknown modifier {other:?}"))),
        }
        rest = tail;
    }
    if rest.is_empty() {
        return Err(bad("empty key"));
    }

    let mut base = base_key(rest, mode).ok_or_else(|| bad("unknown key name"))?;
    if ctrl {
        let [b] = base[..] else {
            return Err(bad("ctrl only combines with a single character"));
        };
        base = vec![ctrl_byte(b).ok_or_else(|| bad("no control code for this key"))?];
    }
    if alt {
        base.insert(0, 0x1b);
    }
    Ok(base)
}

fn base_key(name: &str, mode: CursorMode) -> Option<Vec<u8>> {
    let lower = name.to_ascii_lowercase();
    if let Some((_, seq)) = NAMED.iter().find(|(n, _)| *n == lower) {
        return Some(match (seq, mode) {
            (Seq::Fixed(b), _) | (Seq::Cursor(b, _), CursorMode::Normal) => b.to_vec(),
            (Seq::Cursor(_, b), CursorMode::Application) => b.to_vec(),
        });
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c.to_string().into_bytes()),
        _ => None,
    }
}

fn ctrl_byte(b: u8) -> Option<u8> {
    match b {
        b'a'..=b'z' => Some(b - b'a' + 1),
        b'A'..=b'Z' => Some(b - b'A' + 1),
        b' ' | b'@' | b'2' => Some(0),
        b'[' => Some(0x1b),
        b'\\' => Some(0x1c),
        b']' => Some(0x1d),
        b'^' => Some(0x1e),
        b'_' => Some(0x1f),
        b'?' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CursorMode::*;

    fn enc(c: &str) -> Vec<u8> {
        encode(c, Normal).unwrap()
    }

    #[test]
    fn named_keys_map_to_xterm_sequences() {
        assert_eq!(enc("Return"), b"\r");
        assert_eq!(enc("enter"), b"\r");
        assert_eq!(enc("Tab"), b"\t");
        assert_eq!(enc("Escape"), b"\x1b");
        assert_eq!(enc("Backspace"), b"\x7f");
        assert_eq!(enc("Space"), b" ");
        assert_eq!(enc("PageDown"), b"\x1b[6~");
        assert_eq!(enc("Delete"), b"\x1b[3~");
        assert_eq!(enc("F1"), b"\x1bOP");
        assert_eq!(enc("F5"), b"\x1b[15~");
        assert_eq!(enc("F12"), b"\x1b[24~");
    }

    #[test]
    fn arrows_follow_cursor_mode() {
        assert_eq!(enc("Up"), b"\x1b[A");
        assert_eq!(encode("Up", Application).unwrap(), b"\x1bOA");
        assert_eq!(encode("Home", Application).unwrap(), b"\x1bOH");
        assert_eq!(encode("PageUp", Application).unwrap(), b"\x1b[5~");
    }

    #[test]
    fn modifiers() {
        assert_eq!(enc("ctrl+c"), [0x03]);
        assert_eq!(enc("CTRL+C"), [0x03]);
        assert_eq!(enc("ctrl+["), [0x1b]);
        assert_eq!(enc("alt+x"), b"\x1bx");
        assert_eq!(enc("alt+Left"), b"\x1b\x1b[D");
        assert_eq!(enc("ctrl+alt+d"), [0x1b, 0x04]);
        assert_eq!(enc("alt++"), b"\x1b+");
        assert_eq!(enc("q"), b"q");
    }

    #[test]
    fn unknown_keys_are_bad_key() {
        for c in ["Hyper", "shift+a", "ctrl+Up", "ctrl+", "", "F13"] {
            let err = encode(c, Normal).unwrap_err();
            assert_eq!(err.code, "bad_key", "{c:?}");
        }
    }
}
