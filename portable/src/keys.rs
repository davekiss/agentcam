//! Key combos (`Return`, `ctrl+c`, `alt+x`): the bytes a terminal sends, and the keysyms an x11
//! source presses. One grammar and one table of names serve both. A tty program gets xterm's
//! legacy bytes until it turns on the kitty keyboard protocol, and then that protocol's encoding
//! for the flags it chose (https://sw.kovidgoyal.net/kitty/keyboard-protocol/).

use crate::error::{RecError, Result};

/// Cursor keys have two encodings; full-screen programs switch the terminal into
/// "application cursor" mode (DECCKM) and expect the SS3 form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    Normal,
    Application,
}

/// What a tty program asked of its keyboard, read from the emulated terminal at each key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keyboard {
    pub cursor: CursorMode,
    /// The kitty progressive enhancement flags in effect; 0 is the legacy encoding.
    pub kitty: u8,
}

const EVENT_TYPES: u8 = 2;
const ALTERNATE_KEYS: u8 = 4;
const ALL_KEYS: u8 = 8;
const ASSOCIATED_TEXT: u8 = 16;

enum Seq {
    Fixed(&'static [u8]),
    /// CSI form in normal mode, SS3 form in application mode.
    Cursor(&'static [u8], &'static [u8]),
}

/// A key's kitty escape code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kitty {
    /// `CSI code u`
    U(u32),
    /// `CSI number ~`
    Tilde(u32),
    /// `CSI 1 letter`, and the legacy bytes while unmodified.
    Letter(u8),
    /// Enter, Tab, and Backspace: `CSI code u`, but the legacy byte while unmodified and not
    /// reporting all keys, so a shell stays usable after a program leaves the mode on.
    Control(u32),
    /// A key that types its character, encoded as the unshifted key's code point.
    Text(char),
}

/// Every named key once: what a legacy tty receives, its kitty form, and the X11 keysym an x11
/// source presses.
const NAMED: &[(&str, Seq, Kitty, u32)] = &[
    ("return", Seq::Fixed(b"\r"), Kitty::Control(13), 0xff0d),
    ("enter", Seq::Fixed(b"\r"), Kitty::Control(13), 0xff0d),
    ("tab", Seq::Fixed(b"\t"), Kitty::Control(9), 0xff09),
    ("escape", Seq::Fixed(b"\x1b"), Kitty::U(27), 0xff1b),
    ("esc", Seq::Fixed(b"\x1b"), Kitty::U(27), 0xff1b),
    (
        "backspace",
        Seq::Fixed(b"\x7f"),
        Kitty::Control(127),
        0xff08,
    ),
    ("space", Seq::Fixed(b" "), Kitty::Text(' '), 0x0020),
    (
        "up",
        Seq::Cursor(b"\x1b[A", b"\x1bOA"),
        Kitty::Letter(b'A'),
        0xff52,
    ),
    (
        "down",
        Seq::Cursor(b"\x1b[B", b"\x1bOB"),
        Kitty::Letter(b'B'),
        0xff54,
    ),
    (
        "right",
        Seq::Cursor(b"\x1b[C", b"\x1bOC"),
        Kitty::Letter(b'C'),
        0xff53,
    ),
    (
        "left",
        Seq::Cursor(b"\x1b[D", b"\x1bOD"),
        Kitty::Letter(b'D'),
        0xff51,
    ),
    (
        "home",
        Seq::Cursor(b"\x1b[H", b"\x1bOH"),
        Kitty::Letter(b'H'),
        0xff50,
    ),
    (
        "end",
        Seq::Cursor(b"\x1b[F", b"\x1bOF"),
        Kitty::Letter(b'F'),
        0xff57,
    ),
    ("pageup", Seq::Fixed(b"\x1b[5~"), Kitty::Tilde(5), 0xff55),
    ("pagedown", Seq::Fixed(b"\x1b[6~"), Kitty::Tilde(6), 0xff56),
    ("insert", Seq::Fixed(b"\x1b[2~"), Kitty::Tilde(2), 0xff63),
    ("delete", Seq::Fixed(b"\x1b[3~"), Kitty::Tilde(3), 0xffff),
    ("f1", Seq::Fixed(b"\x1bOP"), Kitty::Letter(b'P'), 0xffbe),
    ("f2", Seq::Fixed(b"\x1bOQ"), Kitty::Letter(b'Q'), 0xffbf),
    ("f3", Seq::Fixed(b"\x1bOR"), Kitty::Tilde(13), 0xffc0),
    ("f4", Seq::Fixed(b"\x1bOS"), Kitty::Letter(b'S'), 0xffc1),
    ("f5", Seq::Fixed(b"\x1b[15~"), Kitty::Tilde(15), 0xffc2),
    ("f6", Seq::Fixed(b"\x1b[17~"), Kitty::Tilde(17), 0xffc3),
    ("f7", Seq::Fixed(b"\x1b[18~"), Kitty::Tilde(18), 0xffc4),
    ("f8", Seq::Fixed(b"\x1b[19~"), Kitty::Tilde(19), 0xffc5),
    ("f9", Seq::Fixed(b"\x1b[20~"), Kitty::Tilde(20), 0xffc6),
    ("f10", Seq::Fixed(b"\x1b[21~"), Kitty::Tilde(21), 0xffc7),
    ("f11", Seq::Fixed(b"\x1b[23~"), Kitty::Tilde(23), 0xffc8),
    ("f12", Seq::Fixed(b"\x1b[24~"), Kitty::Tilde(24), 0xffc9),
];

