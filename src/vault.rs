//! The vault: `~/brain`, plain files in a local git repo with no remote (B11).
//!
//! ```text
//! docs/<dir>/<dir>.md   C1      pages/<slug>.md   C2      log/YYYY-MM-DD.md  C3
//! labels.jsonl          C7      profile.md        C5
//! ```
//!
//! Every write is a temp file then a rename; read-modify-writes hold the vault lock (a
//! `flock` on the root directory). Commits happen in the background, at most one per
//! delay: see [`Vault::commit_later`].

use std::cell::Cell;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{BufRead as _, BufReader, ErrorKind, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::error::{Error, Result, io};
use crate::id::DocId;

const GITIGNORE: &str = ".*.tmp\n.DS_Store\n";
/// Inside `.git`, so it is never committed. Locked by the pending committer for as long as
/// it waits; the kernel drops the lock when it exits or is killed, so it never goes stale.
const MARKER: &str = ".git/brain-commit";
const DELAY: Duration = Duration::from_secs(60);
/// Set to the vault root, this env var makes `brain` run as the committer.
pub const COMMITTER_ENV: &str = "BRAIN_COMMIT";

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
    /// Concurrent first uses wait on the lock, and exactly one creates it.
    pub fn open_or_create(root: &Path) -> Result<Self> {
        if root.join(".git").exists() {
            return Self::open(root);
        }
        io(fs::create_dir_all(root), root)?;
        let _lock = lock(root)?;
        if !root.join(".git").exists() {
            git(root, &["init", "-q"])?;
            write_atomic(&root.join(".gitignore"), GITIGNORE)?;
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
        }
        Self::open(root)
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
        self.path(&crate::doc::path(id)).is_file()
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

    pub fn read_bytes(&self, rel: &Path) -> Result<Option<Vec<u8>>> {
        let p = self.path(rel);
        match fs::read(&p) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => io(Err(e), &p),
        }
    }

    pub fn write(&self, rel: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
        let p = self.path(rel);
        if let Some(dir) = p.parent() {
            io(fs::create_dir_all(dir), dir)?;
        }
        // Set first: a rename that fails after the temp landed still leaves work to commit.
        self.dirty.set(true);
        write_atomic(&p, contents)
    }

    /// Copies a file in, byte for byte, by the same temp-then-rename as every write.
    pub fn copy(&self, from: &Path, rel: &Path) -> Result<()> {
        let p = self.path(rel);
        if let Some(dir) = p.parent() {
            io(fs::create_dir_all(dir), dir)?;
        }
        self.dirty.set(true);
        let tmp = temp_for(&p);
        let copied = io(fs::copy(from, &tmp), from).and_then(|_| io(fs::rename(&tmp, &p), &p));
        if copied.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        copied
    }

    /// Read, change, rename, under the vault lock, so concurrent updates never lose one.
    /// `f` returns the new contents, or `None` to leave the file alone. `true` if written.
    pub fn update(
        &self,
        rel: &Path,
        f: impl FnOnce(Option<String>) -> Result<Option<String>>,
    ) -> Result<bool> {
        let _lock = lock(&self.root)?;
        match f(self.read(rel)?)? {
            Some(new) => self.write(rel, &new).map(|()| true),
            None => Ok(false),
        }
    }

    /// Deletes a file; one already gone is fine.
    pub fn remove(&self, rel: &Path) -> Result<()> {
        let p = self.path(rel);
        self.dirty.set(true);
        match fs::remove_file(&p) {
            Err(e) if e.kind() != ErrorKind::NotFound => io(Err(e), &p),
            _ => Ok(()),
        }
    }

    pub fn append(&self, rel: &Path, text: &str) -> Result<()> {
        self.update(rel, |old| Ok(Some(old.unwrap_or_default() + text)))
            .map(drop)
    }

    /// Runs `f` under the vault lock. `f` must not take it again: that would deadlock.
    pub(crate) fn locked<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _lock = lock(&self.root)?;
        f()
    }

    /// Runs `f` under the vault lock once every pending write is committed, so a git
    /// operation on the vault (a wiki proposal or merge) never races the autosave: the
    /// pending committer later finds nothing to commit.
    pub(crate) fn settled<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _lock = lock(&self.root)?;
        commit(&self.root)?;
        f()
    }

    /// Commits this process's writes within the delay, without a daemon.
    ///
    /// If no committer is pending (the marker is not locked), this starts `brain` again as
    /// a detached committer and waits for its handshake: the committer locks the marker,
    /// says `ok`, sleeps for the delay, takes the vault lock, unlocks the marker, then
    /// commits everything pending. A write that finds the marker locked is picked up by that
    /// commit, because the unlock comes before `git add`. So commits are at least a delay apart.
    ///
    /// Inside a systemd unit (`INVOCATION_ID` is set) a detached child would die with the
    /// unit, so this commits now instead, under the vault lock. The tick runs every
    /// 15 minutes, so that still makes at most one commit a minute.
    pub fn commit_later(&self) -> Result<()> {
        if !self.dirty.get() {
            return Ok(());
        }
        if std::env::var_os("INVOCATION_ID").is_some() {
            let _lock = lock(&self.root)?;
            commit(&self.root)?;
            self.dirty.set(false);
            return Ok(());
        }
        let marker = self.root.join(MARKER);
        let file = io(
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&marker),
            &marker,
        )?;
        match file.try_lock() {
            Ok(()) => drop(file),
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Error(e)) => return io(Err(e), &marker),
        }
        let exe = io(std::env::current_exe(), Path::new("brain"))?;
        handshake(Command::new(exe).env(COMMITTER_ENV, &self.root)).map_err(|detail| {
            Error::Command {
                cmd: "brain committer".to_owned(),
                detail,
            }
        })?;
        self.dirty.set(false);
        Ok(())
    }
}

