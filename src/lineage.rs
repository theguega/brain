//! Papers with Code lineage, as the `builds_on` property: "A builds_on B" means B came
//! first. A paper's own note lists its PwC predecessors; each of its successors in the
//! vault gets the edge on its note, and the others are dropped (their own answer covers
//! them). A paper without `builds_on` has never had an answer from PwC; `reindex` retries
//! a few of those each time, and remembers when it tried in the index, so the vault holds
//! only what PwC said.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension as _, params};
use serde_json::Value;

use crate::doc::{self, Fields};
use crate::error::Result;
use crate::id::{ArxivId, DocId};
use crate::index::Index;
use crate::vault::Vault;
use crate::{note, pwc};

/// Papers retried per reindex: PwC is slow, and a reindex should stay quick.
pub const PER_REINDEX: usize = 10;
/// A paper tried within this many seconds is skipped.
const AGAIN_AFTER: i64 = 24 * 3600;

/// A PwC lookup: `Ok(None)` when PwC does not know the paper, `Err` when it is down.
pub type Lookup<'a> = &'a dyn Fn(&ArxivId) -> Result<Option<pwc::Paper>>;

/// The arXiv papers of a PwC list, once each, without `id` itself.
fn papers(id: &DocId, list: Option<&Vec<pwc::Related>>) -> Vec<DocId> {
    let mut out: Vec<DocId> = Vec::new();
    for r in list.into_iter().flatten() {
        if let Some(Ok(other)) = r.arxiv_id.as_deref().map(ArxivId::parse)
            && let other = DocId::Arxiv(other)
            && other != *id
            && !out.contains(&other)
        {
            out.push(other);
        }
    }
    out
}

/// `builds_on` as written: a wikilink to each doc's note.
#[must_use]
pub fn builds_on(ids: &[DocId]) -> Vec<String> {
    ids.iter().map(|id| note::wikilink(&id.dir())).collect()
}

/// What a PwC reply adds to a paper.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// The paper's PwC predecessors: what it builds on. `None` unless PwC has a record
    /// for the paper: a 404 or an empty `{}` is no answer, and the paper is retried. A
    /// record's `null` lists mean none, so `[]`.
    pub lineage: Option<Vec<DocId>>,
    /// Papers that build on it: each gets the edge on its own note (see [`link_successors`]).
    pub successors: Vec<DocId>,
    pub tldr: Option<String>,
    pub venue: Option<String>,
}

impl Answer {
    #[must_use]
    pub fn of(id: &DocId, found: Option<pwc::Paper>) -> Self {
        let Some(p) = found else {
            return Self::default();
        };
        let answered =
            p.arxiv_id.is_some() || p.predecessor_papers.is_some() || p.successor_papers.is_some();
        Self {
            lineage: answered.then(|| papers(id, p.predecessor_papers.as_ref())),
            successors: papers(id, p.successor_papers.as_ref()),
            tldr: p.tldr.filter(|t| !t.is_empty()),
            venue: p.conference_name.filter(|v| !v.is_empty()),
        }
    }

    /// Adds the predecessors to `builds_on` if answered, keeping what a successor's answer
    /// put there, and sets `tldr` and `venue` where unset. `true` if it changed.
    pub fn fill(&self, obj: &mut Fields) -> bool {
        let mut changed = false;
        if let Some(ids) = &self.lineage {
            changed |= add_links(obj, &builds_on(ids));
        }
        for (k, v) in [("tldr", &self.tldr), ("venue", &self.venue)] {
            if let Some(v) = v
                && !obj.contains_key(k)
            {
                obj.insert(k.to_owned(), Value::String(v.clone()));
                changed = true;
            }
        }
        changed
    }
}

/// Appends the wikilinks `builds_on` lacks, creating it. `true` if it changed.
fn add_links(obj: &mut Fields, links: &[String]) -> bool {
    let created = !obj.contains_key("builds_on");
    let slot = obj
        .entry("builds_on")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Value::Array(have) = slot else {
        return false;
    };
    let before = have.len();
    for l in links {
        if !have.iter().any(|h| h.as_str() == Some(l.as_str())) {
            have.push(Value::String(l.clone()));
        }
    }
    created || have.len() > before
}

