mod cast;
mod error;
mod export;
mod keys;
mod model;
mod output;
mod paths;
mod protocol;
mod recorder;
mod tty;
mod upload;
mod x11;

use clap::{ArgGroup, Args, Parser, Subcommand};
use error::{RecError, Result};
use model::{Button, Dims, Frame, Size, Source, Take};
use protocol::{Request, Response};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A screen recorder for agents. Every command prints one JSON object on stdout.
#[derive(Parser)]
#[command(name = "rec", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Record in the foreground until --duration, the command exits, or SIGINT.
    Record(RecordArgs),
    /// Start a background recorder and return once it is recording.
    Start(RecordArgs),
    /// Stop the active recorder and print the finished take.json.
    Stop,
    /// Report whether a take is recording.
    Status,
    /// Add a marker at the current moment.
    Mark { label: String },
    /// Type text into the recorded terminal or screen.
    Type {
        text: String,
        /// Milliseconds between characters.
        #[arg(long, default_value_t = 40)]
        delay: u64,
        /// Send the text but log it as redacted.
        #[arg(long)]
        secret: bool,
    },
    /// Send a key or chord, such as Return, ctrl+c, alt+x, or shift+Tab (x11).
    Key { combo: String },
    /// Click at a pixel of the x11 screen.
    Click {
        x: u32,
        y: u32,
        /// left, middle, or right.
        #[arg(long, default_value = "left")]
        button: Button,
    },
    /// Move the pointer to a pixel of the x11 screen.
    Move { x: u32, y: u32 },
    /// Print the emulated tty screen, or write a PNG of the x11 screen.
    Screen {
        /// x11: where to write the PNG. Defaults to <take>/screen-<t>.png.
        #[arg(long)]
        png: Option<PathBuf>,
    },
    /// Block until the screen matches a regex, or until it stops changing.
    #[command(group(ArgGroup::new("until").required(true).args(["text", "idle"])))]
    Wait {
        /// tty: a regex the screen must match.
        #[arg(long)]
        text: Option<String>,
        /// Seconds the screen (x11) or output (tty) must stay unchanged.
        #[arg(long, value_name = "SECONDS")]
        idle: Option<f64>,
        /// Match only output written since the last `type` or `key`, instead of the screen.
        #[arg(long, requires = "text")]
        new: bool,
        /// Seconds.
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
    },
    /// Render a take to MP4.
    Export(ExportArgs),
    /// Report the platform and what each source needs.
    Doctor,
    /// List what can be captured.
    Sources,
}

#[derive(Args)]
struct ExportArgs {
    /// Take folder path or take id.
    take: String,
    /// 16:9 or 9:16; repeat for several. Defaults to all.
    #[arg(long = "layout")]
    layouts: Vec<String>,
    #[arg(long, default_value = "dark")]
    theme: String,
    /// Font size in pixels. Defaults to the largest that fits.
    #[arg(long)]
    font_size: Option<f32>,
    /// Draw the risograph attention border.
    #[arg(long)]
    border: bool,
    /// x11: when to draw the pointer. auto shows it while it moves and around clicks.
    #[arg(long, value_enum, default_value_t)]
    cursor: export::pointer::CursorMode,
    /// Retime the take so its pacing follows the app: cut dead air, compress waits.
    #[arg(long)]
    tighten: bool,
    /// With --tighten: write the edit plan to this JSON file and render nothing.
    #[arg(
        long,
        value_name = "FILE",
        requires = "tighten",
        conflicts_with = "upload"
    )]
    plan_out: Option<PathBuf>,
    /// Retime with an edited plan from --plan-out instead of the tighten policy.
    #[arg(long, value_name = "FILE", conflicts_with = "tighten")]
    plan: Option<PathBuf>,
    /// `blob` or `blob:private` (Vercel Blob, token in BLOB_READ_WRITE_TOKEN), or a presigned https:// PUT URL.
    #[arg(long)]
    upload: Option<String>,
}

