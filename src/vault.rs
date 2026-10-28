//! The vault: `~/brain`, plain files in a local git repo with no remote (B11).
//!
//! ```text
//! docs/<id>/meta.json   C1      pages/<slug>.md   C2      log/YYYY-MM-DD.md  C3
//! labels.jsonl          C7      profile.md        C5
//! ```
//!
//! Every write is a temp file then a rename. Commits happen in the background, at most
//! one a minute: see [`Vault::commit_later`].

use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result, io};
use crate::id::DocId;

const GITIGNORE: &str = ".*.tmp\n.DS_Store\n";
/// Inside `.git`, so it is never committed. Present while a commit is pending.
const MARKER: &str = ".git/brain-commit-pending";
const DELAY: Duration = Duration::from_secs(60);
/// Sleeps, then commits whatever is pending. Writes made during the sleep share the commit.
const COMMITTER: &str = r#"sleep "$1"; rm -f "$2"; cd "$3" && git add -A && { git diff --cached --quiet || git -c commit.gpgsign=false commit -q -m "brain: autosave"; }"#;

#[derive(Debug)]
pub struct Vault {
    root: PathBuf,
    /// Set by every write; [`Vault::commit_later`] does nothing without it.
    dirty: Cell<bool>,
}

impl Vault {
    /// `~/brain`.
    pub fn default_root() -> Result<PathBuf> {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("brain"))
            .ok_or_else(|| Error::Usage("HOME is not set; pass --vault".to_owned()))
    }

    pub fn open(root: &Path) -> Result<Self> {
        if root.join(".git").exists() {
            Ok(Self {
                root: root.to_owned(),
                dirty: Cell::new(false),
            })
        } else {
            Err(Error::NoVault(root.to_owned()))
        }
    }

    /// Opens the vault, creating it as a fresh git repo with no remote if needed.
    pub fn open_or_create(root: &Path) -> Result<Self> {
        if root.join(".git").exists() {
            return Self::open(root);
        }
        io(fs::create_dir_all(root), root)?;
        git(root, &["init", "-q"])?;
        let v = Self::open(root)?;
        v.write(Path::new(".gitignore"), GITIGNORE)?;
        git(root, &["add", "-A"])?;
        git(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "brain: create vault",
            ],
        )?;
        v.dirty.set(false);
        Ok(v)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `docs/<prefix>--<rest>`, relative to the root.
    #[must_use]
    pub fn doc_dir(id: &DocId) -> PathBuf {
        Path::new("docs").join(id.dir())
    }

    #[must_use]
    pub fn has_doc(&self, id: &DocId) -> bool {
        self.path(&Self::doc_dir(id).join("meta.json")).is_file()
    }

    #[must_use]
    pub fn path(&self, rel: &Path) -> PathBuf {
        self.root.join(rel)
    }

    pub fn read(&self, rel: &Path) -> Result<Option<String>> {
        let p = self.path(rel);
        match fs::read_to_string(&p) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => io(Err(e), &p),
        }
    }

    pub fn write(&self, rel: &Path, contents: &str) -> Result<()> {
        let p = self.path(rel);
        if let Some(dir) = p.parent() {
            io(fs::create_dir_all(dir), dir)?;
        }
        write_atomic(&p, contents)?;
        self.dirty.set(true);
        Ok(())
    }

    /// Pretty JSON with a trailing newline, so git diffs stay line-based.
    pub fn write_json<T: Serialize>(&self, rel: &Path, value: &T) -> Result<()> {
        let mut s = serde_json::to_string_pretty(value).map_err(|source| Error::Json {
            path: rel.to_owned(),
            source,
        })?;
        s.push('\n');
        self.write(rel, &s)
    }

    /// Read, extend, rename: an append is as atomic as any other write.
    pub fn append(&self, rel: &Path, text: &str) -> Result<()> {
        let mut s = self.read(rel)?.unwrap_or_default();
        s.push_str(text);
        self.write(rel, &s)
    }

    /// Commits this process's writes within a minute, without a daemon.
    ///
    /// The first write after a commit creates the marker and starts a detached `sh` that
    /// sleeps, removes the marker, and commits everything pending. Later writes see the
    /// marker and leave the pending commit to pick them up, so there is at most one commit
    /// per delay. The child is in its own process group, so it survives Ctrl-C and the
    /// end of `brain`. A marker older than twice the delay belongs to a committer that
    /// died (a reboot) and is replaced.
    pub fn commit_later(&self) -> Result<()> {
        if !self.dirty.get() {
            return Ok(());
        }
        let delay = commit_delay();
        let marker = self.root.join(MARKER);
        let claim = || {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
        };
        if let Err(e) = claim() {
            let stale = e.kind() == ErrorKind::AlreadyExists
                && fs::metadata(&marker)
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > delay * 2));
            if !stale {
                return if e.kind() == ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    io(Err(e), &marker)
                };
            }
            io(fs::remove_file(&marker), &marker)?;
            io(claim(), &marker)?;
        }
        let spawned = Command::new("sh")
            .args(["-c", COMMITTER, "sh"])
            .arg(delay.as_secs().to_string())
            .arg(&marker)
            .arg(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        match spawned {
            // Not waited on: it must outlive brain, and init reaps it.
            Ok(child) => drop(child),
            Err(e) => {
                let _ = fs::remove_file(&marker);
                return Err(Error::Command {
                    cmd: "sh".to_owned(),
                    detail: e.to_string(),
                });
            }
        }
        self.dirty.set(false);
        Ok(())
    }
}

/// `BRAIN_COMMIT_DELAY` seconds, for tests; 60 otherwise.
fn commit_delay() -> Duration {
    std::env::var("BRAIN_COMMIT_DELAY")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DELAY, Duration::from_secs)
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| Error::Command {
            cmd: format!("git {}", args.join(" ")),
            detail: e.to_string(),
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(Error::Command {
            cmd: format!("git {}", args.join(" ")),
            detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = io(fs::read_to_string(path), path)?;
    serde_json::from_str(&raw).map_err(|source| Error::Json {
        path: path.to_owned(),
        source,
    })
}

/// Write to a sibling temp file named after the target and this process, then rename:
/// a crash never leaves half a file, and two files in one directory never share a temp.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    io(fs::write(&tmp, contents), &tmp)?;
    io(fs::rename(&tmp, path), path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_differ_per_file() {
        let dir = std::env::temp_dir().join(format!("brain-atomic-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        write_atomic(&dir.join("meta.json"), "{}").unwrap();
        write_atomic(&dir.join("text.md"), "# t").unwrap();
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["meta.json", "text.md"]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
