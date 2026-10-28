//! Papers with Code lineage. A paper whose `lineage` is unset has never had an answer
//! from PwC; `reindex` retries a few of those each time, and remembers when it tried in
//! the index, so the vault holds only what PwC said.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension as _, params};
use serde_json::Value;

use crate::doc::Edge;
use crate::error::{Error, Result};
use crate::id::{ArxivId, DocId, Rel};
use crate::index::Index;
use crate::pwc;
use crate::vault::Vault;

/// Papers retried per reindex: PwC is slow, and a reindex should stay quick.
pub const PER_REINDEX: usize = 10;
/// A paper tried within this many seconds is skipped.
const AGAIN_AFTER: i64 = 24 * 3600;

/// A PwC lookup: `Ok(None)` when PwC does not know the paper, `Err` when it is down.
pub type Lookup<'a> = &'a dyn Fn(&ArxivId) -> Result<Option<pwc::Paper>>;

/// PwC predecessors and successors as `builds_on` edges, whether or not they are in the vault.
#[must_use]
pub fn edges(id: &DocId, pwc: &pwc::Paper) -> Vec<Edge> {
    let ids = |v: &Option<Vec<pwc::Related>>| {
        v.iter()
            .flatten()
            .filter_map(|r| {
                ArxivId::parse(r.arxiv_id.as_deref()?)
                    .ok()
                    .map(DocId::Arxiv)
            })
            .collect::<Vec<_>>()
    };
    let Ok(rel) = Rel::parse(Rel::BUILDS_ON) else {
        return Vec::new();
    };
    let edge = |src: DocId, dst: DocId| Edge {
        src,
        dst,
        rel: rel.clone(),
    };
    let mut out: Vec<Edge> = ids(&pwc.predecessor_papers)
        .into_iter()
        .map(|p| edge(id.clone(), p))
        .collect();
    out.extend(
        ids(&pwc.successor_papers)
            .into_iter()
            .map(|s| edge(s, id.clone())),
    );
    out
}

/// What a PwC reply adds to a paper.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// `None` unless PwC sent its predecessor or successor list: a 404 or an empty
    /// record is no answer, and the paper is retried.
    pub lineage: Option<Vec<Edge>>,
    pub tldr: Option<String>,
    pub venue: Option<String>,
}

impl Answer {
    #[must_use]
    pub fn of(id: &DocId, found: Option<pwc::Paper>) -> Self {
        let Some(p) = found else {
            return Self::default();
        };
        let answered = p.predecessor_papers.is_some() || p.successor_papers.is_some();
        Self {
            lineage: answered.then(|| edges(id, &p)),
            tldr: p.tldr.filter(|t| !t.is_empty()),
            venue: p.conference_name.filter(|v| !v.is_empty()),
        }
    }

    /// Sets `lineage` if answered, and `tldr` and `venue` where unset. `true` if it changed.
    pub fn fill(self, meta: &mut Value) -> Result<bool> {
        let Some(obj) = meta.as_object_mut() else {
            return Ok(false);
        };
        let mut changed = false;
        if let Some(edges) = self.lineage {
            let v = serde_json::to_value(edges).map_err(|source| Error::Decode {
                what: "lineage",
                source,
            })?;
            obj.insert("lineage".to_owned(), v);
            changed = true;
        }
        for (k, v) in [("tldr", self.tldr), ("venue", self.venue)] {
            if let Some(v) = v
                && !obj.contains_key(k)
            {
                obj.insert(k.to_owned(), Value::String(v));
                changed = true;
            }
        }
        Ok(changed)
    }
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
                let rel = Vault::doc_dir(&id).join("meta.json");
                vault.update(&rel, |old| {
                    let Some(old) = old else { return Ok(None) };
                    let mut meta: Value =
                        serde_json::from_str(&old).map_err(|source| Error::Json {
                            path: rel.clone(),
                            source,
                        })?;
                    if answer.fill(&mut meta)? {
                        pretty(&meta, &rel).map(Some)
                    } else {
                        Ok(None)
                    }
                })?;
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

/// Pretty JSON with a trailing newline, as [`Vault::write_json`] writes it.
pub(crate) fn pretty(v: &Value, rel: &Path) -> Result<String> {
    let mut s = serde_json::to_string_pretty(v).map_err(|source| Error::Json {
        path: rel.to_owned(),
        source,
    })?;
    s.push('\n');
    Ok(s)
}

/// arXiv papers whose meta has no `lineage` key, in id order.
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
        let rel = Vault::doc_dir(&id).join("meta.json");
        let Some(raw) = vault.read(&rel)? else {
            continue;
        };
        // An unreadable meta is `brain lint`'s to report, not this retry's.
        let Ok(meta) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if meta.get("kind").and_then(Value::as_str) == Some("paper")
            && meta.get("lineage").is_none()
            && let DocId::Arxiv(aid) = &id
        {
            out.push((id.clone(), aid.clone()));
        }
    }
    out.sort();
    Ok(out)
}