#[derive(Args, Clone)]
#[command(group(ArgGroup::new("source").required(true).args(["tty", "x11"])))]
struct RecordArgs {
    /// Record a command in a pseudo-terminal.
    #[arg(long)]
    tty: bool,
    /// Record a virtual X display (Linux), optionally launching a command into it.
    #[arg(long)]
    x11: bool,
    /// tty: the grid as COLSxROWS (default 120x36). x11: the screen in pixels (default 1920x1080).
    #[arg(long)]
    size: Option<Dims>,
    /// Pick the grid or screen that fills this export layout (16:9 or 9:16).
    #[arg(long = "for", value_name = "LAYOUT", conflicts_with = "size")]
    for_layout: Option<String>,
    /// Stop after this many seconds.
    #[arg(long)]
    duration: Option<f64>,
    /// Folder that take folders are created in.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Record into this existing folder (used by `rec start`).
    #[arg(long, hide = true)]
    take_dir: Option<PathBuf>,
    #[arg(last = true)]
    command: Vec<String>,
}

impl RecordArgs {
    fn source(&self) -> Result<Source> {
        let bad = |e: String| RecError::new("bad_args", e);
        let command = self.command.clone();
        if self.tty {
            if command.is_empty() {
                return Err(bad("--tty needs a command after --".into()));
            }
            let size = match (&self.for_layout, self.size) {
                (Some(layout), _) => export::grid_for(layout)?,
                (None, Some(d)) => Size::try_from(d).map_err(bad)?,
                (None, None) => Size { cols: 120, rows: 36 },
            };
            return Ok(Source::Tty { command, size });
        }
        let frame = match (&self.for_layout, self.size) {
            (Some(layout), _) => export::screen_for(layout)?,
            (None, Some(d)) => Frame::try_from(d).map_err(bad)?,
            (None, None) => Frame::try_from(Dims { w: 1920, h: 1080 }).expect("valid"),
        };
        Ok(Source::X11 { command, frame })
    }
}

/// The `rec record` flags that reproduce `source`, for `rec start` to hand to its recorder.
fn record_flags(source: &Source) -> Vec<String> {
    let (kind, size, command) = match source {
        Source::Tty { command, size } => ("--tty", format!("{}x{}", size.cols, size.rows), command),
        Source::X11 { command, frame } => {
            ("--x11", format!("{}x{}", frame.width, frame.height), command)
        }
    };
    let mut flags = vec![kind.to_string(), "--size".into(), size];
    if !command.is_empty() {
        flags.push("--".into());
        flags.extend(command.iter().cloned());
    }
    flags
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) if !e.use_stderr() => e.exit(),
        Err(e) => fail(RecError::new("bad_args", e.to_string().trim())),
    };
    match run(cli.cmd) {
        Ok(v) => println!("{v}"),
        Err(e) => fail(e),
    }
}

fn fail(e: RecError) -> ! {
    println!("{}", json!({ "error": e }));
    std::process::exit(1)
}

fn run(cmd: Cmd) -> Result<Value> {
    match cmd {
        Cmd::Record(args) => record(args),
        Cmd::Start(args) => start(args),
        Cmd::Stop => stop(),
        Cmd::Status => status(),
        Cmd::Mark { label } => match call(&Request::Mark { label })? {
            Response::Marked { take, t, label } => Ok(json!({ "take": take, "t": t, "label": label })),
            other => unexpected(other),
        },
        Cmd::Type {
            text,
            delay,
            secret,
        } => sent(call(&Request::Type {
            text,
            delay_ms: delay,
            secret,
        })?),
        Cmd::Key { combo } => {
            keys::x11(&combo)?;
            sent(call(&Request::Key { combo })?)
        }
        Cmd::Click { x, y, button } => sent(call(&Request::Click { x, y, button })?),
        Cmd::Move { x, y } => sent(call(&Request::Move { x, y })?),
        Cmd::Screen { png } => screen(png),
        Cmd::Wait {
            text,
            idle,
            new,
            timeout,
        } => match (text, idle) {
            (Some(text), _) => wait(&text, new, timeout),
            (None, Some(idle)) => wait_idle(idle, timeout),
            (None, None) => unreachable!("clap requires one"),
        },
        Cmd::Export(args) => export(args),
        Cmd::Doctor => Ok(doctor()),
        Cmd::Sources => Ok(json!({ "tty": true, "x11": x11_missing().is_empty() })),
    }
}

