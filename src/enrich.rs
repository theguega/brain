//! Backfills a paper's `hub:` block for notes added before add wrote one.
//!
//! `add` short-circuits on a doc it already has, so a note written before the block existed
//! never gains one. The Hub's answer is cheap but not free: it is the same bucket as the
//! markdown, 500 requests per 5 minutes anonymously, 1000 with a token. A vault of 756 papers
//! is therefore eight minutes of fetching, not a burst, so this runs a few papers per tick
//! and remembers when it last asked, exactly like the lineage retry.
//!
//! The markdown bucket is never touched: the text is already stored, only the record is refetched.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension as _, params};

use crate::doc::{self, Hub};
use crate::error::Result;
use crate::id::{ArxivId, DocId};
use crate::index::Index;
use crate::vault::Vault;
use crate::{hub, lineage, note};

/// Papers refetched per tick. At one tick per 15 minutes this clears a large vault in a day
/// without coming near the Hub's 500-per-5-minutes bucket.
pub const PER_TICK: usize = 25;

/// A paper asked about within this many seconds is skipped.
const AGAIN_AFTER: i64 = 7 * 24 * 3600;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filled {
    pub filled: usize,
    /// Papers still without the block, tried now or not.
    pub pending: usize,
    pub warning: Option<String>,
}

/// Refetches the Hub record for at most `limit` papers that have no `hub:` key.
/// The first Hub failure ends the round: the rest would fail the same way.
pub fn retry(
    vault: &Vault,
    index: &Path,
    limit: usize,
    lookup: &dyn Fn(&ArxivId) -> Result<Option<hub::Info>>,
) -> Result<Filled> {
    // A note with no block because the Hub simply knows nothing about the paper would be
    // refetched forever. The block records what the Hub said, including "nothing": an empty
    // `hub: {}` is written once and ends the retry.
    let ix = Index::open(index)?;
    ix.conn.execute(
        "CREATE TABLE IF NOT EXISTS hub_tries (id TEXT PRIMARY KEY, at INTEGER NOT NULL)",
        [],
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |i| i64::try_from(i.as_secs()).unwrap_or(i64::MAX));
    let mut out = Filled::default();
    let mut tries = 0;
    for (id, aid) in lineage::papers_without(vault, "hub")? {
        out.pending += 1;
        if tries == limit || out.warning.is_some() {
            continue;
        }
        let last: Option<i64> = ix
            .conn
            .query_row(
                "SELECT at FROM hub_tries WHERE id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if last.is_some_and(|t| now - t < AGAIN_AFTER) {
            continue;
        }
        tries += 1;
        ix.conn.execute(
            "INSERT OR REPLACE INTO hub_tries VALUES (?1, ?2)",
            params![id.to_string(), now],
        )?;
        match lookup(&aid) {
            Ok(info) => {
                let block = info.as_ref().and_then(hub::Info::block).unwrap_or_default();
                write(vault, &id, &block)?;
                out.filled += 1;
                out.pending -= 1;
            }
            Err(e) => out.warning = Some(format!("hugging face hub: {e}")),
        }
    }
    Ok(out)
}

/// Sets `hub:` on the note, or `hub: {}` when the Hub had nothing. `false` if it could not.
fn write(vault: &Vault, id: &DocId, block: &Hub) -> Result<bool> {
    let rel = doc::path(id);
    vault.update(&rel, |old| {
        let Some(old) = old else { return Ok(None) };
        let (mut fm, body) = note::read(&old, &rel)?;
        if fm.contains_key("hub") {
            return Ok(None);
        }
        fm.insert(
            "hub".to_owned(),
            serde_json::to_value(block).map_err(|source| crate::Error::Decode {
                what: "hub block",
                source,
            })?,
        );
        doc::text(id, fm, &body).map(Some)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn paper(vault: &Vault, aid: &str) -> DocId {
        let id = DocId::Arxiv(ArxivId::parse(aid).unwrap());
        let fm = crate::note::parse(&format!(
            "id: {id}\nkind: paper\ntaint: external\nadded: 2026-01-01T00:00:00+00:00\n\
             title: T\nauthors: [A]\ndate: 2026-01-01\nlinks: {{}}\n"
        ))
        .unwrap();
        vault
            .write(&doc::path(&id), crate::doc::text(&id, fm, "body").unwrap())
            .unwrap();
        id
    }

    #[test]
    fn a_paper_the_hub_knows_nothing_about_is_written_once_and_let_go() {
        let d = std::env::temp_dir().join(format!("brain-enrich-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let vault = Vault::open_or_create(&d).unwrap();
        let id = paper(&vault, "2501.00001");
        let db = d.join("index.db");
        Index::open(&db).unwrap();
        let calls = RefCell::new(0);
        let lookup = |_: &ArxivId| {
            *calls.borrow_mut() += 1;
            Ok(None)
        };

        let first = retry(&vault, &db, PER_TICK, &lookup).unwrap();
        assert_eq!(first.filled, 1, "the block is written even when empty");
        // The note now carries `hub: {}`, so the next pass has nothing to do.
        let (fm, _) = doc::read(&vault, &id).unwrap().unwrap();
        assert!(fm.contains_key("hub"));
        let second = retry(&vault, &db, PER_TICK, &lookup).unwrap();
        assert_eq!(second.pending, 0);
        assert_eq!(*calls.borrow(), 1, "not asked twice");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn one_round_stops_at_the_limit_and_keeps_the_rest_pending() {
        let d = std::env::temp_dir().join(format!("brain-enrich-lim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let vault = Vault::open_or_create(&d).unwrap();
        for i in 1..=5 {
            paper(&vault, &format!("2501.0000{i}"));
        }
        let db = d.join("index.db");
        Index::open(&db).unwrap();
        let calls = RefCell::new(0);
        let lookup = |_: &ArxivId| {
            *calls.borrow_mut() += 1;
            Ok(None)
        };
        let out = retry(&vault, &db, 2, &lookup).unwrap();
        assert_eq!(*calls.borrow(), 2, "budgeted");
        assert_eq!(out.pending, 3, "the other three wait for the next tick");
        let _ = std::fs::remove_dir_all(&d);
    }
}
