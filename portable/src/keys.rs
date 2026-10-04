//! Key combos (`Return`, `ctrl+c`, `alt+x`): the bytes an xterm-compatible terminal sends, and
//! the keysyms an x11 source presses. One grammar and one table of names serve both.

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

/// Every named key once: what a tty receives, and the X11 keysym an x11 source presses.
const NAMED: &[(&str, Seq, u32)] = &[
    ("return", Seq::Fixed(b"\r"), 0xff0d),
    ("enter", Seq::Fixed(b"\r"), 0xff0d),
    ("tab", Seq::Fixed(b"\t"), 0xff09),
    ("escape", Seq::Fixed(b"\x1b"), 0xff1b),
    ("esc", Seq::Fixed(b"\x1b"), 0xff1b),
    ("backspace", Seq::Fixed(b"\x7f"), 0xff08),
    ("space", Seq::Fixed(b" "), 0x0020),
    ("up", Seq::Cursor(b"\x1b[A", b"\x1bOA"), 0xff52),
    ("down", Seq::Cursor(b"\x1b[B", b"\x1bOB"), 0xff54),
    ("right", Seq::Cursor(b"\x1b[C", b"\x1bOC"), 0xff53),
    ("left", Seq::Cursor(b"\x1b[D", b"\x1bOD"), 0xff51),
    ("home", Seq::Cursor(b"\x1b[H", b"\x1bOH"), 0xff50),
    ("end", Seq::Cursor(b"\x1b[F", b"\x1bOF"), 0xff57),
    ("pageup", Seq::Fixed(b"\x1b[5~"), 0xff55),
    ("pagedown", Seq::Fixed(b"\x1b[6~"), 0xff56),
    ("delete", Seq::Fixed(b"\x1b[3~"), 0xffff),
    ("f1", Seq::Fixed(b"\x1bOP"), 0xffbe),
    ("f2", Seq::Fixed(b"\x1bOQ"), 0xffbf),
    ("f3", Seq::Fixed(b"\x1bOR"), 0xffc0),
    ("f4", Seq::Fixed(b"\x1bOS"), 0xffc1),
    ("f5", Seq::Fixed(b"\x1b[15~"), 0xffc2),
    ("f6", Seq::Fixed(b"\x1b[17~"), 0xffc3),
    ("f7", Seq::Fixed(b"\x1b[18~"), 0xffc4),
    ("f8", Seq::Fixed(b"\x1b[19~"), 0xffc5),
    ("f9", Seq::Fixed(b"\x1b[20~"), 0xffc6),
    ("f10", Seq::Fixed(b"\x1b[21~"), 0xffc7),
    ("f11", Seq::Fixed(b"\x1b[23~"), 0xffc8),
    ("f12", Seq::Fixed(b"\x1b[24~"), 0xffc9),
];

pub const SHIFT_L: u32 = 0xffe1;
pub const CONTROL_L: u32 = 0xffe3;
pub const ALT_L: u32 = 0xffe9;
pub const SUPER_L: u32 = 0xffeb;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Mods {
    ctrl: bool,
    alt: bool,
    shift: bool,
    super_: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// An index into `NAMED`.
    Named(usize),
    Char(char),
}

fn parse(combo: &str) -> Result<(Mods, Key)> {
    let bad = |why: &str| RecError::new("bad_key", format!("{combo:?}: {why}"));
    // Split off modifiers from the left so a literal "+" key (e.g. "alt++") survives.
    let mut rest = combo;
    let mut mods = Mods::default();
    while let Some((head, tail)) = rest.split_once('+') {
        if tail.is_empty() {
            break;
        }
        match head.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "meta" | "option" => mods.alt = true,
            "shift" => mods.shift = true,
            "super" | "cmd" | "win" => mods.super_ = true,
            other => return Err(bad(&format!("unknown modifier {other:?}"))),
        }
        rest = tail;
    }
    if rest.is_empty() {
        return Err(bad("empty key"));
    }
    let lower = rest.to_ascii_lowercase();
    if let Some(i) = NAMED.iter().position(|(n, ..)| *n == lower) {
        return Ok((mods, Key::Named(i)));
    }
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok((mods, Key::Char(c))),
        _ => Err(bad("unknown key name")),
    }
}