fn unexpected(r: Response) -> Result<Value> {
    Err(RecError::new("protocol", format!("unexpected reply {r:?}")))
}

fn sent(r: Response) -> Result<Value> {
    match r {
        Response::Sent { t } => Ok(json!({ "t": t })),
        other => unexpected(other),
    }
}

fn active() -> Result<model::Active> {
    paths::read_active()?.ok_or_else(|| RecError::new("not_recording", "nothing is recording"))
}

fn call(req: &Request) -> Result<Response> {
    protocol::call(&active()?.take, req)
}

fn take_json(take: &Take) -> Value {
    serde_json::to_value(take).expect("serializable")
}

fn record(args: RecordArgs) -> Result<Value> {
    let source = args.source()?;
    let take_dir = match args.take_dir {
        Some(dir) => dir,
        None => {
            if let Some(a) = paths::read_active()? {
                return Err(paths::already_recording(&a));
            }
            let out = args.out.map_or_else(paths::default_out_dir, Ok)?;
            paths::create_take_dir(&out, chrono::Local::now())?
        }
    };
    let take = recorder::record(recorder::RecordOptions {
        take_dir: take_dir.clone(),
        duration: args.duration,
        source,
    })?;
    match take.error {
        Some(e) => Err(RecError::new(
            &e.code,
            format!("{} ({})", e.message, take_dir.display()),
        )),
        None => Ok(take_json(&take)),
    }
}

