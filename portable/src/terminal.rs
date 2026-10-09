//! What `agentcam` answers as the terminal a tty program runs in: replies to the queries a program
//! writes to find out what it is talking to, and the kitty keyboard flags it pushes. It hooks
//! the vt100 emulator, so a query split across reads, or written mid-frame, is seen in order with
//! the screen state it asks about.

use vt100::{MouseProtocolEncoding, MouseProtocolMode, Screen};

/// Each kitty keyboard flags stack is capped, as the spec asks, with the oldest entry evicted.
const KITTY_STACK_MAX: usize = 16;

/// A control sequence from the program that the emulator leaves to us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Query(Query),
    Kitty(KittyOp),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Query {
    /// `CSI c`
    PrimaryAttributes,
    /// `CSI > c`
    SecondaryAttributes,
    /// `CSI > q`
    Version,
    /// `CSI 5 n`
    Status,
    /// `CSI 6 n`, or `CSI ? 6 n` for the DEC form.
    CursorPosition { dec: bool },
    /// `CSI ? u`
    KittyFlags,
    /// `CSI ? Ps $ p`
    PrivateMode(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KittyOp {
    /// `CSI > flags u`
    Push(u8),
    /// `CSI < n u`
    Pop(u16),
    /// `CSI = flags ; mode u`
    Set(u8, SetMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetMode {
    Replace,
    Or,
    AndNot,
}

/// The one table of recognized sequences: private marker, second intermediate, final byte, and
/// the first parameter, to the control it means.
fn recognize(i1: Option<u8>, i2: Option<u8>, params: &[&[u16]], c: char) -> Option<Control> {
    let p = |n: usize| params.get(n).and_then(|p| p.first()).copied();
    let flags = |n: usize| p(n).unwrap_or(0).min(u8::MAX as u16) as u8;
    Some(match (i1, i2, c, p(0)) {
        (None, None, 'c', None | Some(0)) => Control::Query(Query::PrimaryAttributes),
        (Some(b'>'), None, 'c', None | Some(0)) => Control::Query(Query::SecondaryAttributes),
        (Some(b'>'), None, 'q', None | Some(0)) => Control::Query(Query::Version),
        (None, None, 'n', Some(5)) => Control::Query(Query::Status),
        (None, None, 'n', Some(6)) => Control::Query(Query::CursorPosition { dec: false }),
        (Some(b'?'), None, 'n', Some(6)) => Control::Query(Query::CursorPosition { dec: true }),
        (Some(b'?'), None, 'u', _) => Control::Query(Query::KittyFlags),
        (Some(b'?'), Some(b'$'), 'p', Some(mode)) => Control::Query(Query::PrivateMode(mode)),
        (Some(b'>'), None, 'u', _) => Control::Kitty(KittyOp::Push(flags(0))),
        (Some(b'<'), None, 'u', n) => Control::Kitty(KittyOp::Pop(n.unwrap_or(1).max(1))),
        (Some(b'='), None, 'u', _) => {
            let mode = match p(1).unwrap_or(1) {
                1 => SetMode::Replace,
                2 => SetMode::Or,
                3 => SetMode::AndNot,
                _ => return None,
            };
            Control::Kitty(KittyOp::Set(flags(0), mode))
        }
        _ => return None,
    })
}

/// One screen's kitty keyboard state: the flags in effect and the ones pushes saved.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct KittyStack {
    current: u8,
    saved: Vec<u8>,
}

impl KittyStack {
    fn apply(&mut self, op: KittyOp) {
        match op {
            KittyOp::Push(flags) => {
                if self.saved.len() == KITTY_STACK_MAX {
                    self.saved.remove(0);
                }
                self.saved.push(self.current);
                self.current = flags;
            }
            KittyOp::Pop(n) => {
                for _ in 0..n {
                    match self.saved.pop() {
                        Some(flags) => self.current = flags,
                        None => {
                            self.current = 0;
                            break;
                        }
                    }
                }
            }
            KittyOp::Set(flags, SetMode::Replace) => self.current = flags,
            KittyOp::Set(flags, SetMode::Or) => self.current |= flags,
            KittyOp::Set(flags, SetMode::AndNot) => self.current &= !flags,
        }
    }
}

/// The terminal side of the PTY, as the emulator's callbacks: kitty flags per screen, and the
/// replies owed to the program, which the reader writes back into the PTY after each read.
#[derive(Debug, Default)]
pub struct Terminal {
    main: KittyStack,
    alternate: KittyStack,
    replies: Vec<u8>,
}

impl Terminal {
    /// The kitty keyboard flags of the screen the program is on now.
    pub fn kitty_flags(&self, screen: &Screen) -> u8 {
        self.stack(screen).current
    }

    /// The replies owed since the last call, in the order the queries arrived.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    fn stack(&self, screen: &Screen) -> &KittyStack {
        if screen.alternate_screen() {
            &self.alternate
        } else {
            &self.main
        }
    }

    fn reply(&self, query: Query, screen: &Screen) -> Vec<u8> {
        match query {
            // A VT220-class terminal with ANSI color, as xterm-compatible terminals answer.
            Query::PrimaryAttributes => b"\x1b[?62;22c".to_vec(),
            Query::SecondaryAttributes => b"\x1b[>1;10;0c".to_vec(),
            Query::Version => {
                format!("\x1bP>|agentcam {}\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes()
            }
            Query::Status => b"\x1b[0n".to_vec(),
            Query::CursorPosition { dec } => {
                let (row, col) = screen.cursor_position();
                let mark = if dec { "?" } else { "" };
                format!("\x1b[{mark}{};{}R", row + 1, col + 1).into_bytes()
            }
            Query::KittyFlags => format!("\x1b[?{}u", self.kitty_flags(screen)).into_bytes(),
            Query::PrivateMode(mode) => {
                let state = match private_mode(screen, mode) {
                    None => 0,
                    Some(true) => 1,
                    Some(false) => 2,
                };
                format!("\x1b[?{mode};{state}$y").into_bytes()
            }
        }
    }
}

/// Whether a DEC private mode the emulator tracks is set, or None for one it does not know.
fn private_mode(screen: &Screen, mode: u16) -> Option<bool> {
    let mouse = |m: MouseProtocolMode| screen.mouse_protocol_mode() == m;
    let encoding = |e: MouseProtocolEncoding| screen.mouse_protocol_encoding() == e;
    Some(match mode {
        1 => screen.application_cursor(),
        25 => !screen.hide_cursor(),
        47 | 1047 | 1049 => screen.alternate_screen(),
        9 => mouse(MouseProtocolMode::Press),
        1000 => mouse(MouseProtocolMode::PressRelease),
        1002 => mouse(MouseProtocolMode::ButtonMotion),
        1003 => mouse(MouseProtocolMode::AnyMotion),
        1005 => encoding(MouseProtocolEncoding::Utf8),
        1006 => encoding(MouseProtocolEncoding::Sgr),
        2004 => screen.bracketed_paste(),
        _ => return None,
    })
}

impl vt100::Callbacks for Terminal {
    fn unhandled_csi(
        &mut self,
        screen: &mut Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        match recognize(i1, i2, params, c) {
            Some(Control::Query(query)) => {
                let reply = self.reply(query, screen);
                self.replies.extend(reply);
            }
            Some(Control::Kitty(op)) => {
                let stack = if screen.alternate_screen() {
                    &mut self.alternate
                } else {
                    &mut self.main
                };
                stack.apply(op);
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> vt100::Parser<Terminal> {
        vt100::Parser::new_with_callbacks(24, 80, 0, Terminal::default())
    }

    /// Feeds `chunks` as separate reads and returns everything the terminal replied.
    fn replies(p: &mut vt100::Parser<Terminal>, chunks: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in chunks {
            p.process(chunk);
            out.extend(p.callbacks_mut().take_replies());
        }
        out
    }

    fn flags(p: &vt100::Parser<Terminal>) -> u8 {
        p.callbacks().kitty_flags(p.screen())
    }

    #[test]
    fn each_query_gets_its_reply() {
        let version = format!("\x1bP>|agentcam {}\x1b\\", env!("CARGO_PKG_VERSION"));
        let cases: &[(&[u8], &[u8])] = &[
            (b"\x1b[c", b"\x1b[?62;22c"),
            (b"\x1b[0c", b"\x1b[?62;22c"),
            (b"\x1b[>c", b"\x1b[>1;10;0c"),
            (b"\x1b[>0q", version.as_bytes()),
            (b"\x1b[5n", b"\x1b[0n"),
            (b"\x1b[6n", b"\x1b[1;1R"),
            (b"\x1b[?6n", b"\x1b[?1;1R"),
            (b"\x1b[?u", b"\x1b[?0u"),
            (b"\x1b[?2026$p", b"\x1b[?2026;0$y"),
            (b"\x1b[?2004$p", b"\x1b[?2004;2$y"),
            (b"\x1b[?25$p", b"\x1b[?25;1$y"),
        ];
        for (query, want) in cases {
            let got = replies(&mut parser(), &[query]);
            assert_eq!(
                String::from_utf8_lossy(&got),
                String::from_utf8_lossy(want),
                "{query:?}"
            );
        }
    }

    #[test]
    fn other_sequences_get_no_reply() {
        let got = replies(
            &mut parser(),
            &[b"hello \x1b[1;31mred\x1b[0m \x1b[2J\x1b[H\x1b[u\x1b[7n\x1b[>4;2m"],
        );
        assert_eq!(got, b"");
    }

    #[test]
    fn a_query_split_across_reads_is_answered_once_whole() {
        let mut p = parser();
        let got = replies(&mut p, &[b"ab\x1b", b"[", b"?", b"u", b"\x1b[>", b"0q"]);
        let want = format!("\x1b[?0u\x1bP>|agentcam {}\x1b\\", env!("CARGO_PKG_VERSION"));
        assert_eq!(String::from_utf8_lossy(&got), want);
    }

    #[test]
    fn cursor_position_is_where_the_query_falls_in_the_output() {
        // The query comes after "hello" on row 3 and before more output moves the cursor.
        let got = replies(&mut parser(), &[b"\x1b[3;1Hhello\x1b[6n\r\nmore"]);
        assert_eq!(got, b"\x1b[3;6R");
    }

    #[test]
    fn replies_keep_query_order() {
        // Claude Code's startup probe: DA1, the kitty query twice, and XTVERSION.
        let got = replies(&mut parser(), &[b"\x1b[?u\x1b[c\x1b[?u\x1b[>0q"]);
        let want = format!(
            "\x1b[?0u\x1b[?62;22c\x1b[?0u\x1bP>|agentcam {}\x1b\\",
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(String::from_utf8_lossy(&got), want);
    }

    #[test]
    fn kitty_push_pop_and_set_follow_the_spec() {
        let mut p = parser();
        p.process(b"\x1b[>1u");
        assert_eq!(flags(&p), 1);
        p.process(b"\x1b[>5u");
        assert_eq!(flags(&p), 5);
        p.process(b"\x1b[=8;2u");
        assert_eq!(flags(&p), 13, "mode 2 sets the given bits");
        p.process(b"\x1b[=4;3u");
        assert_eq!(flags(&p), 9, "mode 3 clears the given bits");
        p.process(b"\x1b[=2u");
        assert_eq!(flags(&p), 2, "mode 1, the default, replaces");
        assert_eq!(replies(&mut p, &[b"\x1b[?u"]), b"\x1b[?2u");
        p.process(b"\x1b[<u");
        assert_eq!(flags(&p), 1, "a pop restores what the push saved");
        p.process(b"\x1b[<5u");
        assert_eq!(
            flags(&p),
            0,
            "a pop that empties the stack resets the flags"
        );
        p.process(b"\x1b[<u");
        assert_eq!(flags(&p), 0);
    }

    #[test]
    fn kitty_push_without_flags_pushes_zero_and_the_stack_is_capped() {
        let mut p = parser();
        p.process(b"\x1b[>3u\x1b[>u");
        assert_eq!(flags(&p), 0);
        p.process(b"\x1b[<u");
        assert_eq!(flags(&p), 3);
        let mut stack = KittyStack::default();
        for f in 1..=(KITTY_STACK_MAX as u8 + 4) {
            stack.apply(KittyOp::Push(f));
        }
        assert_eq!(stack.saved.len(), KITTY_STACK_MAX);
        assert_eq!(stack.saved[0], 4, "the oldest saved entries were evicted");
    }

    #[test]
    fn main_and_alternate_screens_keep_separate_stacks() {
        let mut p = parser();
        p.process(b"\x1b[>1u");
        p.process(b"\x1b[?1049h\x1b[>15u");
        assert_eq!(flags(&p), 15);
        p.process(b"\x1b[?1049l");
        assert_eq!(flags(&p), 1);
        p.process(b"\x1b[?1049h");
        assert_eq!(flags(&p), 15);
        p.process(b"\x1b[<u\x1b[?1049l");
        assert_eq!(
            flags(&p),
            1,
            "popping on the alternate screen left the main stack alone"
        );
    }
}
