//! `$XDG_STATE_HOME/brain`: what the machine keeps about its own runs, never in the vault.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

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
    let path = dir.join(file);
    let mut f = io(
        OpenOptions::new().create(true).append(true).open(&path),
        &path,
    )?;
    io(writeln!(f, "{line}"), &path)
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