/// Shifted ASCII punctuation and the US-layout key it is typed on.
const US_SHIFTED: &[(char, char)] = &[
    ('~', '`'),
    ('!', '1'),
    ('@', '2'),
    ('#', '3'),
    ('$', '4'),
    ('%', '5'),
    ('^', '6'),
    ('&', '7'),
    ('*', '8'),
    ('(', '9'),
    (')', '0'),
    ('_', '-'),
    ('+', '='),
    ('{', '['),
    ('}', ']'),
    ('|', '\\'),
    (':', ';'),
    ('"', '\''),
    ('<', ','),
    ('>', '.'),
    ('?', '/'),
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

impl Mods {
    /// The held modifiers in press order: kitty's left-hand key code, its bit, and the x11 keysym.
    fn held(self) -> impl DoubleEndedIterator<Item = (u32, u8, u32)> {
        [
            (self.ctrl, 57442, 4, CONTROL_L),
            (self.alt, 57443, 2, ALT_L),
            (self.shift, 57441, 1, SHIFT_L),
            (self.super_, 57444, 8, SUPER_L),
        ]
        .into_iter()
        .filter(|m| m.0)
        .map(|(_, code, bit, sym)| (code, bit, sym))
    }

    fn bits(self) -> u8 {
        self.held().map(|(_, bit, _)| bit).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// An index into `NAMED`.
    Named(usize),
    Char(char),
}

fn named(name: &str) -> Key {
    Key::Named(
        NAMED
            .iter()
            .position(|(n, ..)| *n == name)
            .expect("a named key"),
    )
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
pub fn encode(combo: &str, keyboard: Keyboard) -> Result<Vec<u8>> {
    let (mods, key) = parse(combo)?;
    if keyboard.kitty == 0 {
        return legacy(combo, mods, key, keyboard.cursor);
    }
    Ok(kitty(mods, key, keyboard))
}

/// The bytes a tty program receives for one character of typed text. Text is sent as itself
/// unless the program asked for every key as an escape code.
pub fn encode_char(c: char, keyboard: Keyboard) -> Vec<u8> {
    if keyboard.kitty & ALL_KEYS == 0 {
        return c.to_string().into_bytes();
    }
    let key = match c {
        '\r' | '\n' => named("return"),
        '\t' => named("tab"),
        '\x1b' => named("escape"),
        '\x7f' | '\x08' => named("backspace"),
        _ => Key::Char(c),
    };
    kitty(Mods::default(), key, keyboard)
}

fn legacy(combo: &str, mods: Mods, key: Key, cursor: CursorMode) -> Result<Vec<u8>> {
    let bad = |why: &str| RecError::new("bad_key", format!("{combo:?}: {why}"));
    if mods.shift || mods.super_ {
        return Err(bad(
            "shift and super only combine on x11, or on a tty program using the kitty keyboard \
             protocol; otherwise type the shifted character",
        ));
    }
    let mut base = match key {
        Key::Named(i) => legacy_named(i, cursor).to_vec(),
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

fn legacy_named(i: usize, cursor: CursorMode) -> &'static [u8] {
    match (&NAMED[i].1, cursor) {
        (Seq::Fixed(b), _) | (Seq::Cursor(b, _), CursorMode::Normal) => b,
        (Seq::Cursor(_, b), CursorMode::Application) => b,
    }
}

/// One key pressed (and released, when the program asked for release events) under the kitty
/// keyboard protocol with `keyboard.kitty` flags.
fn kitty(mods: Mods, key: Key, keyboard: Keyboard) -> Vec<u8> {
    let flags = keyboard.kitty;
    let all = flags & ALL_KEYS != 0;
    let releases = flags & EVENT_TYPES != 0;
    let (form, mods) = match key {
        Key::Named(i) => (NAMED[i].2, mods),
        // A letter under ctrl, alt or super is its key, as on x11, so `ctrl+C` is `ctrl+c`.
        Key::Char(c) if c.is_alphabetic() && (mods.ctrl || mods.alt || mods.super_) => {
            (Kitty::Text(unshift(c).0), mods)
        }
        Key::Char(c) => {
            let (base, shifted) = unshift(c);
            (
                Kitty::Text(base),
                Mods {
                    shift: mods.shift || shifted,
                    ..mods
                },
            )
        }
    };
    let plain = mods == Mods::default();
    let text = match form {
        Kitty::Text(base) if !(mods.ctrl || mods.alt || mods.super_) => {
            Some(if mods.shift { shift(base) } else { base })
        }
        _ => None,
    };
    let mut out = Vec::new();
    if all {
        let mut bits = 0;
        for (code, bit, _) in mods.held() {
            bits |= bit;
            out.extend(csi_u(&code.to_string(), bits, false, None));
        }
    }
    // Text and the shell-safe controls go as legacy bytes with no release; unmodified function
    // keys keep their legacy form for the press but report a release.
    let released = match (form, key) {
        (Kitty::Text(_), _) if !all && text.is_some() => {
            out.extend(text.expect("matched").to_string().into_bytes());
            false
        }
        (Kitty::Control(_), Key::Named(i)) if !all && plain => {
            out.extend(legacy_named(i, keyboard.cursor));
            false
        }
        (Kitty::Letter(_) | Kitty::Tilde(_), Key::Named(i)) if !all && plain => {
            out.extend(legacy_named(i, keyboard.cursor));
            true
        }
        _ => {
            out.extend(escape(form, mods, false, text, flags));
            true
        }
    };
    if releases && released {
        out.extend(escape(form, mods, true, None, flags));
    }
    if all && releases {
        let mut bits = mods.bits();
        for (code, bit, _) in mods.held().rev() {
            bits &= !bit;
            out.extend(csi_u(&code.to_string(), bits, true, None));
        }
    }
    out
}

/// The escape code for a press or release of `form` under `mods`.
fn escape(form: Kitty, mods: Mods, release: bool, text: Option<char>, flags: u8) -> Vec<u8> {
    let bits = mods.bits();
    match form {
        Kitty::Letter(letter) => {
            let tail = letter as char;
            match modifier_field(bits, release) {
                None => format!("\x1b[{tail}"),
                Some(field) => format!("\x1b[1;{field}{tail}"),
            }
            .into_bytes()
        }
        Kitty::Tilde(n) => match modifier_field(bits, release) {
            None => format!("\x1b[{n}~"),
            Some(field) => format!("\x1b[{n};{field}~"),
        }
        .into_bytes(),
        Kitty::U(code) | Kitty::Control(code) => csi_u(&code.to_string(), bits, release, None),
        Kitty::Text(base) => {
            let mut key = (base as u32).to_string();
            let shifted = shift(base);
            if flags & ALTERNATE_KEYS != 0 && mods.shift && shifted != base {
                key += &format!(":{}", shifted as u32);
            }
            let text = text.filter(|_| flags & ALL_KEYS != 0 && flags & ASSOCIATED_TEXT != 0);
            csi_u(&key, bits, release, text)
        }
    }
}

/// `CSI key ; modifiers[:event] ; text u`, dropping trailing fields that hold only defaults.
fn csi_u(key: &str, bits: u8, release: bool, text: Option<char>) -> Vec<u8> {
    let field = modifier_field(bits, release);
    match (field, text) {
        (field, Some(t)) => format!("\x1b[{key};{};{}u", field.unwrap_or_default(), t as u32),
        (Some(field), None) => format!("\x1b[{key};{field}u"),
        (None, None) => format!("\x1b[{key}u"),
    }
    .into_bytes()
}

/// The modifiers field: 1 plus the modifier bits, with `:3` for a release; None when it would be
/// a plain press, which the protocol writes by leaving the field out.
fn modifier_field(bits: u8, release: bool) -> Option<String> {
    let value = 1 + bits as u32;
    match (value, release) {
        (1, false) => None,
        (value, false) => Some(value.to_string()),
        (value, true) => Some(format!("{value}:3")),
    }
}

/// The key a character is typed on, and whether typing it takes shift.
fn unshift(c: char) -> (char, bool) {
    if let Some(&(_, base)) = US_SHIFTED.iter().find(|(s, _)| *s == c) {
        return (base, true);
    }
    let mut lower = c.to_lowercase();
    match (c.is_uppercase(), lower.next(), lower.next()) {
        (true, Some(l), None) => (l, true),
        _ => (c, false),
    }
}

/// The character a key types with shift held.
fn shift(base: char) -> char {
    if let Some(&(shifted, _)) = US_SHIFTED.iter().find(|(_, b)| *b == base) {
        return shifted;
    }
    let mut upper = base.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => u,
        _ => base,
    }
}

/// The modifier keysyms held down, in press order, and the keysym pressed under them. A letter
/// under ctrl, alt or super is its lowercase key, as on a tty, so `ctrl+C` is not `ctrl+shift+c`.
pub fn x11(combo: &str) -> Result<(Vec<u32>, u32)> {
    let (mods, key) = parse(combo)?;
    let sym = match key {
        Key::Named(i) => NAMED[i].3,
        Key::Char(c) if mods.ctrl || mods.alt || mods.super_ => keysym(c.to_ascii_lowercase()),
        Key::Char(c) => keysym(c),
    };
    Ok((mods.held().map(|(_, _, sym)| sym).collect(), sym))
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

    fn legacy(cursor: CursorMode) -> Keyboard {
        Keyboard { cursor, kitty: 0 }
    }

    fn enc(c: &str) -> Vec<u8> {
        encode(c, legacy(Normal)).unwrap()
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
        assert_eq!(encode("Up", legacy(Application)).unwrap(), b"\x1bOA");
        assert_eq!(encode("Home", legacy(Application)).unwrap(), b"\x1bOH");
        assert_eq!(encode("PageUp", legacy(Application)).unwrap(), b"\x1b[5~");
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
            let err = encode(c, legacy(Normal)).unwrap_err();
            assert_eq!(err.code, "bad_key", "{c:?}");
        }
    }

    fn kitty(flags: u8) -> Keyboard {
        Keyboard {
            cursor: Normal,
            kitty: flags,
        }
    }

    /// The combo's bytes under `flags`, as text so a failure shows the escape codes.
    fn k(flags: u8, combo: &str) -> String {
        String::from_utf8(encode(combo, kitty(flags)).unwrap()).unwrap()
    }

    fn typed(flags: u8, text: &str) -> String {
        let bytes: Vec<u8> = text
            .chars()
            .flat_map(|c| encode_char(c, kitty(flags)))
            .collect();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn disambiguate_encodes_chords_and_escape_as_csi_u() {
        assert_eq!(k(1, "ctrl+j"), "\x1b[106;5u", "no longer LF");
        assert_eq!(k(1, "ctrl+c"), "\x1b[99;5u");
        assert_eq!(
            k(1, "ctrl+C"),
            "\x1b[99;5u",
            "a letter under ctrl is its key"
        );
        assert_eq!(k(1, "Escape"), "\x1b[27u");
        assert_eq!(k(1, "alt+x"), "\x1b[120;3u");
        assert_eq!(k(1, "ctrl+alt+d"), "\x1b[100;7u");
        assert_eq!(k(1, "ctrl+space"), "\x1b[32;5u");
        assert_eq!(k(1, "super+x"), "\x1b[120;9u");
        // The spec's example table: ctrl+shift on i, 3 and ; is CSI u.
        assert_eq!(k(1, "ctrl+shift+i"), "\x1b[105;6u");
        assert_eq!(k(1, "ctrl+shift+3"), "\x1b[51;6u");
        assert_eq!(k(1, "ctrl+shift+;"), "\x1b[59;6u");
        assert_eq!(k(1, "shift+alt+a"), "\x1b[97;4u");
    }

    #[test]
    fn disambiguate_keeps_text_and_plain_enter_tab_backspace_legacy() {
        assert_eq!(k(1, "a"), "a");
        assert_eq!(k(1, "A"), "A");
        assert_eq!(k(1, "shift+a"), "A");
        assert_eq!(k(1, "shift+1"), "!");
        assert_eq!(k(1, "Space"), " ");
        assert_eq!(k(1, "Return"), "\r");
        assert_eq!(k(1, "Tab"), "\t");
        assert_eq!(k(1, "BackSpace"), "\x7f");
        assert_eq!(typed(1, "Hi there\n"), "Hi there\n");
    }

    #[test]
    fn modified_enter_tab_backspace_are_csi_u() {
        assert_eq!(k(1, "shift+Return"), "\x1b[13;2u");
        assert_eq!(k(1, "ctrl+Return"), "\x1b[13;5u");
        assert_eq!(k(1, "shift+Tab"), "\x1b[9;2u");
        assert_eq!(
            k(1, "ctrl+shift+Tab"),
            "\x1b[9;6u",
            "the spec's fixterms erratum"
        );
        assert_eq!(k(1, "alt+BackSpace"), "\x1b[127;3u");
    }

    #[test]
    fn functional_keys_follow_the_spec_tables() {
        assert_eq!(k(1, "Up"), "\x1b[A", "unmodified keeps the legacy form");
        let app = Keyboard {
            cursor: Application,
            kitty: 1,
        };
        assert_eq!(encode("Up", app).unwrap(), b"\x1bOA");
        assert_eq!(k(1, "ctrl+Up"), "\x1b[1;5A");
        assert_eq!(encode("ctrl+Up", app).unwrap(), b"\x1b[1;5A");
        assert_eq!(k(1, "shift+End"), "\x1b[1;2F");
        assert_eq!(k(1, "alt+Home"), "\x1b[1;3H");
        assert_eq!(k(1, "ctrl+PageDown"), "\x1b[6;5~");
        assert_eq!(k(1, "shift+Insert"), "\x1b[2;2~");
        assert_eq!(k(1, "ctrl+Delete"), "\x1b[3;5~");
        assert_eq!(k(1, "ctrl+F1"), "\x1b[1;5P");
        assert_eq!(k(1, "shift+F3"), "\x1b[13;2~", "F3 has no CSI R form");
        assert_eq!(k(1, "F3"), "\x1bOR");
        assert_eq!(k(1, "alt+F12"), "\x1b[24;3~");
        assert_eq!(k(8, "F1"), "\x1b[P");
        assert_eq!(k(8, "F5"), "\x1b[15~");
        assert_eq!(k(8, "Up"), "\x1b[A");
    }

    #[test]
    fn report_all_keys_sends_everything_as_escape_codes() {
        assert_eq!(k(8, "a"), "\x1b[97u");
        assert_eq!(k(8, "Return"), "\x1b[13u");
        assert_eq!(k(8, "Tab"), "\x1b[9u");
        assert_eq!(k(8, "BackSpace"), "\x1b[127u");
        assert_eq!(k(8, "Space"), "\x1b[32u");
        assert_eq!(typed(8, "a\n"), "\x1b[97u\x1b[13u");
        // Pressing a modifier is a key event too, with its own bit already set.
        assert_eq!(k(8, "A"), "\x1b[57441;2u\x1b[97;2u");
        assert_eq!(k(8, "ctrl+j"), "\x1b[57442;5u\x1b[106;5u");
    }

    #[test]
    fn associated_text_rides_in_the_third_field() {
        assert_eq!(k(8 | 16, "a"), "\x1b[97;;97u");
        // The spec's example: shift+a -> CSI 97 ; 2 ; 65 u.
        assert_eq!(k(8 | 16, "shift+a"), "\x1b[57441;2u\x1b[97;2;65u");
        assert_eq!(
            k(8 | 16, "ctrl+a"),
            "\x1b[57442;5u\x1b[97;5u",
            "a chord types no text"
        );
        assert_eq!(typed(8 | 16, "é"), "\x1b[233;;233u");
    }

    #[test]
    fn alternate_keys_add_the_shifted_key_only_under_shift() {
        assert_eq!(k(1 | 4, "ctrl+shift+a"), "\x1b[97:65;6u");
        assert_eq!(k(1 | 4, "ctrl+a"), "\x1b[97;5u");
        assert_eq!(k(1 | 4, "ctrl+shift+2"), "\x1b[50:64;6u");
        assert_eq!(k(1 | 4, "shift+Tab"), "\x1b[9;2u");
        assert_eq!(
            k(1 | 4, "a"),
            "a",
            "only keys already sent as escape codes change"
        );
    }

    #[test]
    fn event_types_add_a_release_after_each_escape_coded_press() {
        assert_eq!(k(1 | 2, "Escape"), "\x1b[27u\x1b[27;1:3u");
        assert_eq!(k(1 | 2, "ctrl+j"), "\x1b[106;5u\x1b[106;5:3u");
        assert_eq!(k(1 | 2, "Up"), "\x1b[A\x1b[1;1:3A");
        assert_eq!(k(1 | 2, "ctrl+PageUp"), "\x1b[5;5~\x1b[5;5:3~");
        // Text and the shell-safe keys report no release unless every key is an escape code.
        assert_eq!(k(1 | 2, "a"), "a");
        assert_eq!(k(1 | 2, "Return"), "\r");
        assert_eq!(k(8 | 2, "Return"), "\x1b[13u\x1b[13;1:3u");
        // Releasing a modifier clears its bit.
        assert_eq!(
            k(8 | 2, "ctrl+shift+a"),
            "\x1b[57442;5u\x1b[57441;6u\x1b[97;6u\x1b[97;6:3u\x1b[57441;5:3u\x1b[57442;1:3u"
        );
    }

    #[test]
    fn legacy_is_unchanged_with_no_flags() {
        let kb = legacy(Normal);
        let cases: &[(&str, &[u8])] = &[
            ("ctrl+j", b"\n"),
            ("Escape", b"\x1b"),
            ("Tab", b"\t"),
            ("alt+x", b"\x1bx"),
            ("Up", b"\x1b[A"),
            ("a", b"a"),
            ("F3", b"\x1bOR"),
            ("Insert", b"\x1b[2~"),
        ];
        for (combo, want) in cases {
            assert_eq!(encode(combo, kb).unwrap(), *want, "{combo}");
        }
        assert_eq!(encode_char('\n', kb), b"\n");
        assert_eq!(encode("shift+a", kb).unwrap_err().code, "bad_key");
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
