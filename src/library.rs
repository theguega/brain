//! The library folder: plain files, synced with git.
//!
//! ```text
//! <root>/
//!   papers/<id>/paper.json   the record (meta, tags, note, card)
//!   papers/<id>/paper.md     full text
//!   inbox.jsonl              discovered candidates, one per line
//!   .cache/                  index.db, PDFs (gitignored, rebuilt on demand)
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result, io};
use crate::id::PaperId;
use crate::paper::Paper;

const GITIGNORE: &str = ".cache/\n.DS_Store\n";

#[derive(Debug)]
pub struct Library {
    root: PathBuf,
}

impl Library {
    /// `$PAPERDB_LIBRARY`, else `~/papers`.
    pub fn default_root() -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("PAPERDB_LIBRARY") {
            return Ok(PathBuf::from(p));
        }
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("papers"))
            .ok_or_else(|| Error::Usage("HOME is not set; set PAPERDB_LIBRARY".to_owned()))
    }

    pub fn open(root: &Path) -> Result<Self> {
        if root.join("papers").is_dir() {
            Ok(Self {
                root: root.to_owned(),
            })
        } else {
            Err(Error::NoLibrary(root.to_owned()))
        }
    }

    /// Creates an empty library (a fresh git repo) at `root`.
    pub fn create(root: &Path) -> Result<Self> {
        if root.join("papers").exists() {
            return Err(Error::Usage(format!(
                "{} is already a library",
                root.display()
            )));
        }
        let lib = Self {
            root: root.to_owned(),
        };
        let papers = root.join("papers");
        io(fs::create_dir_all(&papers), &papers)?;
        write_atomic(&root.join("papers/.keep"), "")?;
        write_atomic(&root.join(".gitignore"), GITIGNORE)?;
        git(root, &["init", "-q"])?;
        lib.commit("init library")?;
        Ok(lib)
    }

    /// Clones an existing library from `remote` into `root`.
    pub fn clone_from(remote: &str, root: &Path) -> Result<Self> {
        let parent = root.parent().unwrap_or(Path::new("."));
        io(fs::create_dir_all(parent), parent)?;
        let dest = root.to_string_lossy();
        git(parent, &["clone", "-q", remote, &dest])?;
        Self::open(root)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn cache(&self) -> PathBuf {
        self.root.join(".cache")
    }

    fn dir(&self, id: &PaperId) -> PathBuf {
        self.root.join("papers").join(id.as_str())
    }

    #[must_use]
    pub fn contains(&self, id: &PaperId) -> bool {
        self.dir(id).join("paper.json").is_file()
    }

    /// An id, an arXiv URL, or a paper's short name (case-insensitive).
    pub fn find(&self, reference: &str) -> Result<PaperId> {
        if let Ok(id) = PaperId::parse(reference)
            && self.contains(&id)
        {
            return Ok(id);
        }
        let mut hits = Vec::new();
        for p in self.papers()? {
            if p.name.eq_ignore_ascii_case(reference) {
                hits.push(p.id());
            }
        }
        match hits.as_slice() {
            [id] => Ok(id.clone()),
            [] => Err(Error::NotFound(reference.to_owned())),
            many => Err(Error::Usage(format!(
                "{reference:?} names several papers: {}",
                many.iter()
                    .map(PaperId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    pub fn load(&self, id: &PaperId) -> Result<Paper> {
        let p = self.dir(id).join("paper.json");
        if !p.is_file() {
            return Err(Error::NotFound(id.to_string()));
        }
        read_json(&p)
    }

    pub fn save(&self, paper: &Paper) -> Result<()> {
        let d = self.dir(&paper.id());
        io(fs::create_dir_all(&d), &d)?;
        write_json(&d.join("paper.json"), paper)
    }

    pub fn remove(&self, id: &PaperId) -> Result<()> {
        if !self.contains(id) {
            return Err(Error::NotFound(id.to_string()));
        }
        let d = self.dir(id);
        io(fs::remove_dir_all(&d), &d)
    }

    /// Every paper id, sorted.
    pub fn ids(&self) -> Result<Vec<PaperId>> {
        let papers = self.root.join("papers");
        let mut ids = Vec::new();
        for entry in io(fs::read_dir(&papers), &papers)? {
            let entry = io(entry, &papers)?;
            let name = entry.file_name();
            if let Some(id) = name.to_str().and_then(|n| PaperId::parse(n).ok())
                && self.contains(&id)
            {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn papers(&self) -> Result<Vec<Paper>> {
        self.ids()?.iter().map(|id| self.load(id)).collect()
    }

    #[must_use]
    pub fn text_path(&self, id: &PaperId) -> PathBuf {
        self.dir(id).join("paper.md")
    }

    /// Full text, if it was fetched.
    pub fn text(&self, id: &PaperId) -> Result<Option<String>> {
        let p = self.text_path(id);
        if !p.is_file() {
            return Ok(None);
        }
        io(fs::read_to_string(&p), &p).map(Some)
    }

    pub fn save_text(&self, id: &PaperId, text: &str) -> Result<()> {
        write_atomic(&self.text_path(id), text)
    }

    /// Commit everything pending. A no-op when nothing changed or the folder is not a repo.
    pub fn commit(&self, message: &str) -> Result<()> {
        if !self.root.join(".git").exists() {
            return Ok(());
        }
        git(&self.root, &["add", "-A"])?;
        if git(&self.root, &["status", "--porcelain"])?
            .trim()
            .is_empty()
        {
            return Ok(());
        }
        git(&self.root, &["commit", "-q", "-m", message]).map(drop)
    }

    /// Commit, then pull (rebase) and push when a remote is configured.
    pub fn sync(&self) -> Result<SyncOutcome> {
        self.commit("sync")?;
        if git(&self.root, &["remote"])?.trim().is_empty() {
            return Ok(SyncOutcome::LocalOnly);
        }
        git(&self.root, &["fetch", "-q"])?;
        let pulled = count(&self.root, "HEAD..@{u}")?;
        if pulled > 0 {
            git(&self.root, &["pull", "-q", "--rebase", "--autostash"])?;
        }
        let pushed = count(&self.root, "@{u}..HEAD")?;
        if pushed > 0 {
            git(&self.root, &["push", "-q"])?;
        }
        Ok(SyncOutcome::Remote { pulled, pushed })
    }

    /// Local commits not on the upstream yet, as of the last fetch (no network).
    /// `None` when the folder has no remote or upstream branch.
    #[must_use]
    pub fn unpushed(&self) -> Option<u32> {
        count(&self.root, "@{u}..HEAD").ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyncOutcome {
    /// No remote configured: committed locally only.
    LocalOnly,
    /// Commits received from and sent to the remote.
    Remote { pulled: u32, pushed: u32 },
}

/// Number of commits in a revision range, e.g. `HEAD..@{u}`.
fn count(dir: &Path, range: &str) -> Result<u32> {
    let n = git(dir, &["rev-list", "--count", range])?;
    n.trim().parse().map_err(|_| Error::Command {
        cmd: format!("git rev-list --count {range}"),
        detail: format!("unexpected output {n:?}"),
    })
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
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

/// Pretty JSON with a trailing newline, so git diffs stay line-based.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut s = serde_json::to_string_pretty(value).map_err(|source| Error::Json {
        path: path.to_owned(),
        source,
    })?;
    s.push('\n');
    write_atomic(path, &s)
}

/// Write to a sibling temp file, then rename: a crash never leaves half a record.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    io(fs::write(&tmp, contents), &tmp)?;
    io(fs::rename(&tmp, path), path)
}
