//! The one recording code path: `rec record`. `rec start` runs this same thing detached.
//! The lifecycle, the control socket, and the timeline live here; what is recorded and how
//! input reaches it is a `Capture` (a PTY in tty.rs, a virtual display in x11.rs).

use crate::error::{RecError, Result};
use crate::model::{
    self, normalize, round_t, Active, Button, Event, Marker, Source, Take, TakeStatus,
    TimedEvent, Timeline, Track,
};
use crate::paths;
use crate::protocol::{self, Request, Response, ScreenText};
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct RecordOptions {
    pub take_dir: PathBuf,
    pub duration: Option<f64>,
    pub source: Source,
}

/// The take clock and the input log. A capture creates it at the moment its first track
/// starts, which is t0.
pub struct Journal {
    t0: Instant,
    live: AtomicBool,
    events: Mutex<Vec<TimedEvent>>,
}

impl Journal {
    pub fn start() -> Arc<Journal> {
        Arc::new(Journal {
            t0: Instant::now(),
            live: AtomicBool::new(true),
            events: Mutex::new(Vec::new()),
        })
    }

    pub fn t0(&self) -> Instant {
        self.t0
    }

    pub fn now(&self) -> f64 {
        round_t(self.t0.elapsed().as_secs_f64())
    }

    /// False once the take starts finishing; long inputs check it between characters.
    pub fn live(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }

    pub fn log(&self, t: f64, event: Event) {
        self.events.lock().unwrap().push(TimedEvent { t, event });
    }
}

#[derive(Debug)]
pub enum FinishReason {
    Stop,
    ChildExited,
    Duration,
    Signal,
    Failed(RecError),
}

pub fn not_supported(what: &str, source: &str) -> RecError {
    RecError::new(
        "not_supported",
        format!("{what} is not supported on {source} takes"),
    )
}

/// One source kind's recording: its children, its tracks, and how input reaches it.
pub trait Capture: Send + Sync {
    /// "tty" or "x11", for error messages.
    fn kind(&self) -> &'static str;

    /// Sends `text` one character at a time, `delay` apart, stopping early when the take ends.
    fn type_text(&self, text: &str, delay: Duration, journal: &Journal) -> Result<()>;

    fn key(&self, combo: &str) -> Result<()>;

    /// A value that changes whenever what the take records changes, for `rec wait --idle`.
    fn fingerprint(&self) -> Result<u64>;

    fn click(&self, _x: u32, _y: u32, _button: Button) -> Result<()> {
        Err(not_supported("click", self.kind()))
    }

    fn move_to(&self, _x: u32, _y: u32) -> Result<()> {
        Err(not_supported("move", self.kind()))
    }

    fn screen_text(&self) -> Result<ScreenText> {
        Err(not_supported(
            "reading the screen as text (OCR comes later)",
            self.kind(),
        ))
    }

    fn output(&self) -> Result<String> {
        Err(not_supported("wait --new", self.kind()))
    }

    /// Writes a PNG of what is on screen and returns its size.
    fn snapshot(&self, _png: &Path) -> Result<(u32, u32)> {
        Err(not_supported("screen --png", self.kind()))
    }

    /// Polled by the recorder: why the take should end on its own, if it should.
    fn ended(&self) -> Option<FinishReason>;

    /// Stops every child and finalizes the track files. Runs once, and must leave no process
    /// behind even when it reports an error.
    fn teardown(&self) -> Result<()>;
}

/// What a capture hands back once it is recording.
pub struct Started {
    pub journal: Arc<Journal>,
    pub capture: Box<dyn Capture>,
    pub tracks: Vec<Track>,
    /// The X display an x11 take records, for `rec start` to hand back.
    pub display: Option<String>,
}

struct Shared {
    journal: Arc<Journal>,
    take_dir: PathBuf,
    source: Source,
    display: Option<String>,
    capture: Box<dyn Capture>,
    markers: Mutex<std::fs::File>,
    tx: Sender<Msg>,
}

/// Asks the recorder to finish. The first one received ends the take; later ones only add
/// stop waiters.
pub struct Msg {
    reason: FinishReason,
    waiter: Option<UnixStream>,
}

