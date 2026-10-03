//! The one recording code path: `rec record`. `rec start` runs this same thing detached.

use crate::cast::{CastWriter, Header, Utf8Buffer};
use crate::error::{RecError, Result};
use crate::keys::{self, CursorMode};
use crate::model::{
    self, round_t, Active, Event, Marker, Size, Source, Take, TakeStatus, TimedEvent, Timeline,
    Track,
};
use crate::paths;
use crate::protocol::{self, Cursor, Request, Response, ScreenText};
use portable_pty::{CommandBuilder, PtySize};
use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct RecordOptions {
    pub take_dir: PathBuf,
    pub size: Size,
    pub duration: Option<f64>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Recording,
    Finalizing,
    Finished,
    Failed,
}

#[derive(Debug)]
enum FinishReason {
    Stop,
    ChildExited,
    Duration,
    Signal,
    Failed(RecError),
}

enum Msg {
    /// The first one received ends the take; later ones only add stop waiters.
    Finish {
        reason: FinishReason,
        waiter: Option<UnixStream>,
    },
    ReaderDone,
}

struct Shared {
    t0: Instant,
    take_dir: PathBuf,
    source: Source,
    phase: Mutex<Phase>,
    screen: Mutex<vt100::Parser>,
    pty_in: Mutex<Box<dyn Write + Send>>,
    events: Mutex<Vec<TimedEvent>>,
    markers: Mutex<std::fs::File>,
    tx: Sender<Msg>,
}

impl Shared {
    fn now(&self) -> f64 {
        round_t(self.t0.elapsed().as_secs_f64())
    }

    fn recording(&self) -> bool {
        *self.phase.lock().unwrap() == Phase::Recording
    }

    fn send_input(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self.pty_in.lock().unwrap();
        w.write_all(bytes)
            .and_then(|_| w.flush())
            .map_err(|e| RecError::io("write to pty", e))
    }

    fn log(&self, t: f64, event: Event) {
        self.events.lock().unwrap().push(TimedEvent { t, event });
    }
}

pub fn record(opts: RecordOptions) -> Result<Take> {
    let pid = std::process::id();
    if let Some(active) = paths::read_active()? {
        if active.pid != pid {
            return Err(paths::already_recording(&active));
        }
    }

    let dir = opts.take_dir.clone();
    let source = Source::Tty {
        command: opts.command.clone(),
        size: opts.size,
    };
    let created = chrono::Utc::now();
    let mut take = Take {
        version: 2,
        id: paths::take_id(&dir),
        created_at: created.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        status: TakeStatus::Recording,
        duration: None,
        source: source.clone(),
        tracks: vec![Track::Term {
            file: model::CAST_FILE.into(),
            offset: 0.0,
        }],
        error: None,
    };
    take.write(&dir)?;
    paths::write_active(&Active {
        pid,
        take: dir.clone(),
        started_at: take.created_at.clone(),
        source: source.clone(),
    })?;

    if let Err(err) = run(&mut take, &dir, &opts) {
        // run only fails before the child starts, or when the final take.json write fails.
        take.status = TakeStatus::Failed;
        take.error = Some(err);
        let _ = take.write(&dir);
    }
    let _ = std::fs::remove_file(protocol::socket_path(&dir));
    paths::clear_active(pid);
    Ok(take)
}

