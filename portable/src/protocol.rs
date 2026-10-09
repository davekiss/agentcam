//! CLI <-> recorder control protocol: one JSON line each way over `<take>/control.sock`.

use crate::error::{RecError, Result};
use crate::model::{Button, Source, Take};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    Status,
    Type {
        text: String,
        delay_ms: u64,
        secret: bool,
    },
    Key {
        combo: String,
    },
    /// Pixels of an x11 screen, or 0-based cells of a tty grid.
    Click {
        x: u32,
        y: u32,
        button: Button,
    },
    Move {
        x: u32,
        y: u32,
    },
    Drag {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        button: Button,
        steps: u32,
    },
    /// tty: the emulated screen as text.
    Screen,
    /// x11: write a PNG of the screen, to `<take>/screen-<t>.png` when no path is given.
    Snapshot {
        png: Option<PathBuf>,
    },
    /// A value that changes whenever the recorded screen or output does.
    Fingerprint,
    /// Plain-text output written since the last input `agentcam` sent.
    Output,
    Mark {
        label: String,
    },
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenText {
    pub text: String,
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "lowercase")]
pub enum Response {
    Status {
        take: PathBuf,
        elapsed: f64,
        source: Source,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<String>,
    },
    /// `type` and `key`: the take-clock moment the input started.
    Sent {
        t: f64,
    },
    Marked {
        take: PathBuf,
        t: f64,
        label: String,
    },
    Screen {
        t: f64,
        screen: ScreenText,
    },
    Output {
        t: f64,
        text: String,
    },
    Snapshot {
        t: f64,
        png: PathBuf,
        width: u32,
        height: u32,
    },
    Fingerprint {
        t: f64,
        hash: u64,
    },
    Stopped {
        take: Box<Take>,
    },
    Error {
        error: RecError,
    },
}

pub fn socket_path(take: &Path) -> PathBuf {
    take.join(crate::model::SOCKET_FILE)
}

/// Unix socket paths are capped near 104 bytes (macOS) / 108 (Linux), and take folders
/// under a deep `--out` exceed that. Binding or connecting by bare filename from inside
/// the folder sidesteps the cap. Callers are single-threaded at this point, so the brief
/// cwd change is not observed by anyone else.
fn via_short_path<T>(
    path: &Path,
    f: impl FnOnce(&Path) -> std::io::Result<T>,
) -> std::io::Result<T> {
    const SAFE_LEN: usize = 100;
    if path.as_os_str().len() < SAFE_LEN {
        return f(path);
    }
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return f(path);
    };
    let prev = std::env::current_dir()?;
    std::env::set_current_dir(dir)?;
    let result = f(Path::new(name));
    std::env::set_current_dir(prev)?;
    result
}

pub fn bind(path: &Path) -> Result<UnixListener> {
    let _ = std::fs::remove_file(path);
    via_short_path(path, |p| UnixListener::bind(p))
        .map_err(|e| RecError::io("bind control socket", e))
}

pub fn read_message<T: for<'de> Deserialize<'de>>(stream: &UnixStream) -> Result<T> {
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| RecError::io("read control socket", e))?;
    if line.is_empty() {
        return Err(RecError::new(
            "recorder_gone",
            "recorder closed the connection",
        ));
    }
    serde_json::from_str(&line).map_err(|e| RecError::new("protocol", format!("{e}: {line}")))
}

pub fn write_message<T: Serialize>(mut stream: &UnixStream, msg: &T) -> std::io::Result<()> {
    let mut line = serde_json::to_string(msg).expect("serializable");
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
}

/// Sends one request and returns the recorder's reply, turning `Response::Error` into `Err`.
pub fn call(take: &Path, req: &Request) -> Result<Response> {
    let stream = via_short_path(&socket_path(take), |p| UnixStream::connect(p)).map_err(|e| {
        RecError::new(
            "recorder_unreachable",
            format!("cannot reach recorder for {}: {e}", take.display()),
        )
    })?;
    write_message(&stream, req).map_err(|e| RecError::io("write control socket", e))?;
    match read_message(&stream)? {
        Response::Error { error } => Err(error),
        ok => Ok(ok),
    }
}
