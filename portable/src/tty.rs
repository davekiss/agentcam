//! The tty capture: a command in a PTY `rec` owns, its output streamed to term.cast.

use crate::cast::{CastWriter, Header, Utf8Buffer};
use crate::error::{RecError, Result};
use crate::keys::{self, CursorMode, Keyboard};
use crate::model::{self, Button, Size, Track};
use crate::mouse::{mouse_off, Action, Protocol};
use crate::output::OutputLog;
use crate::protocol::{Cursor, ScreenText};
use crate::recorder::{Capture, FinishReason, Journal, Msg, Started};
use crate::terminal::Terminal;
use portable_pty::{CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a drag holds the button still before moving, as a hand does. Claude Code 2.1 keeps
/// only the newest of the input events that reach a plugin's surface within a frame, so motion
/// 15 ms after the press replaced it and the drag extended the old selection.
const PRESS_HOLD: Duration = Duration::from_millis(100);

/// The least time between the input of one command and the next, the default typing delay, so
/// `rec type` then `rec key Return` does not land both in one frame of the program.
const INPUT_GAP: Duration = Duration::from_millis(40);

/// What the reader thread writes: the emulated screen and the plain-text output.
struct Stream {
    screen: Mutex<vt100::Parser<Terminal>>,
    output: Mutex<OutputLog>,
    eof: AtomicBool,
}

struct Tty {
    stream: Arc<Stream>,
    /// Output cursor taken just before the last input byte reached the PTY, so whatever the
    /// program writes in response lands after it.
    input_mark: AtomicU64,
    last_input: Mutex<Option<Instant>>,
    /// Shared with the reader thread, which writes the terminal's replies to queries.
    pty_in: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    reader_done: Mutex<Receiver<()>>,
}

pub fn start(dir: &Path, command: &[String], size: Size, tx: Sender<Msg>) -> Result<Started> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| RecError::new("bad_args", "no command given after --"))?;
    let pty = portable_pty::native_pty_system()
        .openpty(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| RecError::new("pty", e.to_string()))?;
    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    cmd.env("TERM", "xterm-256color");
    // portable-pty defaults to $HOME when no cwd is given; run where the user ran rec.
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    let mut reader = pty
        .master
        .try_clone_reader()
        .map_err(|e| RecError::new("pty", e.to_string()))?;
    let writer = pty
        .master
        .take_writer()
        .map_err(|e| RecError::new("pty", e.to_string()))?;

    let cast_file = std::fs::File::create(dir.join(model::CAST_FILE))
        .map_err(|e| RecError::io("create term.cast", e))?;
    let mut cast = CastWriter::new(
        cast_file,
        &Header {
            version: 2,
            width: size.cols,
            height: size.rows,
            timestamp: chrono::Utc::now().timestamp(),
        },
    )
    .map_err(|e| RecError::io("write term.cast", e))?;

    let journal = Journal::start();
    let child = pty
        .slave
        .spawn_command(cmd)
        .map_err(|e| RecError::new("spawn_failed", format!("{program}: {e}")))?;
    // The child holds its own copy; keeping ours would stop EOF from ever arriving.
    drop(pty.slave);

    let (done_tx, done_rx) = mpsc::channel();
    let stream = Arc::new(Stream {
        screen: Mutex::new(vt100::Parser::new_with_callbacks(
            size.rows,
            size.cols,
            0,
            Terminal::default(),
        )),
        output: Mutex::new(OutputLog::new(OutputLog::CAP)),
        eof: AtomicBool::new(false),
    });
    let pty_in: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));
    {
        let tty = stream.clone();
        let pty_in = pty_in.clone();
        let t0 = journal.t0();
        std::thread::spawn(move || {
            let mut utf8 = Utf8Buffer::default();
            let mut buf = [0u8; 65536];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let t = t0.elapsed().as_secs_f64();
                let replies = {
                    let mut screen = tty.screen.lock().unwrap();
                    screen.process(&buf[..n]);
                    screen.callbacks_mut().take_replies()
                };
                // Replies are input to the program, so they go to the PTY and not to term.cast.
                if !replies.is_empty() {
                    let mut w = pty_in.lock().unwrap();
                    let _ = w.write_all(&replies).and_then(|_| w.flush());
                }
                tty.output.lock().unwrap().push(&buf[..n]);
                if let Err(e) = cast.output(t, &utf8.push(&buf[..n])) {
                    let _ = tx.send(Msg::failed(RecError::io("write term.cast", e)));
                    break;
                }
            }
            let _ = cast.output(t0.elapsed().as_secs_f64(), &utf8.finish());
            tty.eof.store(true, Ordering::Relaxed);
            let _ = done_tx.send(());
        });
    }
    Ok(Started {
        journal,
        capture: Box::new(Tty {
            stream,
            input_mark: AtomicU64::new(0),
            last_input: Mutex::new(None),
            pty_in,
            child: Mutex::new(child),
            reader_done: Mutex::new(done_rx),
        }),
        tracks: vec![Track::Term {
            file: model::CAST_FILE.into(),
            offset: 0.0,
        }],
        display: None,
    })
}

impl Tty {
    fn send_input(&self, bytes: &[u8]) -> Result<()> {
        self.write_input(bytes, true)
    }

