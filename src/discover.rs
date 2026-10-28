//! New papers like the ones you keep, via Semantic Scholar's recommender.
//!
//! The library is the query: starred and recently added papers are the positive
//! examples, papers you skipped the negative ones. Each run lists what you have
//! not seen yet; you add what you like and skip the rest.

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::date::Day;
use crate::error::{Error, Result, io};
use crate::id::{ArxivId, PaperId};
use crate::library::{Library, write_atomic};
use crate::paper::Paper;
use crate::s2;

/// Semantic Scholar takes at most 100 example papers per request, positive and negative combined.
const MAX_SEEDS: usize = 100;
const MAX_NEGATIVE: usize = 20;
/// Pending candidates older than this drop out of the inbox.
const EXPIRE_DAYS: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    New,
    /// Dismissed: never listed again, and used as a negative example.
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: ArxivId,
    pub title: String,
    #[serde(default)]
    pub published: String,
    /// `YYYY-MM-DD` first listed.
    #[serde(default)]
    pub found: String,
    pub state: State,
}

impl Candidate {
    #[must_use]
    pub fn url(&self) -> String {
        format!("https://huggingface.co/papers/{}", self.id)
    }
}

/// `inbox.jsonl`, one candidate per line, sorted by id: line-based git merges stay clean.
#[derive(Debug)]
pub struct Inbox(BTreeMap<ArxivId, Candidate>);

impl Inbox {
    pub fn load(lib: &Library) -> Result<Self> {
        let path = lib.root().join("inbox.jsonl");
        if !path.exists() {
            return Ok(Self(BTreeMap::new()));
        }
        let raw = io(fs::read_to_string(&path), &path)?;
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<Candidate>(l)
                    .map(|c| (c.id.clone(), c))
                    .map_err(|source| Error::Json {
                        path: path.clone(),
                        source,
                    })
            })
            .collect::<Result<_>>()
            .map(Self)
    }

    pub fn save(&self, lib: &Library) -> Result<()> {
        let path = lib.root().join("inbox.jsonl");
        let mut out = String::new();
        for c in self.0.values() {
            let line = serde_json::to_string(c).map_err(|source| Error::Json {
                path: path.clone(),
                source,
            })?;
            out.push_str(&line);
            out.push('\n');
        }
        write_atomic(&path, &out)
    }

    /// Untriaged candidates, newest first.
    #[must_use]
    pub fn pending(&self) -> Vec<&Candidate> {
        let mut v: Vec<_> = self.0.values().filter(|c| c.state == State::New).collect();
        v.sort_by(|a, b| b.published.cmp(&a.published));
        v
    }

    /// Mark as skipped. False if `id` is not in the inbox.
    pub fn skip(&mut self, id: &ArxivId) -> bool {
        self.0
            .get_mut(id)
            .map(|c| c.state = State::Skipped)
            .is_some()
    }

    /// Drop a candidate (it was added to the library).
    pub fn take(&mut self, id: &ArxivId) -> Option<Candidate> {
        self.0.remove(id)
    }
}

/// Ask for `limit` recommendations, add the unseen ones to the inbox, and return them.
pub fn discover(lib: &Library, limit: u32) -> Result<Vec<Candidate>> {
    let papers = lib.papers()?;
    let mut inbox = Inbox::load(lib)?;
    let today = Day::today();

    let negative: Vec<ArxivId> = {
        let mut skipped: Vec<&Candidate> = inbox
            .0
            .values()
            .filter(|c| c.state == State::Skipped)
            .collect();
        skipped.sort_by(|a, b| b.found.cmp(&a.found));
        skipped
            .into_iter()
            .take(MAX_NEGATIVE)
            .map(|c| c.id.clone())
            .collect()
    };
    let positive = seeds(&papers, MAX_SEEDS - negative.len());
    if positive.is_empty() {
        return Err(Error::Usage(
            "add a few arXiv papers first: discover learns from them".into(),
        ));
    }

    let expired = today.minus(EXPIRE_DAYS).iso();
    inbox
        .0
        .retain(|_, c| c.state == State::Skipped || c.found >= expired);
    let mut fresh = Vec::new();
    for r in s2::recommend(&positive, &negative, limit)? {
        if lib.contains(&PaperId::Arxiv(r.id.clone())) || inbox.0.contains_key(&r.id) {
            continue;
        }
        let c = Candidate {
            id: r.id,
            title: r.title,
            published: r.published,
            found: today.iso(),
            state: State::New,
        };
        inbox.0.insert(c.id.clone(), c.clone());
        fresh.push(c);
    }
    inbox.save(lib)?;
    Ok(fresh)
}

/// Starred papers first, then the most recently added.
fn seeds(papers: &[Paper], n: usize) -> Vec<ArxivId> {
    let starred = |p: &Paper| p.tags.iter().any(|t| t.as_str() == "starred");
    let mut ranked: Vec<&Paper> = papers.iter().collect();
    ranked.sort_by(|a, b| {
        (starred(b), &b.added, &b.published).cmp(&(starred(a), &a.added, &a.published))
    });
    ranked
        .into_iter()
        .filter_map(|p| match p.id() {
            PaperId::Arxiv(id) => Some(id),
            PaperId::Slug(_) => None,
        })
        .take(n)
        .collect()
}