/// Starts the committer detached, and returns once it holds the marker (or found it held).
fn handshake(cmd: &mut Command) -> Result<(), String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    if let Some(out) = child.stdout.take() {
        let _ = BufReader::new(out).read_line(&mut line);
    }
    // Not waited on: it must outlive brain, and init reaps it.
    drop(child);
    match line.trim() {
        "ok" | "busy" => Ok(()),
        _ => Err("no handshake".to_owned()),
    }
}

/// The committer: `brain` with `BRAIN_COMMIT=<vault>`. See [`Vault::commit_later`].
pub fn run_committer(root: &Path) -> Result<()> {
    let marker = root.join(MARKER);
    let file = io(
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&marker),
        &marker,
    )?;
    let mut out = std::io::stdout();
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return io(writeln!(out, "busy").and_then(|()| out.flush()), &marker);
        }
        Err(TryLockError::Error(e)) => return io(Err(e), &marker),
    }
    io(writeln!(out, "ok").and_then(|()| out.flush()), &marker)?;
    std::thread::sleep(commit_delay());
    // Vault lock before the marker unlock: there is no moment when neither is held, and a
    // write that found the marker held landed before the `git add` below.
    let _lock = lock(root)?;
    io(file.unlock(), &marker)?;
    commit(root)
}

/// Commits everything pending, if anything is. The caller holds the vault lock.
/// Refuses during a git merge someone left open: it would commit conflict markers.
fn commit(root: &Path) -> Result<()> {
    if root.join(".git/MERGE_HEAD").exists() {
        return Err(Error::Usage(format!(
            "a git merge is in progress in {}; finish it or run `git merge --abort` there",
            root.display()
        )));
    }
    git(root, &["add", "-A"])?;
    if git(root, &["diff", "--cached", "--name-only"])?
        .trim()
        .is_empty()
    {
        return Ok(());
    }
    git(
        root,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "brain: autosave",
        ],
    )
    .map(drop)
}

/// The vault lock: an exclusive `flock` on the root directory, released on drop.
fn lock(root: &Path) -> Result<File> {
    let dir = io(File::open(root), root)?;
    io(dir.lock(), root)?;
    Ok(dir)
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
pub(crate) fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let tmp = temp_for(path);
    io(fs::write(&tmp, contents), &tmp)?;
    io(fs::rename(&tmp, path), path)
}

fn temp_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_differ_per_file() {
        let dir = std::env::temp_dir().join(format!("brain-atomic-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        write_atomic(&dir.join("a.md"), "---\n---\n").unwrap();
        write_atomic(&dir.join("index.html"), "<p>").unwrap();
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["a.md", "index.html"]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
