//! Where takes and active state live, and how take folders get their ids.

use crate::error::{RecError, Result};
use crate::model::Active;
use std::path::{Path, PathBuf};

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| RecError::new("no_home", "HOME is not set"))
}

#[cfg(target_os = "macos")]
pub fn default_out_dir() -> Result<PathBuf> {
    Ok(home()?.join("Movies/rec"))
}

#[cfg(not(target_os = "macos"))]
pub fn default_out_dir() -> Result<PathBuf> {
    xdg("XDG_DATA_HOME", ".local/share")
}

#[cfg(target_os = "macos")]
pub fn state_dir() -> Result<PathBuf> {
    Ok(home()?.join("Library/Application Support/rec"))
}

#[cfg(not(target_os = "macos"))]
pub fn state_dir() -> Result<PathBuf> {
    xdg("XDG_STATE_HOME", ".local/state")
}

#[cfg(not(target_os = "macos"))]
fn xdg(var: &str, fallback: &str) -> Result<PathBuf> {
    // The XDG spec says relative values are invalid and must be ignored.
    match std::env::var_os(var).map(PathBuf::from) {
        Some(p) if p.is_absolute() => Ok(p.join("rec")),
        _ => Ok(home()?.join(fallback).join("rec")),
    }
}

/// Creates a fresh `take-YYYYmmdd-HHMMSS` folder under `out`, adding `-2`, `-3`, ...
/// when another take already claimed that second. `create_dir` is the lock.
pub fn create_take_dir(out: &Path, now: chrono::DateTime<chrono::Local>) -> Result<PathBuf> {
    std::fs::create_dir_all(out).map_err(|e| RecError::io(&out.display().to_string(), e))?;
    let base = format!("take-{}", now.format("%Y%m%d-%H%M%S"));
    for n in 1.. {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let dir = out.join(name);
        match std::fs::create_dir(&dir) {
            Ok(()) => return std::fs::canonicalize(&dir).map_err(|e| RecError::io("take dir", e)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(RecError::io(&dir.display().to_string(), e)),
        }
    }
    unreachable!()
}

pub fn take_id(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Accepts a take folder path, or a bare take id under the default out dir.
pub fn resolve_take(arg: &str) -> Result<PathBuf> {
    let p = PathBuf::from(arg);
    let candidate = if p.join(crate::model::TAKE_FILE).is_file() {
        p
    } else {
        default_out_dir()?.join(arg)
    };
    if !candidate.join(crate::model::TAKE_FILE).is_file() {
        return Err(RecError::new("not_found", format!("no take at {arg:?}")));
    }
    std::fs::canonicalize(&candidate).map_err(|e| RecError::io(arg, e))
}

fn active_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("active.json"))
}

pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // Signal 0 checks existence; EPERM means it exists but belongs to someone else.
    let ok = unsafe { libc::kill(pid, 0) } == 0;
    ok || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The live recorder, if any. A record whose pid is gone is removed silently.
pub fn read_active() -> Result<Option<Active>> {
    let path = active_path()?;
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    match serde_json::from_str::<Active>(&raw) {
        Ok(a) if pid_alive(a.pid) => Ok(Some(a)),
        _ => {
            let _ = std::fs::remove_file(&path);
            Ok(None)
        }
    }
}

pub fn write_active(active: &Active) -> Result<()> {
    let path = active_path()?;
    let dir = path.parent().expect("state dir");
    std::fs::create_dir_all(dir).map_err(|e| RecError::io(&dir.display().to_string(), e))?;
    crate::model::write_json_atomic(&path, active)
}

pub fn already_recording(a: &Active) -> RecError {
    RecError::new(
        "already_recording",
        format!("already recording {} (pid {})", a.take.display(), a.pid),
    )
}

/// Removes active.json only if it still names this process.
pub fn clear_active(pid: u32) {
    let Ok(path) = active_path() else { return };
    let ours = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Active>(&raw).ok())
        .is_some_and(|a| a.pid == pid);
    if ours {
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn same_second_takes_get_numbered_suffixes() {
        let out = std::env::temp_dir().join(format!("rec-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 2, 21, 35, 1).unwrap();
        let ids: Vec<String> = (0..3)
            .map(|_| take_id(&create_take_dir(&out, now).unwrap()))
            .collect();
        assert_eq!(
            ids,
            [
                "take-20261002-213501",
                "take-20261002-213501-2",
                "take-20261002-213501-3"
            ]
        );
        std::fs::remove_dir_all(&out).unwrap();
    }
}
