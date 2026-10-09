//! The x11 capture: a virtual display (Xvfb) `agentcam` owns, filmed with ffmpeg's x11grab, driven
//! with XTEST, and read back with GetImage. The recorder owns three children, Xvfb, ffmpeg and
//! the optional app, and `Children` guarantees all three are gone however the take ends.

use crate::error::{RecError, Result};
use crate::keys;
use crate::model::{self, normalize, Button, Event, Frame, PointerCapture, Track};
use crate::recorder::{Capture, FinishReason, Journal, Started};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, ImageFormat, ImageOrder, MapState, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

// Export reads screen.mp4 one frame per output frame, so capture runs at the export rate.
use crate::export::FPS;
/// libx264 preset for the live capture. Export re-encodes, so this only trades capture CPU
/// against screen.mp4 size.
const PRESET: &str = "fast";
const FIRST_DISPLAY: u32 = 99;

fn x11_err(context: &str, e: impl std::fmt::Display) -> RecError {
    RecError::new("x11", format!("{context}: {e}"))
}

fn lock_file(n: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/.X{n}-lock"))
}

fn socket_file(n: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/.X11-unix/X{n}"))
}

/// The first display number from `from` that `taken` says is free.
pub fn pick_display(from: u32, taken: impl Fn(u32) -> bool) -> u32 {
    (from..)
        .find(|&n| !taken(n))
        .expect("some display number is free")
}

fn display_taken(n: u32) -> bool {
    lock_file(n).exists() || socket_file(n).exists()
}

/// Clears what a crashed recorder left on `display` (":N"): its Xvfb, if still running, and
/// the lock and socket files that would make the next Xvfb refuse that number.
pub fn clean_stale_display(display: &str) {
    let Some(n) = display
        .strip_prefix(':')
        .and_then(|n| n.parse::<u32>().ok())
    else {
        return;
    };
    let pid = std::fs::read_to_string(lock_file(n))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok());
    if let Some(pid) = pid {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if comm.trim() == "Xvfb" {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let until = Instant::now() + Duration::from_secs(2);
            while crate::paths::pid_alive(pid as u32) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if crate::paths::pid_alive(pid as u32) {
            return;
        }
    }
    let _ = std::fs::remove_file(lock_file(n));
    let _ = std::fs::remove_file(socket_file(n));
}

/// A child in its own process group, so a terminal's Ctrl-C reaches only the recorder and the
/// recorder decides the order things stop in. On Linux it is also signalled when the recorder
/// dies; ffmpeg and apps honor that, but Xvfb does not, so a stale record cleans it up.
fn spawn(cmd: &mut Command, what: &str) -> Result<Child> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| {
        let code = if e.kind() == std::io::ErrorKind::NotFound {
            "missing_dependency"
        } else {
            "spawn_failed"
        };
        RecError::new(code, format!("{what}: {e}"))
    })
}

