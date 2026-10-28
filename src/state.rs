//! `$XDG_STATE_HOME/brain`: what the machine keeps about its own runs, never in the vault.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Result, io};
use crate::index::xdg;

pub fn dir() -> Result<PathBuf> {
    Ok(xdg("XDG_STATE_HOME", ".local/state")?.join("brain"))
}

/// Appends one compact JSON line to `file` in the state dir.
pub fn append(file: &str, line: &Value) -> Result<()> {
    let dir = dir()?;
    io(fs::create_dir_all(&dir), &dir)?;
    append_to(&dir.join(file), line)
}

/// The line and its newline go out in one `write(2)` on an `O_APPEND` file, so concurrent
/// appenders never interleave inside a line.
fn append_to(path: &Path, line: &Value) -> Result<()> {
    let mut f = io(
        OpenOptions::new().create(true).append(true).open(path),
        path,
    )?;
    io(f.write_all(format!("{line}\n").as_bytes()), path)
}

/// An exclusive lock on `name` in the state dir, held until the file drops; `None` when
/// another process holds it.
pub fn lock(name: &str) -> Result<Option<File>> {
    let dir = dir()?;
    io(fs::create_dir_all(&dir), &dir)?;
    let path = dir.join(name);
    let f = io(
        OpenOptions::new().create(true).append(true).open(&path),
        &path,
    )?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => io(Err(e), &path),
    }
}

/// The last line of `file` that parses, `None` when there is no file or no line.
pub fn last(file: &str) -> Result<Option<Value>> {
    let path = dir()?.join(file);
    match fs::read_to_string(&path) {
        Ok(s) => Ok(s.lines().rev().find_map(|l| serde_json::from_str(l).ok())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => io(Err(e), &path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_appends_never_tear_a_line() {
        let dir = std::env::temp_dir().join(format!("brain-state-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("jobs.jsonl");
        let pad = "x".repeat(600);
        std::thread::scope(|s| {
            for t in 0..8 {
                let (path, pad) = (&path, &pad);
                s.spawn(move || {
                    for i in 0..200 {
                        append_to(path, &serde_json::json!({"t": t, "i": i, "pad": pad})).unwrap();
                    }
                });
            }
        });
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1600);
        assert!(
            text.lines()
                .all(|l| serde_json::from_str::<Value>(l).is_ok())
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
