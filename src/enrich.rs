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

use crate::doc;
use crate::error::Result;
use crate::id::{ArxivId, DocId};
use crate::index::Index;
use crate::vault::Vault;
use crate::{alphaxiv, hub, lineage, note};

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

/// Refetches one enrichment for at most `limit` papers that lack `key` in their frontmatter.
/// `fill` turns a fetched record into the frontmatter value to store; `what` names the source
/// in the warning. The first failure ends the round: the rest would fail the same way.
pub fn retry<T>(
    vault: &Vault,
    index: &Path,
    key: &str,
    what: &str,
    limit: usize,
    fetch: &dyn Fn(&ArxivId) -> Result<Option<T>>,
    fill: impl Fn(&T) -> serde_json::Value,
) -> Result<Filled> {
    // A note with no block because the source simply knows nothing would be refetched forever.
    // The block records what the source said, including "nothing": an empty `key: {}` is
    // written once and ends the retry.
    let ix = Index::open(index)?;
    ix.conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {key}_tries (id TEXT PRIMARY KEY, at INTEGER NOT NULL)"
        ),
        [],
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |i| i64::try_from(i.as_secs()).unwrap_or(i64::MAX));
    let mut out = Filled::default();
    let mut tries = 0;
    for (id, aid) in lineage::papers_without(vault, key)? {
        out.pending += 1;
        if tries == limit || out.warning.is_some() {
            continue;
        }
        let last: Option<i64> = ix
            .conn
            .query_row(
                &format!("SELECT at FROM {key}_tries WHERE id = ?1"),
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if last.is_some_and(|t| now - t < AGAIN_AFTER) {
            continue;
        }
        tries += 1;
        ix.conn.execute(
            &format!("INSERT OR REPLACE INTO {key}_tries VALUES (?1, ?2)"),
            params![id.to_string(), now],
        )?;
        match fetch(&aid) {
            Ok(found) => {
                let value = found
                    .as_ref()
                    .map_or_else(|| serde_json::Value::Object(serde_json::Map::new()), &fill);
                write(vault, &id, key, value)?;
                out.filled += 1;
                out.pending -= 1;
            }
            Err(e) => out.warning = Some(format!("{what}: {e}")),
        }
    }
    Ok(out)
}

/// Sets `key` on the note, `false` if it is already there. Under the vault lock, as reindex
/// may be filling the same note.
fn write(vault: &Vault, id: &DocId, key: &str, value: serde_json::Value) -> Result<bool> {
    let rel = doc::path(id);
    vault.update(&rel, |old| {
        let Some(old) = old else { return Ok(None) };
        let (mut fm, body) = note::read(&old, &rel)?;
        if fm.contains_key(key) {
            return Ok(None);
        }
        fm.insert(key.to_owned(), value.clone());
        doc::text(id, fm, &body).map(Some)
    })
}

/// A record's block as a frontmatter value.
fn block<T: serde::Serialize>(record: &T) -> serde_json::Value {
    serde_json::to_value(record)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
}

/// The Hub's `hub:` block.
pub fn hub(vault: &Vault, index: &Path, limit: usize) -> Result<Filled> {
    retry(
        vault,
        index,
        "hub",
        "hugging face hub",
        limit,
        &|aid| hub::info(aid).map(|i| i.and_then(|i| i.block())),
        block,
    )
}

/// alphaXiv's `axiv:` block.
pub fn axiv(vault: &Vault, index: &Path, limit: usize) -> Result<Filled> {
    retry(
        vault,
        index,
        "axiv",
        "alphaxiv",
        limit,
        &|aid| alphaxiv::info(aid).map(|i| i.map(|i| i.outline).filter(|o| !o.is_empty())),
        block,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A vault dir `Vault::open` accepts, without `open_or_create`'s initial commit: that
    /// needs a git identity, which CI does not have and a unit test must not set globally.
    fn vault(dir: &std::path::Path) -> Vault {
        std::fs::create_dir_all(dir.join(".git")).expect("a vault dir");
        Vault::open(dir).expect("a vault with .git")
    }

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
        let vault = vault(&d);
        let id = paper(&vault, "2501.00001");
        let db = d.join("index.db");
        Index::open(&db).unwrap();
        let calls = RefCell::new(0);
        let lookup = |_: &ArxivId| -> Result<Option<crate::doc::Hub>> {
            *calls.borrow_mut() += 1;
            Ok(None)
        };

        let first = retry(&vault, &db, "hub", "hub", PER_TICK, &lookup, block).unwrap();
        assert_eq!(first.filled, 1, "the block is written even when empty");
        // The note now carries `hub: {}`, so the next pass has nothing to do.
        let (fm, _) = doc::read(&vault, &id).unwrap().unwrap();
        assert!(fm.contains_key("hub"));
        let second = retry(&vault, &db, "hub", "hub", PER_TICK, &lookup, block).unwrap();
        assert_eq!(second.pending, 0);
        assert_eq!(*calls.borrow(), 1, "not asked twice");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn one_round_stops_at_the_limit_and_keeps_the_rest_pending() {
        let d = std::env::temp_dir().join(format!("brain-enrich-lim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let vault = vault(&d);
        for i in 1..=5 {
            paper(&vault, &format!("2501.0000{i}"));
        }
        let db = d.join("index.db");
        Index::open(&db).unwrap();
        let calls = RefCell::new(0);
        let lookup = |_: &ArxivId| -> Result<Option<crate::doc::Hub>> {
            *calls.borrow_mut() += 1;
            Ok(None)
        };
        let out = retry(&vault, &db, "hub", "hub", 2, &lookup, block).unwrap();
        assert_eq!(*calls.borrow(), 2, "budgeted");
        assert_eq!(out.pending, 3, "the other three wait for the next tick");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_populated_block_lands_in_the_note() {
        let d = std::env::temp_dir().join(format!("brain-enrich-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let vault = vault(&d);
        let id = paper(&vault, "2410.24164");
        let db = d.join("index.db");
        Index::open(&db).unwrap();
        let lookup = |_: &ArxivId| {
            Ok(Some(crate::doc::Hub {
                models: vec!["lerobot/pi0_old".to_owned()],
                counts: crate::doc::HubCounts {
                    models: 114,
                    ..crate::doc::HubCounts::default()
                },
                upvotes: Some(31),
                ..crate::doc::Hub::default()
            }))
        };
        retry(&vault, &db, "hub", "hub", PER_TICK, &lookup, block).unwrap();
        let (fm, body) = doc::read(&vault, &id).unwrap().unwrap();
        assert_eq!(fm["hub"]["counts"]["models"], 114);
        assert_eq!(fm["hub"]["models"][0], "lerobot/pi0_old");
        assert_eq!(body, "body", "the text is left alone");
        let _ = std::fs::remove_dir_all(&d);
    }
}
