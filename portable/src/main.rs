mod cast;
mod error;
mod export;
mod keys;
mod model;
mod output;
mod paths;
mod protocol;
mod recorder;
mod upload;

use clap::{Args, Parser, Subcommand};
use error::{RecError, Result};
use model::{Size, Take};
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
    /// Type text into the recorded terminal.
    Type {
        text: String,
        /// Milliseconds between characters.
        #[arg(long, default_value_t = 40)]
        delay: u64,
        /// Send the text but log it as redacted.
        #[arg(long)]
        secret: bool,
    },
    /// Send a key or chord, such as Return, ctrl+c, or alt+x.
    Key { combo: String },
    /// Print the emulated screen.
    Screen,
    /// Block until the screen matches a regex.
    Wait {
        #[arg(long)]
        text: String,
        /// Match only output written since the last `type` or `key`, instead of the screen.
        #[arg(long)]
        new: bool,
        /// Seconds.
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
    },
    /// Render a take to MP4.
    Export {
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
        /// Retime the take so its pacing follows the app: cut dead air, compress waits.
        #[arg(long)]
        tighten: bool,
        /// `blob` or `blob:private` (Vercel Blob, token in BLOB_READ_WRITE_TOKEN), or a presigned https:// PUT URL.
        #[arg(long)]
        upload: Option<String>,
    },
    /// Report the platform and what each source needs.
    Doctor,
    /// List what can be captured.
    Sources,
}

#[derive(Args, Clone)]
struct RecordArgs {
    /// Record a command in a pseudo-terminal.
    #[arg(long, required = true)]
    tty: bool,
    /// Terminal grid as COLSxROWS. Defaults to 120x36.
    #[arg(long)]
    size: Option<Size>,
    /// Pick the grid that fills this export layout (16:9 or 9:16) at a legible size.
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
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

impl RecordArgs {
    fn grid(&self) -> Result<Size> {
        match (&self.for_layout, self.size) {
            (Some(layout), _) => export::grid_for(layout),
            (None, Some(size)) => Ok(size),
            (None, None) => Ok(Size { cols: 120, rows: 36 }),
        }
    }
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
            keys::encode(&combo, keys::CursorMode::Normal)?;
            sent(call(&Request::Key { combo })?)
        }
        Cmd::Screen => match call(&Request::Screen)? {
            Response::Screen { screen, .. } => Ok(json!(screen)),
            other => unexpected(other),
        },
        Cmd::Wait { text, new, timeout } => wait(&text, new, timeout),
        Cmd::Export {
            take,
            layouts,
            theme,
            font_size,
            border,
            tighten,
            upload,
        } => export(
            &take,
            layouts,
            &theme,
            font_size,
            border,
            tighten,
            upload.as_deref(),
        ),
        Cmd::Doctor => Ok(doctor()),
        Cmd::Sources => Ok(json!({ "tty": true, "x11": false })),
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
    let size = args.grid()?;
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
        size,
        duration: args.duration,
        command: args.command,
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
    let size = args.grid()?;
    let out = args.out.clone().map_or_else(paths::default_out_dir, Ok)?;
    let dir = paths::create_take_dir(&out, chrono::Local::now())?;
    let log_path = dir.join(model::LOG_FILE);
    let log = std::fs::File::create(&log_path).map_err(|e| RecError::io("create recorder.log", e))?;
    let log_err = log.try_clone().map_err(|e| RecError::io("recorder.log", e))?;

    let exe = std::env::current_exe().map_err(|e| RecError::io("locate rec binary", e))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["record", "--tty", "--size"])
        .arg(format!("{}x{}", size.cols, size.rows))
        .arg("--take-dir")
        .arg(&dir);
    if let Some(d) = args.duration {
        cmd.arg("--duration").arg(d.to_string());
    }
    cmd.arg("--").args(&args.command);
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
    let mut child = cmd
        .spawn()
        .map_err(|e| RecError::io("spawn recorder", e))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(recorder_log_error(&log_path).unwrap_or_else(|| {
                RecError::new(
                    "recorder_failed",
                    format!("recorder exited ({status}); see {}", log_path.display()),
                )
            }));
        }
        if protocol::call(&dir, &Request::Status).is_ok() {
            return Ok(json!({ "take": dir, "pid": child.id() }));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(RecError::new(
                "start_timeout",
                format!("recorder did not answer within 10s; see {}", log_path.display()),
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
        } => Ok(json!({ "recording": true, "take": take, "elapsed": elapsed, "source": source })),
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

fn export(
    take: &str,
    layouts: Vec<String>,
    theme: &str,
    font_px: Option<f32>,
    border: bool,
    tighten: bool,
    upload: Option<&str>,
) -> Result<Value> {
    let upload = upload
        .map(|t| upload::Target::parse(t, |k| std::env::var(k).ok()))
        .transpose()?;
    let dir = paths::resolve_take(take)?;
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
            tighten,
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

fn doctor() -> Value {
    let pty = portable_pty::native_pty_system().openpty(portable_pty::PtySize::default());
    let ffmpeg = export::find_on_path("ffmpeg");
    let mut missing = Vec::new();
    if pty.is_err() {
        missing.push("pty");
    }
    if ffmpeg.is_none() {
        missing.push("ffmpeg");
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
            "x11": { "ok": false, "error": "not implemented yet" },
        },
        "ffmpeg": { "found": ffmpeg.is_some(), "path": ffmpeg, "neededBy": ["export"] },
        "missing": missing,
    })
}
