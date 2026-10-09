//! The program's output as plain text, for `agentcam wait --new`.

/// The newest output with escape sequences removed, keeping at most `cap` bytes. A cursor is
/// a byte offset into everything ever written, so it stays valid as old text is dropped.
pub struct OutputLog {
    parser: vte::Parser,
    text: Plain,
    cap: usize,
}

struct Plain {
    start: u64,
    buf: String,
}

impl vte::Perform for Plain {
    fn print(&mut self, c: char) {
        self.buf.push(c);
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, b'\n' | b'\t') {
            self.buf.push(byte as char);
        }
    }
}

impl OutputLog {
    pub const CAP: usize = 1 << 20;

    pub fn new(cap: usize) -> OutputLog {
        OutputLog {
            parser: vte::Parser::new(),
            text: Plain {
                start: 0,
                buf: String::new(),
            },
            cap,
        }
    }

    /// Escape sequences split across reads are carried over by the parser.
    pub fn push(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.text, bytes);
        let buf = &mut self.text.buf;
        if buf.len() > self.cap {
            let mut cut = buf.len() - self.cap * 3 / 4;
            while !buf.is_char_boundary(cut) {
                cut += 1;
            }
            buf.drain(..cut);
            self.text.start += cut as u64;
        }
    }

    pub fn cursor(&self) -> u64 {
        self.text.start + self.text.buf.len() as u64
    }

    /// Text written at or after `cursor`, or everything still held if that much was dropped.
    pub fn since(&self, cursor: u64) -> &str {
        let buf = &self.text.buf;
        let mut from = (cursor.saturating_sub(self.text.start) as usize).min(buf.len());
        while !buf.is_char_boundary(from) {
            from += 1;
        }
        &buf[from..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_are_dropped_even_when_split_across_reads() {
        let mut log = OutputLog::new(OutputLog::CAP);
        log.push(b"\x1b[1;3");
        log.push(b"5mhello\x1b[0m\r\n\x1b]0;title\x07w\x1b[2Korld\n");
        assert_eq!(log.since(0), "hello\nworld\n");
    }

    #[test]
    fn since_returns_only_text_after_the_cursor() {
        let mut log = OutputLog::new(OutputLog::CAP);
        log.push(b"$ echo hi\r\n");
        let mark = log.cursor();
        log.push(b"hi\r\n$ ");
        assert_eq!(log.since(mark), "hi\n$ ");
        assert_eq!(log.since(log.cursor()), "");
    }

    #[test]
    fn old_text_is_dropped_but_cursors_keep_counting() {
        let mut log = OutputLog::new(16);
        log.push("ééééééééééé".as_bytes());
        let mark = log.cursor();
        assert_eq!(mark, 22);
        log.push(b"tail");
        assert!(log.since(0).len() <= 16, "held {:?}", log.since(0));
        assert!(log.since(0).ends_with("tail"));
        assert_eq!(log.since(mark), "tail");
    }
}
