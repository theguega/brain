//! `.cache/index.db`: a disposable SQLite view of the library.
//! Rebuilt whenever the files change, so it never needs to be synced or migrated.

use std::fmt::Write as _;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;

use crate::error::{Result, io};
use crate::library::Library;
use crate::paper::{Paper, wire_name};

/// Bump to force a rebuild after a schema change.
const SCHEMA: u32 = 1;

const DDL: &str = "
CREATE TABLE papers (
    id TEXT PRIMARY KEY, name TEXT, title TEXT, authors TEXT, published TEXT, added TEXT,
    tags TEXT, note TEXT, url TEXT, has_text INTEGER, has_card INTEGER,
    family TEXT, backbone TEXT, action_head TEXT, action_space TEXT,
    chunk_size INTEGER, control_hz REAL, embodiment TEXT,
    data_hours REAL, data_episodes INTEGER, data_source TEXT,
    open_weights INTEGER, open_code TEXT, open_data INTEGER,
    compute TEXT, limits TEXT, eval TEXT
);
CREATE VIRTUAL TABLE fts USING fts5(
    id UNINDEXED, name, title, abstract, keywords, note, body,
    tokenize = 'porter unicode61'
);
CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT);
";

/// Column weights for ranking, in `fts` column order (id, name, title, abstract, keywords, note, body).
const BM25: &str = "bm25(fts, 0, 10, 8, 4, 4, 4, 1)";

#[derive(Debug)]
pub struct Index {
    conn: Connection,
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub id: String,
    pub name: String,
    pub title: String,
    pub published: String,
    pub tags: Vec<String>,
    pub family: Option<String>,
    /// Matching excerpt, with hits in [brackets]. Empty for filter-only queries.
    pub snippet: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Query<'a> {
    /// Free text; empty for filter-only.
    pub text: &'a str,
    /// SQL predicate over `papers` columns, e.g. `family = 'vla' AND control_hz >= 30`.
    pub filter: Option<&'a str>,
    pub tags: &'a [String],
    pub limit: u32,
}

impl Index {
    /// Open the index, rebuilding it first if the library changed since the last build.
    pub fn open(lib: &Library) -> Result<Self> {
        let stamp = fingerprint(lib)?;
        let path = lib.cache().join("index.db");
        if let Ok(conn) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            && let Ok(v) = conn.query_row("SELECT v FROM meta WHERE k = 'stamp'", [], |r| {
                r.get::<_, String>(0)
            })
            && v == stamp
        {
            return Ok(Self { conn });
        }
        Self::rebuild(lib)?;
        Ok(Self {
            conn: Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?,
        })
    }

    /// Build from scratch into a temp file, then swap it in.
    pub fn rebuild(lib: &Library) -> Result<usize> {
        let dir = lib.cache();
        io(fs::create_dir_all(&dir), &dir)?;
        let tmp = dir.join("index.db.tmp");
        if tmp.exists() {
            io(fs::remove_file(&tmp), &tmp)?;
        }
        let stamp = fingerprint(lib)?;
        let mut conn = Connection::open(&tmp)?;
        conn.execute_batch(DDL)?;
        let papers = lib.papers()?;
        let tx = conn.transaction()?;
        for p in &papers {
            let text = lib.text(&p.id())?;
            insert(&tx, p, text.as_deref())?;
        }
        tx.execute("INSERT INTO meta VALUES ('stamp', ?1)", [&stamp])?;
        tx.commit()?;
        drop(conn);
        let path = dir.join("index.db");
        io(fs::rename(&tmp, &path), &path)?;
        Ok(papers.len())
    }