/// The bytes a tty program receives for `combo`.
pub fn encode(combo: &str, mode: CursorMode) -> Result<Vec<u8>> {
    let bad = |why: &str| RecError::new("bad_key", format!("{combo:?}: {why}"));
    let (mods, key) = parse(combo)?;
    if mods.shift || mods.super_ {
        return Err(bad(
            "shift and super only combine on x11; on a tty, type the shifted character",
        ));
    }
    let mut base = match key {
        Key::Named(i) => match (&NAMED[i].1, mode) {
            (Seq::Fixed(b), _) | (Seq::Cursor(b, _), CursorMode::Normal) => b.to_vec(),
            (Seq::Cursor(_, b), CursorMode::Application) => b.to_vec(),
        },
        Key::Char(c) => c.to_string().into_bytes(),
    };
    if mods.ctrl {
        let [b] = base[..] else {
            return Err(bad("ctrl only combines with a single character"));
        };
        base = vec![ctrl_byte(b).ok_or_else(|| bad("no control code for this key"))?];
    }
    if mods.alt {
        base.insert(0, 0x1b);
    }
    Ok(base)
}

/// The modifier keysyms held down, in press order, and the keysym pressed under them. A letter
/// under ctrl, alt or super is its lowercase key, as on a tty, so `ctrl+C` is not `ctrl+shift+c`.
pub fn x11(combo: &str) -> Result<(Vec<u32>, u32)> {
    let (mods, key) = parse(combo)?;
    let sym = match key {
        Key::Named(i) => NAMED[i].2,
        Key::Char(c) if mods.ctrl || mods.alt || mods.super_ => keysym(c.to_ascii_lowercase()),
        Key::Char(c) => keysym(c),
    };
    let held = [
        (mods.ctrl, CONTROL_L),
        (mods.alt, ALT_L),
        (mods.shift, SHIFT_L),
        (mods.super_, SUPER_L),
    ];
    let held = held.iter().filter(|(on, _)| *on).map(|(_, s)| *s).collect();
    Ok((held, sym))
}

/// The X11 keysym for a typed character: Latin-1 maps to itself, control characters to their
/// function keys, and everything else to the Unicode keysym range.
pub fn keysym(c: char) -> u32 {
    match c {
        '\n' | '\r' => 0xff0d,
        '\t' => 0xff09,
        '\x08' => 0xff08,
        '\x1b' => 0xff1b,
        '\x7f' => 0xffff,
        ' '..='~' | '\u{a0}'..='\u{ff}' => c as u32,
        _ => 0x0100_0000 + c as u32,
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
        for c in ["Hyper", "shift+a", "super+x", "ctrl+Up", "ctrl+", "", "F13"] {
            let err = encode(c, Normal).unwrap_err();
            assert_eq!(err.code, "bad_key", "{c:?}");
        }
    }

    #[test]
    fn x11_combos_press_one_keysym_under_held_modifiers() {
        assert_eq!(x11("Return").unwrap(), (vec![], 0xff0d));
        assert_eq!(x11("BackSpace").unwrap(), (vec![], 0xff08));
        assert_eq!(x11("PageDown").unwrap(), (vec![], 0xff56));
        assert_eq!(x11("F12").unwrap(), (vec![], 0xffc9));
        assert_eq!(x11("space").unwrap(), (vec![], 0x20));
        assert_eq!(x11("ctrl+c").unwrap(), (vec![CONTROL_L], 'c' as u32));
        assert_eq!(x11("ctrl+C").unwrap(), (vec![CONTROL_L], 'c' as u32));
        assert_eq!(
            x11("ctrl+shift+c").unwrap(),
            (vec![CONTROL_L, SHIFT_L], 'c' as u32)
        );
        assert_eq!(x11("shift+Tab").unwrap(), (vec![SHIFT_L], 0xff09));
        assert_eq!(
            x11("super+alt+Left").unwrap(),
            (vec![ALT_L, SUPER_L], 0xff51)
        );
        assert_eq!(x11("A").unwrap(), (vec![], 'A' as u32));
        assert_eq!(x11("alt++").unwrap(), (vec![ALT_L], '+' as u32));
        for c in ["Hyper", "ctrl+", "", "F13"] {
            assert_eq!(x11(c).unwrap_err().code, "bad_key", "{c:?}");
        }
    }

    #[test]
    fn typed_characters_map_to_keysyms() {
        assert_eq!(keysym('a'), 0x61);
        assert_eq!(keysym('~'), 0x7e);
        assert_eq!(keysym('\n'), 0xff0d);
        assert_eq!(keysym('é'), 0xe9);
        assert_eq!(keysym('✔'), 0x0100_2714);
    }
}
