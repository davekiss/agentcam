//! xterm mouse reports: the bytes a terminal writes to a program that turned mouse reporting
//! on, for the mode and encoding the program chose. Cells are 0-based (col, row).

use crate::error::{RecError, Result};
use crate::model::Button;
use vt100::{MouseProtocolEncoding as Encoding, MouseProtocolMode as Mode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Press(Button),
    Release(Button),
    /// Motion while `Button` is held.
    Drag(Button),
    /// Motion with no button held.
    Move,
}

impl Action {
    /// Whether a program in `mode` asked to hear about this.
    fn reported(self, mode: Mode) -> bool {
        match (self, mode) {
            (_, Mode::None) => false,
            (Action::Press(_), _) => true,
            (Action::Release(_), Mode::Press) => false,
            (Action::Release(_), _) => true,
            (Action::Drag(_), Mode::ButtonMotion | Mode::AnyMotion) => true,
            (Action::Move, Mode::AnyMotion) => true,
            (Action::Drag(_) | Action::Move, _) => false,
        }
    }
}

/// The program's mouse settings, read from the emulated screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Protocol {
    pub mode: Mode,
    pub encoding: Encoding,
}

pub fn mouse_off() -> RecError {
    RecError::new(
        "mouse_off",
        "the program has not enabled mouse reporting, so it would ignore the click",
    )
}

impl Protocol {
    pub fn on(self) -> bool {
        self.mode != Mode::None
    }

    /// The report for `action` at `cell`, or nothing when the mode does not report it.
    pub fn report(self, action: Action, cell: (u16, u16)) -> Result<Option<Vec<u8>>> {
        if !action.reported(self.mode) {
            return Ok(None);
        }
        let held = |b: Button| b.xterm() as u32;
        let (code, release) = match action {
            Action::Press(b) => (held(b), false),
            // The legacy encodings have no way to say which button let go.
            Action::Release(b) if self.encoding == Encoding::Sgr => (held(b), true),
            Action::Release(_) => (3, true),
            Action::Drag(b) => (held(b) + 32, false),
            Action::Move => (3 + 32, false),
        };
        let (x, y) = (cell.0 as u32 + 1, cell.1 as u32 + 1);
        let bytes = match self.encoding {
            Encoding::Sgr => {
                format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes()
            }
            Encoding::Default => {
                let byte =
                    |v: u32| u8::try_from(32 + v).map_err(|_| too_far(cell, 255 - 33, "X10"));
                vec![0x1b, b'[', b'M', byte(code)?, byte(x)?, byte(y)?]
            }
            Encoding::Utf8 => {
                let mut out = b"\x1b[M".to_vec();
                for v in [code, x, y] {
                    if 32 + v > 0x7ff {
                        return Err(too_far(cell, 0x7ff - 33, "UTF-8"));
                    }
                    let ch = char::from_u32(32 + v).expect("below the surrogates");
                    out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                }
                out
            }
        };
        Ok(Some(bytes))
    }
}

fn too_far(cell: (u16, u16), max: u32, name: &str) -> RecError {
    RecError::new(
        "bad_args",
        format!(
            "cell ({}, {}) is beyond what the program's {name} mouse encoding can report \
             (columns and rows 0 to {max})",
            cell.0, cell.1
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SGR: Protocol = Protocol {
        mode: Mode::AnyMotion,
        encoding: Encoding::Sgr,
    };
    const X10: Protocol = Protocol {
        mode: Mode::AnyMotion,
        encoding: Encoding::Default,
    };

    fn report(p: Protocol, a: Action, cell: (u16, u16)) -> Vec<u8> {
        p.report(a, cell).unwrap().expect("reported")
    }

    #[test]
    fn sgr_reports_are_one_based_and_name_the_released_button() {
        assert_eq!(
            report(SGR, Action::Press(Button::Left), (0, 0)),
            b"\x1b[<0;1;1M"
        );
        assert_eq!(
            report(SGR, Action::Release(Button::Right), (0, 0)),
            b"\x1b[<2;1;1m"
        );
        assert_eq!(
            report(SGR, Action::Drag(Button::Middle), (299, 99)),
            b"\x1b[<33;300;100M"
        );
        assert_eq!(report(SGR, Action::Move, (4, 2)), b"\x1b[<35;5;3M");
    }

    #[test]
    fn x10_reports_offset_every_byte_by_32_and_release_as_button_3() {
        assert_eq!(
            report(X10, Action::Press(Button::Left), (0, 0)),
            b"\x1b[M\x20\x21\x21"
        );
        assert_eq!(
            report(X10, Action::Release(Button::Left), (0, 0)),
            b"\x1b[M\x23\x21\x21"
        );
        assert_eq!(
            report(X10, Action::Drag(Button::Left), (9, 4)),
            b"\x1b[M\x40\x2a\x25"
        );
        assert_eq!(
            report(X10, Action::Press(Button::Right), (222, 0)),
            b"\x1b[M\x22\xff\x21"
        );
    }

    #[test]
    fn x10_cannot_reach_past_column_222() {
        let err = X10
            .report(Action::Press(Button::Left), (223, 0))
            .unwrap_err();
        assert_eq!(err.code, "bad_args");
        let err = X10
            .report(Action::Press(Button::Left), (0, 223))
            .unwrap_err();
        assert_eq!(err.code, "bad_args");
    }

    #[test]
    fn utf8_reports_encode_large_coordinates_as_two_byte_characters() {
        let utf8 = Protocol {
            encoding: Encoding::Utf8,
            ..X10
        };
        assert_eq!(
            report(utf8, Action::Press(Button::Left), (0, 0)),
            b"\x1b[M\x20\x21\x21"
        );
        // 32 + 224 + 1 = 257 = U+0101.
        assert_eq!(
            report(utf8, Action::Press(Button::Left), (224, 0)),
            "\x1b[M\u{20}\u{101}\u{21}".as_bytes()
        );
    }

    #[test]
    fn the_mode_decides_what_gets_reported() {
        let cell = (1, 1);
        let reported = |mode, a| {
            Protocol {
                mode,
                encoding: Encoding::Sgr,
            }
            .report(a, cell)
            .unwrap()
            .is_some()
        };
        let all = [
            Action::Press(Button::Left),
            Action::Release(Button::Left),
            Action::Drag(Button::Left),
            Action::Move,
        ];
        let table = [
            (Mode::None, [false, false, false, false]),
            (Mode::Press, [true, false, false, false]),
            (Mode::PressRelease, [true, true, false, false]),
            (Mode::ButtonMotion, [true, true, true, false]),
            (Mode::AnyMotion, [true, true, true, true]),
        ];
        for (mode, want) in table {
            let got = all.map(|a| reported(mode, a));
            assert_eq!(got, want, "{mode:?}");
        }
    }
}