impl Msg {
    pub fn failed(error: RecError) -> Msg {
        Msg {
            reason: FinishReason::Failed(error),
            waiter: None,
        }
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
    let created = chrono::Utc::now();
    let mut take = Take {
        version: 2,
        id: paths::take_id(&dir),
        created_at: created.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        status: TakeStatus::Recording,
        duration: None,
        source: opts.source.clone(),
        tracks: vec![],
        error: None,
    };
    take.write(&dir)?;
    let mut active = Active {
        pid,
        take: dir.clone(),
        started_at: take.created_at.clone(),
        source: opts.source.clone(),
        display: None,
    };
    paths::write_active(&active)?;

    if let Err(err) = run(&mut take, &mut active, &dir, &opts) {
        // run only fails before the capture starts, or when the final take.json write fails.
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
fn run(take: &mut Take, active: &mut Active, dir: &Path, opts: &RecordOptions) -> Result<()> {
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
    let markers = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(model::MARKERS_FILE))
        .map_err(|e| RecError::io("open markers.jsonl", e))?;

    let (tx, rx) = mpsc::channel();
    let started = match &opts.source {
        Source::Tty { command, size } => crate::tty::start(dir, command, *size, tx.clone())?,
        Source::X11 { command, frame } => crate::x11::start(dir, command, *frame)?,
    };
    take.tracks = started.tracks;
    take.write(dir)?;
    if started.display.is_some() {
        active.display = started.display.clone();
        paths::write_active(active)?;
    }
    let shared = Arc::new(Shared {
        journal: started.journal,
        take_dir: dir.to_path_buf(),
        source: take.source.clone(),
        display: started.display,
        capture: started.capture,
        markers: Mutex::new(markers),
        tx,
    });

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

    let journal = &shared.journal;
    let deadline = opts
        .duration
        .map(|s| journal.t0() + Duration::from_secs_f64(s.max(0.0)));
    let mut waiters = Vec::new();
    let reason = loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Msg { reason, waiter }) => {
                waiters.extend(waiter);
                break reason;
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
        if let Some(reason) = shared.capture.ended() {
            break reason;
        }
    };
    let duration = journal.now();
    journal.live.store(false, Ordering::Relaxed);
    eprintln!("rec: finishing ({reason:?}) at {duration}s");

    let torn = shared.capture.teardown();
    let failure = match reason {
        FinishReason::Failed(e) => Some(e),
        _ => None,
    };
    let failure = failure.or(torn.err());
    let timeline = Timeline {
        version: 2,
        events: merged_events(&journal.events.lock().unwrap(), dir),
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

    while let Ok(msg) = rx.try_recv() {
        waiters.extend(msg.waiter);
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
        let _ = shared.tx.send(Msg {
            reason: FinishReason::Stop,
            waiter: Some(stream),
        });
        return;
    }
    let resp = if shared.journal.live() {
        serve(req, shared).unwrap_or_else(|error| Response::Error { error })
    } else {
        Response::Error {
            error: RecError::new("not_recording", "the take is finishing"),
        }
    };
    let _ = protocol::write_message(&stream, &resp);
}

/// A pixel position on an x11 take's screen, checked against the frame.
fn on_screen(source: &Source, x: u32, y: u32) -> Result<(f64, f64)> {
    let Source::X11 { frame, .. } = source else {
        return Err(not_supported("click and move", "tty"));
    };
    if x >= frame.width || y >= frame.height {
        return Err(RecError::new(
            "bad_args",
            format!(
                "({x}, {y}) is outside the {}x{} screen",
                frame.width, frame.height
            ),
        ));
    }
    Ok((normalize(x as f64, frame.width), normalize(y as f64, frame.height)))
}

fn serve(req: Request, shared: &Shared) -> Result<Response> {
    let journal = &shared.journal;
    let capture = &shared.capture;
    match req {
        Request::Status => Ok(Response::Status {
            take: shared.take_dir.clone(),
            elapsed: journal.now(),
            source: shared.source.clone(),
            display: shared.display.clone(),
        }),
        Request::Type {
            text,
            delay_ms,
            secret,
        } => {
            let t = journal.now();
            journal.log(
                t,
                Event::Type {
                    text: (!secret).then(|| text.clone()),
                    redacted: secret,
                },
            );
            capture.type_text(&text, Duration::from_millis(delay_ms), journal)?;
            Ok(Response::Sent { t })
        }
        Request::Key { combo } => {
            let t = journal.now();
            capture.key(&combo)?;
            journal.log(t, Event::Key { key: combo });
            Ok(Response::Sent { t })
        }
        Request::Click { x, y, button } => {
            let (nx, ny) = on_screen(&shared.source, x, y)?;
            let t = journal.now();
            capture.click(x, y, button)?;
            journal.log(
                t,
                Event::Click {
                    x: nx,
                    y: ny,
                    button,
                },
            );
            Ok(Response::Sent { t })
        }
        Request::Move { x, y } => {
            on_screen(&shared.source, x, y)?;
            let t = journal.now();
            capture.move_to(x, y)?;
            Ok(Response::Sent { t })
        }
        Request::Screen => Ok(Response::Screen {
            t: journal.now(),
            screen: capture.screen_text()?,
        }),
        Request::Output => Ok(Response::Output {
            t: journal.now(),
            text: capture.output()?,
        }),
        Request::Snapshot { png } => {
            let t = journal.now();
            let png = png.unwrap_or_else(|| shared.take_dir.join(format!("screen-{t:.3}.png")));
            let (width, height) = capture.snapshot(&png)?;
            Ok(Response::Snapshot {
                t,
                png,
                width,
                height,
            })
        }
        Request::Fingerprint => Ok(Response::Fingerprint {
            t: journal.now(),
            hash: capture.fingerprint()?,
        }),
        Request::Mark { label } => {
            let t = journal.now();
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