    pub fn search(&self, q: &Query<'_>) -> Result<Vec<Hit>> {
        let fts = fts_query(q.text);
        let mut sql = String::from("SELECT p.id, p.name, p.title, p.published, p.tags, p.family, ");
        sql.push_str(if fts.is_empty() {
            "'' FROM papers p WHERE 1"
        } else {
            "snippet(fts, -1, '[', ']', '…', 16) FROM fts JOIN papers p ON p.id = fts.id \
             WHERE fts MATCH :q"
        });
        // Writing to a String cannot fail.
        if let Some(f) = q.filter {
            let _ = write!(sql, " AND ({f})");
        }
        let tag_params: Vec<String> = (0..q.tags.len()).map(|i| format!(":t{i}")).collect();
        for t in &tag_params {
            let _ = write!(
                sql,
                " AND EXISTS (SELECT 1 FROM json_each(p.tags) WHERE value = {t})"
            );
        }
        if fts.is_empty() {
            sql.push_str(" ORDER BY p.published DESC");
        } else {
            let _ = write!(sql, " ORDER BY {BM25}");
        }
        sql.push_str(" LIMIT :limit");

        let limit = i64::from(q.limit);
        let mut binds: Vec<(&str, &dyn rusqlite::ToSql)> = vec![(":limit", &limit)];
        if !fts.is_empty() {
            binds.push((":q", &fts));
        }
        for (name, tag) in tag_params.iter().zip(q.tags) {
            binds.push((name, tag));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(binds.as_slice(), |r| {
            let tags: String = r.get(4)?;
            Ok(Hit {
                id: r.get(0)?,
                name: r.get(1)?,
                title: r.get(2)?,
                published: r.get(3)?,
                tags: serde_json::from_str(&tags).unwrap_or_default(),
                family: r.get(5)?,
                snippet: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every tag in use with its paper count, most used first.
    pub fn tags(&self) -> Result<Vec<(String, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.value, count(*) AS n FROM papers p, json_each(p.tags) t
             GROUP BY t.value ORDER BY n DESC, t.value",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Paper counts for the status view.
    pub fn stats(&self) -> Result<Stats> {
        Ok(self.conn.query_row(
            "SELECT count(*), count(*) - coalesce(sum(has_text), 0),
                    coalesce(sum(has_text AND NOT has_card), 0)
             FROM papers",
            [],
            |r| {
                Ok(Stats {
                    papers: r.get(0)?,
                    no_text: r.get(1)?,
                    no_card: r.get(2)?,
                })
            },
        )?)
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Stats {
    pub papers: u32,
    pub no_text: u32,
    /// Papers with text but no card (what `card todo` lists).
    pub no_card: u32,
}

fn insert(tx: &rusqlite::Transaction<'_>, p: &Paper, text: Option<&str>) -> Result<()> {
    let id = p.id().to_string();
    let c = p.card.as_ref();
    tx.execute(
        "INSERT INTO papers VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)",
        params![
            id,
            p.name,
            p.title,
            p.authors.join(", "),
            p.published,
            p.added,
            json(&p.tags),
            p.note,
            p.url(),
            text.is_some(),
            c.is_some(),
            c.and_then(|c| wire_name(&c.family)),
            c.and_then(|c| c.backbone.clone()),
            c.and_then(|c| c.action_head.as_ref().and_then(wire_name)),
            c.and_then(|c| c.action_space.as_ref().and_then(wire_name)),
            c.and_then(|c| c.chunk_size),
            c.and_then(|c| c.control_hz),
            c.map(|c| c.embodiment.join(", ")),
            c.and_then(|c| c.data.hours),
            c.and_then(|c| c.data.episodes).and_then(|e| i64::try_from(e).ok()),
            c.and_then(|c| c.data.source.as_ref().and_then(wire_name)),
            c.and_then(|c| c.open.weights),
            c.and_then(|c| c.open.code.clone()),
            c.and_then(|c| c.open.data),
            c.and_then(|c| c.compute.clone()),
            c.map(|c| json(&c.limits)),
            c.map(|c| json(&c.eval)),
        ],
    )?;
    tx.execute(
        "INSERT INTO fts VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            id,
            p.name,
            p.title,
            format!("{} {}", p.abstract_, p.summary),
            format!("{} {}", p.keywords.join(", "), json(&p.tags)),
            p.note,
            text.unwrap_or_default(),
        ],
    )?;
    Ok(())
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// Free text -> FTS5 query. Plain words become an AND of quoted terms, so `pi-0` or
/// `gr00t:` can't trip the FTS syntax. Text that already uses FTS syntax passes through.
fn fts_query(text: &str) -> String {
    let text = text.trim();
    if text.contains('"') || text.contains(" OR ") || text.contains(" NEAR") || text.ends_with('*')
    {
        return text.to_owned();
    }
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Cheap change detector: size and mtime of every paper file, plus the schema version.
fn fingerprint(lib: &Library) -> Result<String> {
    let mut h = DefaultHasher::new();
    SCHEMA.hash(&mut h);
    let papers = lib.root().join("papers");
    let mut dirs = Vec::new();
    for entry in io(fs::read_dir(&papers), &papers)? {
        dirs.push(io(entry, &papers)?.path());
    }
    dirs.sort();
    for dir in dirs {
        for f in ["paper.json", "paper.md"] {
            let p = dir.join(f);
            if let Ok(m) = fs::metadata(&p) {
                p.hash(&mut h);
                m.len().hash(&mut h);
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .hash(&mut h);
            }
        }
    }
    Ok(format!("{:016x}", h.finish()))
}