    /// `mark: false` continues the input before it, such as the rest of a drag, so `wait --new`
    /// still covers what the program wrote since that input started.
    fn write_input(&self, bytes: &[u8], mark: bool) -> Result<()> {
        let mut w = self.pty_in.lock().unwrap();
        if mark {
            let cursor = self.stream.output.lock().unwrap().cursor();
            self.input_mark.store(cursor, Ordering::Relaxed);
        }
        *self.last_input.lock().unwrap() = Some(Instant::now());
        w.write_all(bytes)
            .and_then(|_| w.flush())
            .map_err(|e| RecError::io("write to pty", e))
    }

    /// Waits until `INPUT_GAP` has passed since the last input, at the start of each command.
    fn pace(&self) {
        let last = *self.last_input.lock().unwrap();
        if let Some(wait) =
            last.and_then(|t| (t + INPUT_GAP).checked_duration_since(Instant::now()))
        {
            std::thread::sleep(wait);
        }
    }

    fn keyboard(&self) -> Keyboard {
        let parser = self.stream.screen.lock().unwrap();
        let screen = parser.screen();
        Keyboard {
            cursor: if screen.application_cursor() {
                CursorMode::Application
            } else {
                CursorMode::Normal
            },
            kitty: parser.callbacks().kitty_flags(screen),
        }
    }

    fn mouse(&self) -> Protocol {
        let parser = self.stream.screen.lock().unwrap();
        let screen = parser.screen();
        Protocol {
            mode: screen.mouse_protocol_mode(),
            encoding: screen.mouse_protocol_encoding(),
        }
    }

    /// Sends the reports `mouse` gives each action, skipping what the mode leaves out.
    fn send_mouse(
        &self,
        mouse: Protocol,
        actions: &[(Action, (u32, u32))],
        mark: bool,
    ) -> Result<()> {
        let mut bytes = Vec::new();
        for &(action, cell) in actions {
            let cell = (cell.0 as u16, cell.1 as u16);
            bytes.extend(mouse.report(action, cell)?.unwrap_or_default());
        }
        if bytes.is_empty() {
            return Ok(());
        }
        self.write_input(&bytes, mark)
    }
}

impl Capture for Tty {
    fn kind(&self) -> &'static str {
        "tty"
    }

    fn type_text(&self, text: &str, delay: Duration, journal: &Journal) -> Result<()> {
        self.pace();
        for (i, ch) in text.chars().enumerate() {
            if !journal.live() {
                break;
            }
            if i > 0 && !delay.is_zero() {
                std::thread::sleep(delay);
            }
            self.send_input(&keys::encode_char(ch, self.keyboard()))?;
        }
        Ok(())
    }

    fn key(&self, combo: &str) -> Result<()> {
        self.pace();
        let bytes = keys::encode(combo, self.keyboard())?;
        self.send_input(&bytes)
    }

    fn click(&self, x: u32, y: u32, button: Button) -> Result<()> {
        let mouse = self.mouse();
        if !mouse.on() {
            return Err(mouse_off());
        }
        let cell = (x, y);
        self.pace();
        self.send_mouse(
            mouse,
            &[
                (Action::Press(button), cell),
                (Action::Release(button), cell),
            ],
            true,
        )
    }

    /// A motion report when the program asked for motion without a button held, and
    /// otherwise nothing, so a script that moves before clicking runs anywhere.
    fn move_to(&self, x: u32, y: u32) -> Result<()> {
        self.pace();
        self.send_mouse(self.mouse(), &[(Action::Move, (x, y))], true)
    }

    fn drag(&self, path: &[(u32, u32)], button: Button, step: Duration) -> Result<()> {
        let mouse = self.mouse();
        if !mouse.on() {
            return Err(mouse_off());
        }
        let (&first, rest) = path.split_first().expect("a drag has a start");
        self.pace();
        self.send_mouse(mouse, &[(Action::Press(button), first)], true)?;
        std::thread::sleep(PRESS_HOLD);
        for (i, &cell) in rest.iter().enumerate() {
            if i > 0 {
                std::thread::sleep(step);
            }
            self.send_mouse(mouse, &[(Action::Drag(button), cell)], false)?;
        }
        std::thread::sleep(step);
        let last = *path.last().expect("a drag has an end");
        self.send_mouse(mouse, &[(Action::Release(button), last)], false)
    }

    /// Bytes of output so far: it moves exactly when the program writes.
    fn fingerprint(&self) -> Result<u64> {
        Ok(self.stream.output.lock().unwrap().cursor())
    }

    fn screen_text(&self) -> Result<ScreenText> {
        let parser = self.stream.screen.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let (row, col) = screen.cursor_position();
        Ok(ScreenText {
            text: screen.contents(),
            cols,
            rows,
            cursor: Cursor { row, col },
        })
    }

    fn output(&self) -> Result<String> {
        let mark = self.input_mark.load(Ordering::Relaxed);
        Ok(self.stream.output.lock().unwrap().since(mark).to_string())
    }

    fn ended(&self) -> Option<FinishReason> {
        let exited = self.stream.eof.load(Ordering::Relaxed)
            || matches!(self.child.lock().unwrap().try_wait(), Ok(Some(_)));
        exited.then_some(FinishReason::ChildExited)
    }

    fn teardown(&self) -> Result<()> {
        {
            let mut child = self.child.lock().unwrap();
            if !matches!(child.try_wait(), Ok(Some(_))) {
                if let Some(pid) = child.process_id().and_then(|p| i32::try_from(p).ok()) {
                    // The child leads its own session, so this reaches anything it spawned too.
                    unsafe { libc::kill(-pid, libc::SIGHUP) };
                }
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        let _ = self
            .reader_done
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(2));
        Ok(())
    }
}