/// Signals `child`'s process group, waits up to `grace`, then kills it. Returns whether it
/// exited cleanly before the kill.
fn stop(child: &mut Child, sig: i32, grace: Duration) -> bool {
    if let Ok(Some(status)) = child.try_wait() {
        return status.success();
    }
    let pid = child.id() as i32;
    unsafe { libc::kill(-pid, sig) };
    let until = Instant::now() + grace;
    while Instant::now() < until {
        if let Ok(Some(status)) = child.try_wait() {
            return status.success() || sig == libc::SIGINT;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let _ = child.wait();
    false
}

/// The stderr of a child, kept as a short tail for error messages and copied to ours.
#[derive(Clone, Default)]
struct Tail(Arc<Mutex<VecDeque<String>>>);

impl Tail {
    fn push(&self, line: String) {
        let mut q = self.0.lock().unwrap();
        if q.len() == 12 {
            q.pop_front();
        }
        q.push_back(line);
    }

    fn text(&self) -> String {
        Vec::from(self.0.lock().unwrap().clone()).join("\n")
    }
}

struct Children {
    display: u32,
    xvfb: Child,
    ffmpeg: Option<Child>,
    ffmpeg_log: Tail,
    app: Option<Child>,
    done: bool,
}

impl Children {
    /// Stops ffmpeg first so screen.mp4 is finalized with the app still on screen, then the
    /// app, then Xvfb, and clears the display's lock files if Xvfb could not.
    fn teardown(&mut self) -> Result<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        let encoded = self
            .ffmpeg
            .as_mut()
            .is_none_or(|f| stop(f, libc::SIGINT, Duration::from_secs(15)));
        if let Some(app) = &mut self.app {
            stop(app, libc::SIGTERM, Duration::from_secs(2));
        }
        stop(&mut self.xvfb, libc::SIGTERM, Duration::from_secs(3));
        clean_stale_display(&format!(":{}", self.display));
        if encoded {
            Ok(())
        } else {
            Err(RecError::new(
                "capture_failed",
                format!(
                    "ffmpeg did not finish screen.mp4 cleanly: {}",
                    self.ffmpeg_log.text()
                ),
            ))
        }
    }
}

impl Drop for Children {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

/// Starts Xvfb on the first free display at or after :99, retrying the next number when
/// another server claims it first.
fn start_xvfb(frame: Frame) -> Result<(u32, Child, RustConnection, usize)> {
    let mut from = FIRST_DISPLAY;
    for _ in 0..5 {
        let n = pick_display(from, display_taken);
        from = n + 1;
        let mut child = spawn(
            Command::new("Xvfb")
                .arg(format!(":{n}"))
                .args(["-screen", "0"])
                .arg(format!("{}x{}x24", frame.width, frame.height))
                .args(["-nolisten", "tcp", "-noreset"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
            "Xvfb",
        )?;
        let until = Instant::now() + Duration::from_secs(10);
        let connected = loop {
            if let Ok(Some(_)) = child.try_wait() {
                break None;
            }
            if let Ok((conn, screen)) = RustConnection::connect(Some(&format!(":{n}"))) {
                break Some((conn, screen));
            }
            if Instant::now() >= until {
                stop(&mut child, libc::SIGTERM, Duration::from_secs(1));
                return Err(RecError::new(
                    "xvfb_failed",
                    format!("Xvfb on :{n} did not accept connections within 10s"),
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        if let Some((conn, screen)) = connected {
            return Ok((n, child, conn, screen));
        }
    }
    Err(RecError::new(
        "xvfb_failed",
        "Xvfb exited on five display numbers in a row",
    ))
}

/// The wall and monotonic clocks at t0, to place ffmpeg's first frame on the take clock.
struct Clocks {
    wall: f64,
    mono: f64,
}

impl Clocks {
    fn now() -> Clocks {
        let wall = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        Clocks {
            wall,
            mono: ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9,
        }
    }

    /// x11grab stamps frames with the wall clock in some ffmpeg versions and the monotonic
    /// clock in others; whichever is near now is the one in use.
    fn offset_of(&self, start: f64) -> f64 {
        let at = if (start - self.wall).abs() < 3600.0 {
            self.wall
        } else {
            self.mono
        };
        (start - at).max(0.0)
    }
}

/// `start: 1759612345.678901,` from ffmpeg's input dump: the first frame's timestamp.
fn parse_start(line: &str) -> Option<f64> {
    let rest = line.split_once("start: ")?.1;
    rest.split(',').next()?.trim().parse().ok()
}

struct Encoder {
    child: Child,
    log: Tail,
    first_frame: mpsc::Receiver<Instant>,
    start: Arc<Mutex<Option<f64>>>,
}

fn start_ffmpeg(display: u32, frame: Frame, out: &Path) -> Result<Encoder> {
    let mut child = spawn(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-loglevel", "info", "-nostats"])
            .args(["-f", "x11grab", "-framerate", &FPS.to_string()])
            .args(["-video_size", &format!("{}x{}", frame.width, frame.height)])
            .args(["-draw_mouse", "0", "-i", &format!(":{display}")])
            .args(["-c:v", "libx264", "-preset", PRESET, "-pix_fmt", "yuv420p"])
            .args(["-progress", "pipe:1", "-stats_period", "0.1", "-y"])
            .arg(out)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        "ffmpeg",
    )?;
    let log = Tail::default();
    let start = Arc::new(Mutex::new(None));
    {
        let stderr = child.stderr.take().expect("piped");
        let (log, start) = (log.clone(), start.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                if let Some(s) = parse_start(&line) {
                    start.lock().unwrap().get_or_insert(s);
                }
                log.push(line);
            }
        });
    }
    let (tx, first_frame) = mpsc::channel();
    {
        let stdout = child.stdout.take().expect("piped");
        std::thread::spawn(move || {
            let mut sent = false;
            for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
                let frames = line
                    .strip_prefix("frame=")
                    .and_then(|n| n.trim().parse::<u64>().ok());
                if !sent && frames.is_some_and(|n| n >= 1) {
                    sent = true;
                    let _ = tx.send(Instant::now());
                }
            }
        });
    }
    Ok(Encoder {
        child,
        log,
        first_frame,
        start,
    })
}

pub fn start(dir: &Path, command: &[String], frame: Frame) -> Result<Started> {
    let (n, xvfb, conn, screen) = start_xvfb(frame)?;
    let display = format!(":{n}");
    let mut children = Children {
        display: n,
        xvfb,
        ffmpeg: None,
        ffmpeg_log: Tail::default(),
        app: None,
        done: false,
    };
    let x = Arc::new(Screen::new(conn, screen, frame)?);

    let journal = Journal::start();
    let clocks = Clocks::now();
    // Export draws the pointer from the timeline, so its position is known from t0.
    let at_start = x.pointer()?;
    log_pointer(&x, &journal, journal.now(), at_start);
    let enc = start_ffmpeg(n, frame, &dir.join(model::SCREEN_FILE))?;
    children.ffmpeg_log = enc.log.clone();
    let ffmpeg = children.ffmpeg.insert(enc.child);
    let deadline = Instant::now() + Duration::from_secs(10);
    let first = loop {
        if let Ok(at) = enc.first_frame.recv_timeout(Duration::from_millis(25)) {
            break at;
        }
        if let Ok(Some(status)) = ffmpeg.try_wait() {
            return Err(RecError::new(
                "capture_failed",
                format!("ffmpeg exited ({status}): {}", enc.log.text()),
            ));
        }
        if Instant::now() >= deadline {
            return Err(RecError::new(
                "capture_failed",
                format!("ffmpeg wrote no frame within 10s: {}", enc.log.text()),
            ));
        }
    };
    let start = *enc.start.lock().unwrap();
    let offset = match start {
        Some(s) => clocks.offset_of(s),
        None => first.duration_since(journal.t0()).as_secs_f64(),
    };

    if let Some((program, args)) = command.split_first() {
        let err = std::io::stderr();
        let out = std::os::fd::AsFd::as_fd(&err)
            .try_clone_to_owned()
            .map_err(|e| RecError::io("dup stderr", e))?;
        children.app = Some(spawn(
            Command::new(program)
                .args(args)
                .env("DISPLAY", &display)
                .env_remove("WAYLAND_DISPLAY")
                .stdin(Stdio::null())
                .stdout(Stdio::from(out))
                .stderr(Stdio::inherit()),
            program,
        )?);
        // Input sent before the app has a window goes nowhere, so `agentcam start` waits for one.
        // There is no window manager, so the window opens at its default size in the corner;
        // the screen exists for this app, so it fills it.
        match x.wait_for_window(Duration::from_secs(10))? {
            Some(w) => x.fill(w)?,
            None => eprintln!("agentcam: {program} mapped no window within 10s; recording anyway"),
        }
    }

    let sampling = Arc::new(AtomicBool::new(true));
    {
        let (x, journal, sampling) = (x.clone(), journal.clone(), sampling.clone());
        std::thread::spawn(move || sample_pointer(&x, &journal, &sampling, at_start));
    }
    Ok(Started {
        journal,
        capture: Box::new(X11 {
            screen: x,
            children: Mutex::new(children),
            sampling,
        }),
        tracks: vec![Track::Screen {
            file: model::SCREEN_FILE.into(),
            offset: model::round_t(offset),
            width: frame.width,
            height: frame.height,
            pointer: PointerCapture::Timeline,
        }],
        display: Some(display),
    })
}

fn log_pointer(x: &Screen, journal: &Journal, t: f64, p: (i16, i16)) {
    journal.log(
        t,
        Event::Cursor {
            x: normalize(p.0 as f64, x.frame.width),
            y: normalize(p.1 as f64, x.frame.height),
        },
    );
}

/// Logs a `cursor` event, normalized to the frame, each time the pointer moves from `last`, at
/// 30 Hz.
fn sample_pointer(x: &Screen, journal: &Journal, sampling: &AtomicBool, mut last: (i16, i16)) {
    while sampling.load(Ordering::Relaxed) {
        let t = journal.now();
        if let Ok(p) = x.pointer() {
            if p != last {
                last = p;
                log_pointer(x, journal, t, p);
            }
        }
        std::thread::sleep(Duration::from_secs(1) / FPS);
    }
}

struct X11 {
    screen: Arc<Screen>,
    children: Mutex<Children>,
    sampling: Arc<AtomicBool>,
}

impl Capture for X11 {
    fn kind(&self) -> &'static str {
        "x11"
    }

    fn type_text(&self, text: &str, delay: Duration, journal: &Journal) -> Result<()> {
        self.screen.type_text(text, delay, journal)
    }

    fn key(&self, combo: &str) -> Result<()> {
        let (held, sym) = keys::x11(combo)?;
        self.screen.press(&held, sym)
    }

    fn fingerprint(&self) -> Result<u64> {
        Ok(fingerprint(&self.screen.image()?))
    }

    fn click(&self, x: u32, y: u32, button: Button) -> Result<()> {
        self.screen.click(x, y, button)
    }

    fn move_to(&self, x: u32, y: u32) -> Result<()> {
        self.screen.move_to(x, y)
    }

    fn drag(&self, path: &[(u32, u32)], button: Button, step: Duration) -> Result<()> {
        self.screen.drag(path, button, step)
    }

    fn snapshot(&self, png: &Path) -> Result<(u32, u32)> {
        let image = self.screen.image()?;
        write_png(png, &image, self.screen.frame)?;
        Ok((self.screen.frame.width, self.screen.frame.height))
    }

    fn ended(&self) -> Option<FinishReason> {
        let mut c = self.children.lock().unwrap();
        if let Ok(Some(status)) = c.xvfb.try_wait() {
            return Some(FinishReason::Failed(RecError::new(
                "capture_failed",
                format!("Xvfb exited ({status})"),
            )));
        }
        if let Some(Ok(Some(status))) = c.ffmpeg.as_mut().map(Child::try_wait) {
            return Some(FinishReason::Failed(RecError::new(
                "capture_failed",
                format!("ffmpeg exited ({status}): {}", c.ffmpeg_log.text()),
            )));
        }
        match c.app.as_mut().map(Child::try_wait) {
            Some(Ok(Some(_))) => Some(FinishReason::ChildExited),
            _ => None,
        }
    }

    fn teardown(&self) -> Result<()> {
        self.sampling.store(false, Ordering::Relaxed);
        self.children.lock().unwrap().teardown()
    }
}

/// FNV-1a over the pixels, ignoring the padding byte X leaves in each 32-bit pixel.
fn fingerprint(bgrx: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for chunk in bgrx.as_chunks::<8>().0 {
        let word = u64::from_le_bytes(*chunk) & 0x00ff_ffff_00ff_ffff;
        h = (h ^ word).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn write_png(path: &Path, bgrx: &[u8], frame: Frame) -> Result<()> {
    let rgb: Vec<u8> = bgrx
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[2], p[1], p[0]])
        .collect();
    let file =
        std::fs::File::create(path).map_err(|e| RecError::io(&path.display().to_string(), e))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), frame.width, frame.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let err = |e: png::EncodingError| RecError::new("io", format!("{}: {e}", path.display()));
    enc.write_header()
        .map_err(err)?
        .write_image_data(&rgb)
        .map_err(err)
}

/// The core keyboard mapping: `per` keysyms for each keycode from `min`.
struct Keymap {
    min: u8,
    per: usize,
    syms: Vec<u32>,
}

impl Keymap {
    /// The keycode that types `sym`, and whether it needs shift. Unshifted wins.
    fn find(&self, sym: u32) -> Option<(u8, bool)> {
        for col in 0..self.per.min(2) {
            let kc = self
                .syms
                .chunks(self.per)
                .position(|k| k[col] == sym)
                .map(|i| self.min + i as u8);
            if let Some(kc) = kc {
                return Some((kc, col == 1));
            }
        }
        None
    }

    /// Keycodes with nothing on them, which input borrows for keysyms the layout lacks.
    fn spares(&self) -> Vec<u8> {
        self.syms
            .chunks(self.per)
            .enumerate()
            .filter(|(_, k)| k.iter().all(|&s| s == 0))
            .map(|(i, _)| self.min + i as u8)
            .collect()
    }

    fn set(&mut self, kc: u8, sym: u32) {
        let i = (kc - self.min) as usize * self.per;
        for (col, s) in self.syms[i..i + self.per].iter_mut().enumerate() {
            *s = if col < 2 { sym } else { 0 };
        }
    }
}

struct Keys {
    map: Keymap,
    spares: Vec<u8>,
}

/// Splits `syms` into runs that can each be typed after one keymap change: every keysym a run
/// needs that `map` lacks fits on the `spare` keycodes. Each run comes with those keysyms.
/// Clients reload their keymap on each change and can drop a keystroke while they do, so the
/// fewer changes the better.
fn runs(
    map: &Keymap,
    syms: &[u32],
    spare: usize,
) -> Result<Vec<(std::ops::Range<usize>, Vec<u32>)>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut missing: Vec<u32> = Vec::new();
    for (i, &sym) in syms.iter().enumerate() {
        if map.find(sym).is_some() || missing.contains(&sym) {
            continue;
        }
        if spare == 0 {
            return Err(RecError::new(
                "bad_key",
                format!("keysym {sym:#x} is not on the keyboard and no keycode is free"),
            ));
        }
        if missing.len() == spare {
            out.push((start..i, std::mem::take(&mut missing)));
            start = i;
        }
        missing.push(sym);
    }
    if start < syms.len() {
        out.push((start..syms.len(), missing));
    }
    Ok(out)
}

/// One connection to the display, shared by input, the pointer sampler, and screenshots.
struct Screen {
    conn: RustConnection,
    root: Window,
    frame: Frame,
    keys: Mutex<Keys>,
}

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;

impl Screen {
    fn new(conn: RustConnection, screen: usize, frame: Frame) -> Result<Screen> {
        let setup = conn.setup();
        let root = setup.roots[screen].root;
        let bpp32 = setup
            .pixmap_formats
            .iter()
            .any(|f| f.depth == 24 && f.bits_per_pixel == 32);
        if !bpp32 || setup.image_byte_order != ImageOrder::LSB_FIRST {
            return Err(x11_err(
                "display",
                "expected 24-bit color in 32-bit LSB pixels",
            ));
        }
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        conn.xtest_get_version(2, 2)
            .map_err(|e| x11_err("XTEST", e))?
            .reply()
            .map_err(|e| x11_err("XTEST extension missing", e))?;
        let reply = conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(|e| x11_err("keyboard mapping", e))?
            .reply()
            .map_err(|e| x11_err("keyboard mapping", e))?;
        let map = Keymap {
            min,
            per: reply.keysyms_per_keycode as usize,
            syms: reply.keysyms,
        };
        let spares = map.spares();
        Ok(Screen {
            conn,
            root,
            frame,
            keys: Mutex::new(Keys { map, spares }),
        })
    }

    fn fake(&self, type_: u8, detail: u8, x: i16, y: i16) -> Result<()> {
        self.conn
            .xtest_fake_input(type_, detail, 0, self.root, x, y, 0)
            .map_err(|e| x11_err("XTEST", e))?;
        Ok(())
    }

    /// Flushes and waits for the server to have handled everything sent so far.
    fn sync(&self) -> Result<()> {
        self.conn
            .get_input_focus()
            .map_err(|e| x11_err("sync", e))?
            .reply()
            .map_err(|e| x11_err("sync", e))?;
        Ok(())
    }

    fn remap(&self, keys: &mut Keys, kc: u8, sym: u32) -> Result<()> {
        let mut row = vec![0u32; keys.map.per];
        row[0] = sym;
        if keys.map.per > 1 {
            row[1] = sym;
        }
        self.conn
            .change_keyboard_mapping(1, kc, keys.map.per as u8, &row)
            .map_err(|e| x11_err("remap key", e))?;
        keys.map.set(kc, sym);
        Ok(())
    }

    /// Puts `syms` on spare keycodes, then gives clients a moment to reload their keymap.
    fn borrow(&self, keys: &mut Keys, syms: &[u32]) -> Result<()> {
        if syms.is_empty() {
            return Ok(());
        }
        for (i, &sym) in syms.iter().enumerate() {
            self.remap(keys, keys.spares[i], sym)?;
        }
        self.sync()?;
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    /// Empties the first `n` spare keycodes again, once clients have read the key events.
    fn give_back(&self, keys: &mut Keys, n: usize) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
        for i in 0..n {
            self.remap(keys, keys.spares[i], 0)?;
        }
        self.sync()
    }

    fn stroke(&self, keys: &Keys, held: &[u32], sym: u32) -> Result<()> {
        let find = |sym: u32| {
            keys.map.find(sym).ok_or_else(|| {
                RecError::new("bad_key", format!("keysym {sym:#x} is not on the keyboard"))
            })
        };
        let mut mods = held
            .iter()
            .map(|&m| find(m).map(|(kc, _)| kc))
            .collect::<Result<Vec<_>>>()?;
        let (kc, shifted) = find(sym)?;
        if shifted && !held.contains(&keys::SHIFT_L) {
            mods.push(find(keys::SHIFT_L)?.0);
        }
        for &m in &mods {
            self.fake(KEY_PRESS, m, 0, 0)?;
        }
        self.fake(KEY_PRESS, kc, 0, 0)?;
        self.fake(KEY_RELEASE, kc, 0, 0)?;
        for &m in mods.iter().rev() {
            self.fake(KEY_RELEASE, m, 0, 0)?;
        }
        self.conn.flush().map_err(|e| x11_err("flush", e))
    }

    fn press(&self, held: &[u32], sym: u32) -> Result<()> {
        self.type_syms(&[(held.to_vec(), sym)], Duration::ZERO, None)
    }

    fn type_text(&self, text: &str, delay: Duration, journal: &Journal) -> Result<()> {
        let strokes: Vec<_> = text.chars().map(|c| (vec![], keys::keysym(c))).collect();
        self.type_syms(&strokes, delay, Some(journal))
    }

    /// Strokes each `(held modifiers, keysym)`, `delay` apart, borrowing keycodes for keysyms
    /// the layout lacks. Stops early when the take ends.
    fn type_syms(
        &self,
        strokes: &[(Vec<u32>, u32)],
        delay: Duration,
        journal: Option<&Journal>,
    ) -> Result<()> {
        let mut keys = self.keys.lock().unwrap();
        let syms: Vec<u32> = strokes.iter().map(|(_, s)| *s).collect();
        for (run, borrowed) in runs(&keys.map, &syms, keys.spares.len())? {
            self.borrow(&mut keys, &borrowed)?;
            for i in run {
                if journal.is_some_and(|j| !j.live()) {
                    break;
                }
                if i > 0 && !delay.is_zero() {
                    std::thread::sleep(delay);
                }
                self.stroke(&keys, &strokes[i].0, strokes[i].1)?;
            }
            self.sync()?;
            self.give_back(&mut keys, borrowed.len())?;
        }
        Ok(())
    }

    fn move_to(&self, x: u32, y: u32) -> Result<()> {
        self.fake(MOTION_NOTIFY, 0, x as i16, y as i16)?;
        self.sync()
    }

    fn click(&self, x: u32, y: u32, button: Button) -> Result<()> {
        self.fake(MOTION_NOTIFY, 0, x as i16, y as i16)?;
        self.fake(BUTTON_PRESS, button.x11(), 0, 0)?;
        self.fake(BUTTON_RELEASE, button.x11(), 0, 0)?;
        self.sync()
    }

    fn drag(&self, path: &[(u32, u32)], button: Button, step: Duration) -> Result<()> {
        let (&(x, y), rest) = path.split_first().expect("a drag has a start");
        self.fake(MOTION_NOTIFY, 0, x as i16, y as i16)?;
        self.fake(BUTTON_PRESS, button.x11(), 0, 0)?;
        self.sync()?;
        for &(x, y) in rest {
            std::thread::sleep(step);
            self.move_to(x, y)?;
        }
        std::thread::sleep(step);
        self.fake(BUTTON_RELEASE, button.x11(), 0, 0)?;
        self.sync()
    }

    /// Waits until an app maps a top-level window, and returns it.
    fn wait_for_window(&self, within: Duration) -> Result<Option<Window>> {
        let until = Instant::now() + within;
        let err = |e: &dyn std::fmt::Display| x11_err("query windows", e);
        while Instant::now() < until {
            let tree = self
                .conn
                .query_tree(self.root)
                .map_err(|e| err(&e))?
                .reply()
                .map_err(|e| err(&e))?;
            for w in tree.children {
                let app_window = self
                    .conn
                    .get_window_attributes(w)
                    .map_err(|e| err(&e))?
                    .reply()
                    .is_ok_and(|a| a.map_state == MapState::VIEWABLE && !a.override_redirect);
                if app_window {
                    return Ok(Some(w));
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(None)
    }

    /// Moves and sizes `window` to cover the whole screen.
    fn fill(&self, window: Window) -> Result<()> {
        let aux = ConfigureWindowAux::new()
            .x(0)
            .y(0)
            .width(self.frame.width)
            .height(self.frame.height);
        self.conn
            .configure_window(window, &aux)
            .map_err(|e| x11_err("size window", e))?;
        self.sync()
    }

    fn pointer(&self) -> Result<(i16, i16)> {
        let p = self
            .conn
            .query_pointer(self.root)
            .map_err(|e| x11_err("query pointer", e))?
            .reply()
            .map_err(|e| x11_err("query pointer", e))?;
        Ok((p.root_x, p.root_y))
    }

    /// The whole screen as BGRX pixels.
    fn image(&self) -> Result<Vec<u8>> {
        let reply = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                0,
                0,
                self.frame.width as u16,
                self.frame.height as u16,
                !0,
            )
            .map_err(|e| x11_err("get image", e))?
            .reply()
            .map_err(|e| x11_err("get image", e))?;
        Ok(reply.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_free_display_from_99() {
        assert_eq!(pick_display(99, |_| false), 99);
        assert_eq!(pick_display(99, |n| n == 99 || n == 100), 101);
        assert_eq!(pick_display(99, |n| n != 103), 103);
    }

    #[test]
    fn reads_the_first_frame_time_from_the_input_dump() {
        let line = "  Duration: N/A, start: 1759612345.678901, bitrate: 1990656 kb/s";
        assert_eq!(parse_start(line), Some(1759612345.678901));
        assert_eq!(parse_start("Stream #0:0: Video: rawvideo"), None);
    }

    #[test]
    fn offset_uses_whichever_clock_ffmpeg_stamped() {
        let c = Clocks {
            wall: 1_759_612_345.0,
            mono: 5_000.0,
        };
        assert!((c.offset_of(1_759_612_345.25) - 0.25).abs() < 1e-6);
        assert!((c.offset_of(5_000.125) - 0.125).abs() < 1e-6);
        assert_eq!(c.offset_of(4_999.0), 0.0);
    }

    fn keymap() -> Keymap {
        // keycodes 8..=11, two columns each: a/A, Return, nothing, Shift_L.
        Keymap {
            min: 8,
            per: 2,
            syms: vec![0x61, 0x41, 0xff0d, 0, 0, 0, keys::SHIFT_L, 0],
        }
    }

    #[test]
    fn keymap_finds_unshifted_then_shifted_keycodes() {
        let m = keymap();
        assert_eq!(m.find(0x61), Some((8, false)));
        assert_eq!(m.find(0x41), Some((8, true)));
        assert_eq!(m.find(0xff0d), Some((9, false)));
        assert_eq!(m.find(0x0100_2714), None);
    }

    #[test]
    fn keymap_borrows_an_empty_keycode_and_gives_it_back() {
        let mut m = keymap();
        assert_eq!(m.spares(), [10]);
        m.set(10, 0x0100_2714);
        assert_eq!(m.find(0x0100_2714), Some((10, false)));
        assert_eq!(m.spares(), Vec::<u8>::new());
        m.set(10, 0);
        assert_eq!(m.spares(), [10]);
    }

    #[test]
    fn typing_borrows_once_per_run_of_missing_keysyms() {
        let m = keymap();
        let (a, check, e, arrow) = (0x61, 0x0100_2714, 0xe9, 0x0100_2192);
        let all = |s: &[u32], spare| runs(&m, s, spare).unwrap();
        assert_eq!(all(&[a, a], 2), vec![(0..2, vec![])], "nothing to borrow");
        assert_eq!(
            all(&[e, a, check, a, e, check], 2),
            vec![(0..6, vec![e, check])],
            "two missing keysyms fit two spares: one change for the whole text"
        );
        assert_eq!(
            all(&[e, a, check, a, arrow, e], 2),
            vec![(0..4, vec![e, check]), (4..6, vec![arrow, e])],
            "a third keysym starts a new run"
        );
        assert_eq!(runs(&m, &[check], 0).unwrap_err().code, "bad_key");
    }

    #[test]
    fn fingerprint_ignores_padding_and_sees_any_pixel() {
        let a = vec![10u8, 20, 30, 0, 40, 50, 60, 0];
        let mut padded = a.clone();
        padded[3] = 255;
        assert_eq!(fingerprint(&a), fingerprint(&padded));
        let mut moved = a.clone();
        moved[5] = 51;
        assert_ne!(fingerprint(&a), fingerprint(&moved));
    }
}