/// Writes `builds_on: [[id]]` on each successor's note that is in the vault. One whose
/// PwC answer is still pending (no `builds_on`) is left alone: the edge would end its
/// retry, and its own answer lists `id` among its predecessors. The number written.
pub fn link_successors(vault: &Vault, id: &DocId, successors: &[DocId]) -> Result<usize> {
    let link = builds_on(std::slice::from_ref(id));
    let mut n = 0;
    for s in successors.iter().filter(|s| vault.has_doc(s)) {
        let rel = doc::path(s);
        let wrote = vault.update(&rel, |old| {
            let Some(old) = old else { return Ok(None) };
            let (mut fm, body) = note::read(&old, &rel)?;
            if fm.contains_key("builds_on") && add_links(&mut fm, &link) {
                doc::text(s, fm, &body).map(Some)
            } else {
                Ok(None)
            }
        })?;
        n += usize::from(wrote);
    }
    Ok(n)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Retried {
    pub filled: usize,
    /// Papers still without lineage, tried now or not.
    pub pending: usize,
    pub warning: Option<String>,
}

/// Retries at most `limit` papers without lineage that were not tried in the last day.
/// The first PwC failure ends the round: PwC is down, and the rest would fail too.
pub fn retry(vault: &Vault, index: &Path, limit: usize, lookup: Lookup<'_>) -> Result<Retried> {
    let ix = Index::open(index)?;
    ix.conn.execute(
        "CREATE TABLE IF NOT EXISTS lineage_tries (id TEXT PRIMARY KEY, at INTEGER NOT NULL)",
        [],
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let mut out = Retried::default();
    let mut tries = 0;
    for (id, aid) in without_lineage(vault)? {
        out.pending += 1;
        if tries == limit || out.warning.is_some() {
            continue;
        }
        let last: Option<i64> = ix
            .conn
            .query_row(
                "SELECT at FROM lineage_tries WHERE id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if last.is_some_and(|t| now - t < AGAIN_AFTER) {
            continue;
        }
        tries += 1;
        ix.conn.execute(
            "INSERT OR REPLACE INTO lineage_tries VALUES (?1, ?2)",
            params![id.to_string(), now],
        )?;
        match lookup(&aid) {
            Ok(found) => {
                let answer = Answer::of(&id, found);
                let answered = answer.lineage.is_some();
                let rel = doc::path(&id);
                vault.update(&rel, |old| {
                    let Some(old) = old else { return Ok(None) };
                    let (mut fm, body) = note::read(&old, &rel)?;
                    if answer.fill(&mut fm) {
                        doc::text(&id, fm, &body).map(Some)
                    } else {
                        Ok(None)
                    }
                })?;
                link_successors(vault, &id, &answer.successors)?;
                if answered {
                    out.filled += 1;
                    out.pending -= 1;
                }
            }
            Err(e) => out.warning = Some(format!("papers with code: {e}")),
        }
    }
    Ok(out)
}

/// arXiv papers whose note has no `builds_on` key, in id order.
fn without_lineage(vault: &Vault) -> Result<Vec<(DocId, ArxivId)>> {
    let docs = vault.path(Path::new("docs"));
    if !docs.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for e in crate::error::io(std::fs::read_dir(&docs), &docs)? {
        let name = crate::error::io(e, &docs)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        let Ok(id @ DocId::Arxiv(_)) = DocId::from_dir(&name) else {
            continue;
        };
        // An unreadable note is `brain lint`'s to report, not this retry's.
        let Ok(Some((meta, _))) = doc::read(vault, &id) else {
            continue;
        };
        if meta.get("kind").and_then(Value::as_str) == Some("paper")
            && meta.get("builds_on").is_none()
            && let DocId::Arxiv(aid) = &id
        {
            out.push((id.clone(), aid.clone()));
        }
    }
    out.sort();
    Ok(out)
}