/// Records until the first finish reason, then finalizes `take` on disk and answers
/// every `rec stop` that is waiting on it.
fn run(take: &mut Take, dir: &Path, opts: &RecordOptions) -> Result<()> {
    let size = opts.size;
    let (program, args) = opts
        .command
        .split_first()
        .ok_or_else(|| RecError::new("bad_args", "no command given after --"))?;

    let listener = protocol::bind(&protocol::socket_path(dir))?;
    let interrupted = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, interrupted.clone())
            .map_err(|e| RecError::io("install signal handler", e))?;
    }

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
    let markers = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(model::MARKERS_FILE))
        .map_err(|e| RecError::io("open markers.jsonl", e))?;

    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(Shared {
        t0: Instant::now(),
        take_dir: dir.to_path_buf(),
        source: take.source.clone(),
        phase: Mutex::new(Phase::Recording),
        screen: Mutex::new(vt100::Parser::new(size.rows, size.cols, 0)),
        pty_in: Mutex::new(writer),
        events: Mutex::new(Vec::new()),
        markers: Mutex::new(markers),
        tx: tx.clone(),
    });

    let mut child = pty
        .slave
        .spawn_command(cmd)
        .map_err(|e| RecError::new("spawn_failed", format!("{program}: {e}")))?;
    // The child holds its own copy; keeping ours would stop EOF from ever arriving.
    drop(pty.slave);

    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut utf8 = Utf8Buffer::default();
            let mut buf = [0u8; 65536];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let t = shared.t0.elapsed().as_secs_f64();
                shared.screen.lock().unwrap().process(&buf[..n]);
                if let Err(e) = cast.output(t, &utf8.push(&buf[..n])) {
                    let _ = shared.tx.send(Msg::Finish {
                        reason: FinishReason::Failed(RecError::io("write term.cast", e)),
                        waiter: None,
                    });
                    break;
                }
            }
            let t = shared.t0.elapsed().as_secs_f64();
            let _ = cast.output(t, &utf8.finish());
            let _ = shared.tx.send(Msg::ReaderDone);
        });
    }

    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = shared.clone();
                std::thread::spawn(move || handle_connection(stream, &shared));
            }
        });
    }

    eprintln!("rec: recording {}", dir.display());

    let deadline = opts.duration.map(|s| shared.t0 + Duration::from_secs_f64(s.max(0.0)));
    let mut waiters = Vec::new();
    let mut reader_done = false;
    let reason = loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Msg::Finish { reason, waiter }) => {
                waiters.extend(waiter);
                break reason;
            }
            Ok(Msg::ReaderDone) => {
                reader_done = true;
                break FinishReason::ChildExited;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => unreachable!("shared holds a sender"),
        }
        if interrupted.load(Ordering::Relaxed) {
            break FinishReason::Signal;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break FinishReason::Duration;
        }
        if matches!(child.try_wait(), Ok(Some(_))) {
            break FinishReason::ChildExited;
        }
    };
    let duration = shared.now();
    *shared.phase.lock().unwrap() = Phase::Finalizing;
    eprintln!("rec: finishing ({reason:?}) at {duration}s");

    if !matches!(child.try_wait(), Ok(Some(_))) {
        if let Some(pid) = child.process_id().and_then(|p| i32::try_from(p).ok()) {
            // The child leads its own session, so this reaches anything it spawned too.
            unsafe { libc::kill(-pid, libc::SIGHUP) };
        }
        let _ = child.kill();
    }
    let _ = child.wait();

    let drain_until = Instant::now() + Duration::from_secs(2);
    while !reader_done {
        let left = drain_until.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::ReaderDone) => reader_done = true,
            Ok(Msg::Finish { waiter, .. }) => waiters.extend(waiter),
            Err(_) => break,
        }
    }

    let failure = match reason {
        FinishReason::Failed(e) => Some(e),
        _ => None,
    };
    let timeline = Timeline {
        version: 2,
        events: merged_events(&shared.events.lock().unwrap(), dir),
    };
    let wrote = model::write_json_atomic(&dir.join(model::TIMELINE_FILE), &timeline);
    let failure = failure.or(wrote.err());

    take.status = if failure.is_some() {
        TakeStatus::Failed
    } else {
        TakeStatus::Finished
    };
    take.duration = Some(duration);
    take.error = failure;
    take.write(dir)?;
    *shared.phase.lock().unwrap() = match take.status {
        TakeStatus::Failed => Phase::Failed,
        _ => Phase::Finished,
    };

    while let Ok(msg) = rx.try_recv() {
        if let Msg::Finish { waiter, .. } = msg {
            waiters.extend(waiter);
        }
    }
    let reply = Response::Stopped {
        take: Box::new(take.clone()),
    };
    for w in &waiters {
        let _ = protocol::write_message(w, &reply);
    }
    Ok(())
}