fn start(args: RecordArgs) -> Result<Value> {
    use std::os::unix::process::CommandExt;

    if let Some(a) = paths::read_active()? {
        return Err(paths::already_recording(&a));
    }
    let source = args.source()?;
    let out = args.out.clone().map_or_else(paths::default_out_dir, Ok)?;
    let dir = paths::create_take_dir(&out, chrono::Local::now())?;
    let log_path = dir.join(model::LOG_FILE);
    let log =
        std::fs::File::create(&log_path).map_err(|e| RecError::io("create recorder.log", e))?;
    let log_err = log
        .try_clone()
        .map_err(|e| RecError::io("recorder.log", e))?;

    let exe = std::env::current_exe().map_err(|e| RecError::io("locate rec binary", e))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("record").arg("--take-dir").arg(&dir);
    if let Some(d) = args.duration {
        cmd.arg("--duration").arg(d.to_string());
    }
    cmd.args(record_flags(&source));
    cmd.stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // A new session detaches the recorder from our terminal, so it outlives this process.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| RecError::io("spawn recorder", e))?;

    // Xvfb plus ffmpeg's first frame takes a second or two; a loaded box can take longer.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(recorder_log_error(&log_path).unwrap_or_else(|| {
                RecError::new(
                    "recorder_failed",
                    format!("recorder exited ({status}); see {}", log_path.display()),
                )
            }));
        }
        if let Ok(Response::Status { display, .. }) = protocol::call(&dir, &Request::Status) {
            let mut started = json!({ "take": dir, "pid": child.id() });
            if let Some(display) = display {
                started["display"] = json!(display);
            }
            return Ok(started);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(RecError::new(
                "start_timeout",
                format!(
                    "recorder did not answer within 20s; see {}",
                    log_path.display()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The recorder prints its failure as the usual JSON error line into recorder.log.
fn recorder_log_error(log: &std::path::Path) -> Option<RecError> {
    let raw = std::fs::read_to_string(log).ok()?;
    raw.lines().rev().find_map(|l| {
        serde_json::from_str::<Value>(l)
            .ok()
            .and_then(|v| serde_json::from_value(v.get("error")?.clone()).ok())
    })
}

fn stop() -> Result<Value> {
    let Some(a) = paths::read_active()? else {
        return Ok(json!({ "recording": false }));
    };
    match protocol::call(&a.take, &Request::Stop) {
        Ok(Response::Stopped { take }) => Ok(take_json(&take)),
        Ok(other) => unexpected(other),
        Err(_) => {
            // The recorder was already finishing on its own; let it finish and read the result.
            let deadline = Instant::now() + Duration::from_secs(15);
            while paths::pid_alive(a.pid) {
                if Instant::now() >= deadline {
                    return Err(RecError::new(
                        "stop_timeout",
                        format!("recorder pid {} did not finish", a.pid),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(take_json(&Take::read(&a.take)?))
        }
    }
}

fn status() -> Result<Value> {
    let Some(a) = paths::read_active()? else {
        return Ok(json!({ "recording": false }));
    };
    match protocol::call(&a.take, &Request::Status)? {
        Response::Status {
            take,
            elapsed,
            source,
            display,
        } => {
            let mut status =
                json!({ "recording": true, "take": take, "elapsed": elapsed, "source": source });
            if let Some(display) = display {
                status["display"] = json!(display);
            }
            Ok(status)
        }
        other => unexpected(other),
    }
}

fn wait(pattern: &str, new: bool, timeout: f64) -> Result<Value> {
    let re = regex::Regex::new(pattern).map_err(|e| RecError::new("bad_regex", e.to_string()))?;
    let take = active()?.take;
    let (req, what) = if new {
        (Request::Output, "new output")
    } else {
        (Request::Screen, "screen")
    };
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    loop {
        let (t, text) = match protocol::call(&take, &req)? {
            Response::Screen { t, screen } => (t, screen.text),
            Response::Output { t, text } => (t, text),
            other => return unexpected(other),
        };
        if re.is_match(&text) {
            return Ok(json!({ "matched": true, "t": t }));
        }
        if Instant::now() >= deadline {
            return Err(RecError::new(
                "timeout",
                format!("{what} did not match {pattern:?} within {timeout}s"),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// tty: the screen as text. x11: a PNG of the screen.
fn screen(png: Option<PathBuf>) -> Result<Value> {
    let a = active()?;
    let req = match (&a.source, png) {
        (Source::Tty { .. }, None) => Request::Screen,
        (Source::Tty { .. }, Some(_)) => {
            return Err(recorder::not_supported("screen --png", "tty"))
        }
        (Source::X11 { .. }, png) => Request::Snapshot {
            png: png
                .map(|p| std::path::absolute(&p).map_err(|e| RecError::io("png path", e)))
                .transpose()?,
        },
    };
    match protocol::call(&a.take, &req)? {
        Response::Screen { screen, .. } => Ok(json!(screen)),
        Response::Snapshot {
            t,
            png,
            width,
            height,
        } => Ok(json!({ "png": png, "width": width, "height": height, "t": t })),
        other => unexpected(other),
    }
}

/// Returns once the fingerprint has held still for `idle` seconds since the wait began. Going
/// back to the image before the current one does not count as a change, so a blinking cursor
/// does not keep an idle screen busy.
fn wait_idle(idle: f64, timeout: f64) -> Result<Value> {
    let take = active()?.take;
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64(timeout.max(0.0));
    let idle = Duration::from_secs_f64(idle.max(0.0));
    let mut recent: [Option<u64>; 2] = [None, None];
    let mut quiet_since = started;
    loop {
        let (t, hash) = match protocol::call(&take, &Request::Fingerprint)? {
            Response::Fingerprint { t, hash } => (t, hash),
            other => return unexpected(other),
        };
        let now = Instant::now();
        if !recent.contains(&Some(hash)) {
            if recent[0].is_some() {
                quiet_since = now;
            }
            recent = [Some(hash), recent[0]];
        }
        if now.duration_since(quiet_since) >= idle {
            return Ok(json!({ "idle": true, "t": t }));
        }
        if now >= deadline {
            return Err(RecError::new(
                "timeout",
                format!("the screen kept changing for {timeout}s"),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn export(args: ExportArgs) -> Result<Value> {
    let ExportArgs {
        take,
        layouts,
        theme,
        font_size: font_px,
        border,
        cursor,
        tighten,
        plan_out,
        plan,
        upload,
    } = args;
    let upload = upload
        .map(|t| upload::Target::parse(&t, |k| std::env::var(k).ok()))
        .transpose()?;
    let dir = paths::resolve_take(&take)?;
    if let Some(path) = plan_out {
        let plan = export::plan(&dir)?;
        let json = serde_json::to_string_pretty(&plan).expect("serializable");
        std::fs::write(&path, json + "\n")
            .map_err(|e| RecError::io(&path.display().to_string(), e))?;
        return Ok(json!({ "take": dir, "plan": path, "segments": plan.segments.len() }));
    }
    let pacing = match (plan, tighten) {
        (Some(path), _) => export::Pacing::Plan(read_plan(&path)?),
        (None, true) => export::Pacing::Tighten,
        (None, false) => export::Pacing::Raw,
    };
    let theme = theme.as_str();
    let layouts = if layouts.is_empty() {
        export::layout::PRESETS.to_vec()
    } else {
        layouts
            .iter()
            .map(|l| export::layout::preset(l))
            .collect::<Result<_>>()?
    };
    let theme = export::theme::by_name(theme).ok_or_else(|| {
        let known: Vec<_> = export::theme::THEMES.iter().map(|t| t.name).collect();
        RecError::new(
            "bad_theme",
            format!("unknown theme {theme:?}; known: {}", known.join(", ")),
        )
    })?;
    if let Some(target) = &upload {
        target.check_layouts(layouts.len())?;
    }
    let exports = export::export(
        &dir,
        &export::ExportOptions {
            layouts,
            theme,
            font_px,
            border,
            cursor,
            pacing,
        },
    )?;
    let take_id = paths::take_id(&dir);
    let exports = exports
        .into_iter()
        .map(|e| {
            let url = match &upload {
                Some(target) => Some(target.upload(&take_id, &e.path)?),
                None => None,
            };
            Ok(upload::Delivered { export: e, url })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({ "take": dir, "exports": exports }))
}

fn read_plan(path: &std::path::Path) -> Result<export::tighten::Plan> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| RecError::io(&path.display().to_string(), e))?;
    serde_json::from_str(&raw).map_err(|e| {
        RecError::new(
            "bad_args",
            format!("{} is not a tighten plan: {e}", path.display()),
        )
    })
}

/// What the x11 source needs that is not on PATH.
fn x11_missing() -> Vec<&'static str> {
    ["Xvfb", "ffmpeg"]
        .into_iter()
        .filter(|tool| export::find_on_path(tool).is_none())
        .collect()
}

fn doctor() -> Value {
    let pty = portable_pty::native_pty_system().openpty(portable_pty::PtySize::default());
    let ffmpeg = export::find_on_path("ffmpeg");
    let x11_missing = x11_missing();
    let mut missing = Vec::new();
    if pty.is_err() {
        missing.push("pty");
    }
    if ffmpeg.is_none() {
        missing.push("ffmpeg");
    }
    if x11_missing.contains(&"Xvfb") {
        missing.push("Xvfb");
    }
    json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "sources": {
            "tty": {
                "ok": pty.is_ok(),
                "needs": ["pty"],
                "error": pty.as_ref().err().map(|e| e.to_string()),
            },
            "x11": {
                "ok": x11_missing.is_empty(),
                "needs": ["Xvfb", "ffmpeg"],
                "missing": x11_missing,
            },
        },
        "ffmpeg": { "found": ffmpeg.is_some(), "path": ffmpeg, "neededBy": ["export"] },
        "missing": missing,
    })
}