/// Recorder-sent input from memory plus markers from markers.jsonl, in time order.
fn merged_events(sent: &[TimedEvent], dir: &Path) -> Vec<TimedEvent> {
    let mut events = sent.to_vec();
    if let Ok(f) = std::fs::File::open(dir.join(model::MARKERS_FILE)) {
        for line in std::io::BufReader::new(f).lines().map_while(|l| l.ok()) {
            if let Ok(m) = serde_json::from_str::<Marker>(&line) {
                events.push(TimedEvent {
                    t: m.t,
                    event: Event::Marker { label: m.label },
                });
            }
        }
    }
    events.sort_by(|a, b| a.t.total_cmp(&b.t));
    events
}

fn handle_connection(stream: UnixStream, shared: &Shared) {
    let req = match protocol::read_message::<Request>(&stream) {
        Ok(r) => r,
        Err(error) => {
            let _ = protocol::write_message(&stream, &Response::Error { error });
            return;
        }
    };
    if let Request::Stop = req {
        // The main thread replies once the take is finalized.
        let _ = shared.tx.send(Msg::Finish {
            reason: FinishReason::Stop,
            waiter: Some(stream),
        });
        return;
    }
    let resp = if shared.recording() {
        serve(req, shared).unwrap_or_else(|error| Response::Error { error })
    } else {
        Response::Error {
            error: RecError::new("not_recording", "the take is finishing"),
        }
    };
    let _ = protocol::write_message(&stream, &resp);
}

fn serve(req: Request, shared: &Shared) -> Result<Response> {
    match req {
        Request::Status => Ok(Response::Status {
            take: shared.take_dir.clone(),
            elapsed: shared.now(),
            source: shared.source.clone(),
        }),
        Request::Type {
            text,
            delay_ms,
            secret,
        } => {
            let t = shared.now();
            shared.log(
                t,
                Event::Type {
                    text: (!secret).then(|| text.clone()),
                    redacted: secret,
                },
            );
            let mut buf = [0u8; 4];
            for (i, ch) in text.chars().enumerate() {
                if !shared.recording() {
                    break;
                }
                if i > 0 && delay_ms > 0 {
                    std::thread::sleep(Duration::from_millis(delay_ms));
                }
                shared.send_input(ch.encode_utf8(&mut buf).as_bytes())?;
            }
            Ok(Response::Sent { t })
        }
        Request::Key { combo } => {
            let mode = if shared.screen.lock().unwrap().screen().application_cursor() {
                CursorMode::Application
            } else {
                CursorMode::Normal
            };
            let bytes = keys::encode(&combo, mode)?;
            let t = shared.now();
            shared.send_input(&bytes)?;
            shared.log(t, Event::Key { key: combo });
            Ok(Response::Sent { t })
        }
        Request::Screen => {
            let parser = shared.screen.lock().unwrap();
            let screen = parser.screen();
            let (rows, cols) = screen.size();
            let (row, col) = screen.cursor_position();
            Ok(Response::Screen {
                t: shared.now(),
                screen: ScreenText {
                    text: screen.contents(),
                    cols,
                    rows,
                    cursor: Cursor { row, col },
                },
            })
        }
        Request::Mark { label } => {
            let t = shared.now();
            let mut line = serde_json::to_string(&Marker {
                t,
                label: label.clone(),
            })
            .expect("serializable");
            line.push('\n');
            let mut f = shared.markers.lock().unwrap();
            f.write_all(line.as_bytes())
                .and_then(|_| f.flush())
                .map_err(|e| RecError::io("append markers.jsonl", e))?;
            Ok(Response::Marked {
                take: shared.take_dir.clone(),
                t,
                label,
            })
        }
        Request::Stop => unreachable!("handled before dispatch"),
    }
}
