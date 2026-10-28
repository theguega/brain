//! `~/.cache/brain/index.db`: chunks, FTS5, vectors and links, rebuilt from the vault
//! and the registered repos alone (B1). Files win: whatever the index says, `reindex`
//! makes it match them again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde_json::{Value, json};

use crate::chunk::{self, Chunk};
use crate::doc::Taint;
use crate::embed::{self, Embedder};
use crate::error::{Error, Result, io};
use crate::id::{DocId, Slug};
use crate::log::Entry;
use crate::repos::{self, Repo, Scan};
use crate::vault::Vault;
use crate::{contract::Kind, page};

/// Bumped on any change to the tables or to chunking: a mismatch drops the index.
pub const SCHEMA: i32 = 2;

const DDL: &str = "
CREATE TABLE docs (id TEXT PRIMARY KEY, kind TEXT NOT NULL, title TEXT NOT NULL,
    date TEXT NOT NULL, meta TEXT NOT NULL, hash TEXT NOT NULL, taint TEXT NOT NULL);
CREATE TABLE chunks (id INTEGER PRIMARY KEY, doc TEXT NOT NULL, ord INTEGER NOT NULL,
    heading TEXT NOT NULL, text TEXT NOT NULL);
CREATE INDEX chunks_doc ON chunks (doc, ord);
CREATE VIRTUAL TABLE chunks_fts USING fts5 (heading, text, tokenize = 'porter unicode61');
CREATE TABLE vectors (chunk INTEGER PRIMARY KEY, model TEXT NOT NULL, dim INTEGER NOT NULL,
    v BLOB NOT NULL);
CREATE TABLE links (src TEXT NOT NULL, dst TEXT NOT NULL, rel TEXT NOT NULL);
CREATE INDEX links_src ON links (src);
CREATE TABLE repos (name TEXT PRIMARY KEY, path TEXT NOT NULL, remote TEXT,
    head TEXT NOT NULL, indexed_at TEXT NOT NULL);
";

/// One indexed unit: a vault doc, a page, a log block, a repo file, commit or card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Source {
    pub id: String,
    pub kind: String,
    pub title: String,
    /// `YYYY-MM-DD`, or empty when unknown.
    pub date: String,
    pub meta: Value,
    pub taint: Taint,
    pub text: String,
    /// `(dst, rel)`.
    pub links: Vec<(String, String)>,
}

impl Source {
    /// Over every field, so a doc is redone whenever anything stored about it changes.
    fn hash(&self) -> String {
        let s = json!([
            self.kind, self.title, self.date, self.meta, self.taint, self.text, self.links
        ])
        .to_string();
        // FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
        let h = s.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        format!("{h:016x}")
    }

    fn chunks(&self) -> Vec<Chunk> {
        let cs = chunk::chunks(&self.text);
        if cs.is_empty() {
            // Still findable by its title.
            return vec![Chunk {
                heading: String::new(),
                text: self.title.clone(),
                context: String::new(),
            }];
        }
        cs
    }
}

#[derive(Debug)]
pub struct Index {
    pub(crate) conn: Connection,
}

/// `$XDG_CACHE_HOME/brain/index.db`, else `~/.cache/brain/index.db`.
pub fn db_path() -> Result<PathBuf> {
    Ok(xdg("XDG_CACHE_HOME", ".cache")?.join("brain/index.db"))
}

/// `$<var>`, else `~/<fallback>`.
pub(crate) fn xdg(var: &str, fallback: &str) -> Result<PathBuf> {
    match std::env::var_os(var) {
        Some(d) if !d.is_empty() => Ok(PathBuf::from(d)),
        _ => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(fallback))
            .ok_or_else(|| Error::Usage("HOME is not set".to_owned())),
    }
}

impl Index {
    /// Opens the index, creating it, or dropping it when its schema is not [`SCHEMA`].
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            io(fs::create_dir_all(dir), dir)?;
        }
        let conn = connect(path)?;
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == SCHEMA {
            return Ok(Self { conn });
        }
        drop(conn);
        remove(path)?;
        let conn = connect(path)?;
        conn.execute_batch(DDL)?;
        conn.pragma_update(None, "user_version", SCHEMA)?;
        Ok(Self { conn })
    }

    /// For readers: an index that `reindex` built.
    pub fn open_existing(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::NoIndex);
        }
        let ix = Self::open(path)?;
        let built: bool = ix
            .conn
            .query_row("SELECT EXISTS (SELECT 1 FROM docs)", [], |r| r.get(0))?;
        if built { Ok(ix) } else { Err(Error::NoIndex) }
    }
}

fn connect(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(conn)
}

fn remove(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        let p = PathBuf::from(p);
        match fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return io(Err(e), &p),
            _ => {}
        }
    }
    Ok(())
}

/// What `brain reindex` prints.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub docs: usize,
    pub changed: usize,
    pub removed: usize,
    pub chunks: usize,
    pub embedded: usize,
    /// Chunks still without a vector; the next reindex fills them.
    pub unembedded: usize,
    pub repos: usize,
    pub warnings: Vec<String>,
}

/// Incremental by content hash; `full` starts from an empty index.
pub fn reindex(vault: &Vault, path: &Path, full: bool, embedder: &Embedder) -> Result<Counts> {
    if full {
        remove(path)?;
    }
    let mut ix = Index::open(path)?;
    let mut counts = Counts::default();
    let repos = repos::discover(&repos::Config::load(vault.root())?)?;
    let now = crate::time::now()?.at;

    let tx = ix.conn.transaction()?;
    let mut seen = BTreeSet::new();
    let mut changed = 0;
    vault_sources(vault, &mut counts.warnings, |s| {
        changed += usize::from(upsert(&tx, &s)?);
        seen.insert(s.id);
        Ok(())
    })?;
    counts.changed = changed;
    let mut scanned: Vec<(&Repo, Scan)> = Vec::new();
    for repo in &repos {
        let last: Option<String> = tx
            .query_row(
                "SELECT head FROM repos WHERE name = ?1",
                [&repo.name],
                |r| r.get(0),
            )
            .optional()?;
        let mut scan = match repos::scan(repo, last.as_deref()) {
            Ok(Some(scan)) => scan,
            Ok(None) => continue,
            Err(e) => {
                counts.warnings.push(format!("repo {}: {e}", repo.name));
                continue;
            }
        };
        counts.warnings.extend(
            scan.skipped
                .iter()
                .map(|f| format!("repo {}: {f} is too big to index", repo.name)),
        );
        let commits = match scan.commits.take() {
            Some(c) => c,
            // HEAD has not moved: the commits indexed for it are still the set.
            None => {
                let mut stmt = tx.prepare("SELECT id FROM docs WHERE id LIKE ?1")?;
                let ids = stmt
                    .query_map([format!("commit:{}@%", repo.name)], |r| {
                        r.get::<_, String>(0)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                seen.extend(ids);
                Vec::new()
            }
        };
        for s in std::mem::take(&mut scan.docs).iter().chain(&commits) {
            counts.changed += usize::from(upsert(&tx, s)?);
            seen.insert(s.id.clone());
        }
        tx.execute(
            "INSERT OR REPLACE INTO repos VALUES (?1, ?2, ?3, ?4, ?5)",
            params![repo.name, repo.rel, scan.remote, scan.head, now],
        )?;
        scanned.push((repo, scan));
    }
    for (repo, scan) in &scanned {
        let card = repos::card(repo, scan, &linked_from(&tx, repo, scan.remote.as_deref())?);
        counts.changed += usize::from(upsert(&tx, &card)?);
        seen.insert(card.id);
    }

    let names: BTreeSet<&str> = scanned.iter().map(|(r, _)| r.name.as_str()).collect();
    let stale: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM docs")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().filter(|id| !seen.contains(id)).collect()
    };
    for id in &stale {
        delete(&tx, id)?;
    }
    counts.removed = stale.len();
    {
        let mut stmt = tx.prepare("SELECT name FROM repos")?;
        let gone: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(std::result::Result::ok)
            .filter(|n| !names.contains(n.as_str()))
            .collect();
        for n in gone {
            tx.execute("DELETE FROM repos WHERE name = ?1", [n])?;
        }
    }
    tx.commit()?;

    let (embedded, err) = embed_missing(&mut ix, embedder);
    counts.embedded = embedded;
    if let Some(e) = err {
        counts.warnings.push(format!("embedder: {e}"));
    }
    let count = |sql: &str| -> Result<usize> {
        Ok(ix
            .conn
            .query_row(sql, [], |r| r.get::<_, i64>(0))?
            .try_into()
            .unwrap_or(0))
    };
    counts.docs = count("SELECT count(*) FROM docs")?;
    counts.chunks = count("SELECT count(*) FROM chunks")?;
    counts.unembedded = missing_vectors(&ix, &embedder.model)?;
    counts.repos = count("SELECT count(*) FROM repos")?;
    Ok(counts)
}

/// Re-reads these pages alone (the ones a merge changed), then fills missing vectors.
/// Without a built index there is nothing to update: the next reindex builds it.
pub fn reindex_pages(
    vault: &Vault,
    path: &Path,
    slugs: &[Slug],
    embedder: &Embedder,
) -> Result<Vec<String>> {
    let mut ix = match Index::open_existing(path) {
        Ok(ix) => ix,
        Err(Error::NoIndex) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut warnings = Vec::new();
    let tx = ix.conn.transaction()?;
    for slug in slugs {
        if vault.path(&page::path(slug)).is_file() {
            match page_source(vault, slug.as_str()) {
                Ok(s) => {
                    upsert(&tx, &s)?;
                }
                Err(e) => warnings.push(format!("pages/{slug}.md: {e}")),
            }
        } else {
            delete(&tx, slug.as_str())?;
        }
    }
    tx.commit()?;
    if let (_, Some(e)) = embed_missing(&mut ix, embedder) {
        warnings.push(format!("embedder: {e}"));
    }
    Ok(warnings)
}

/// `true` when the doc was new or changed.
fn upsert(tx: &Transaction<'_>, s: &Source) -> Result<bool> {
    let hash = s.hash();
    let old: Option<String> = tx
        .query_row("SELECT hash FROM docs WHERE id = ?1", [&s.id], |r| r.get(0))
        .optional()?;
    if old.as_deref() == Some(hash.as_str()) {
        return Ok(false);
    }
    delete(tx, &s.id)?;
    let taint = match s.taint {
        Taint::Own => "own",
        Taint::External => "external",
    };
    tx.execute(
        "INSERT INTO docs VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            s.id,
            s.kind,
            s.title,
            s.date,
            s.meta.to_string(),
            hash,
            taint
        ],
    )?;
    for (ord, c) in (1_i64..).zip(s.chunks()) {
        tx.execute(
            "INSERT INTO chunks (doc, ord, heading, text) VALUES (?1, ?2, ?3, ?4)",
            params![s.id, ord, c.heading, c.text],
        )?;
        let id = tx.last_insert_rowid();
        // The title rides in the heading column, so its weight is the title's.
        let heading = format!("{} {}", s.title, c.heading);
        let text = if c.context.is_empty() {
            c.text
        } else {
            format!("{}\n\n{}", c.context, c.text)
        };
        tx.execute(
            "INSERT INTO chunks_fts (rowid, heading, text) VALUES (?1, ?2, ?3)",
            params![id, heading.trim(), text],
        )?;
    }
    for (dst, rel) in &s.links {
        tx.execute(
            "INSERT INTO links VALUES (?1, ?2, ?3)",
            params![s.id, dst, rel],
        )?;
    }
    Ok(true)
}

fn delete(tx: &Transaction<'_>, id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE doc = ?1)",
        [id],
    )?;
    tx.execute(
        "DELETE FROM vectors WHERE chunk IN (SELECT id FROM chunks WHERE doc = ?1)",
        [id],
    )?;
    tx.execute("DELETE FROM chunks WHERE doc = ?1", [id])?;
    tx.execute("DELETE FROM links WHERE src = ?1", [id])?;
    tx.execute("DELETE FROM docs WHERE id = ?1", [id])?;
    Ok(())
}

/// Docs and pages that link to the repo: by its name, its card id, or its remote.
fn linked_from(tx: &Transaction<'_>, repo: &Repo, remote: Option<&str>) -> Result<Vec<String>> {
    let remote = remote.map(repos::canonical_url);
    let card = format!("repo:{}", repo.name);
    let mut stmt = tx.prepare("SELECT src, dst FROM links WHERE src NOT LIKE 'repo:%'")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = BTreeSet::new();
    for row in rows {
        let (src, dst) = row?;
        if dst == repo.name
            || dst == card
            || remote.as_deref() == Some(repos::canonical_url(&dst).as_str())
        {
            out.insert(src);
        }
    }
    Ok(out.into_iter().collect())
}

/// Chunks without a vector from `model`: vectors from another model count as missing.
pub(crate) fn missing_vectors(ix: &Index, model: &str) -> Result<usize> {
    let n: i64 = ix.conn.query_row(
        "SELECT count(*) FROM chunks WHERE id NOT IN (SELECT chunk FROM vectors WHERE model = ?1)",
        [model],
        |r| r.get(0),
    )?;
    Ok(usize::try_from(n).unwrap_or(0))
}

/// Fills every chunk that has no vector from the embedder's model, one batch in memory
/// at a time. Stops at the first embedder failure, keeping the vectors so far.
fn embed_missing(ix: &mut Index, embedder: &Embedder) -> (usize, Option<Error>) {
    let mut done = 0;
    let mut after = 0_i64;
    loop {
        let batch: Result<Vec<(i64, String)>> = (|| {
            let mut stmt = ix.conn.prepare(
                "SELECT c.id, f.heading, f.text FROM chunks c JOIN chunks_fts f ON f.rowid = c.id
                 WHERE c.id > ?1 AND c.id NOT IN (SELECT chunk FROM vectors WHERE model = ?2)
                 ORDER BY c.id LIMIT ?3",
            )?;
            let limit = i64::try_from(embed::BATCH).unwrap_or(1);
            let rows = stmt.query_map(params![after, embedder.model, limit], |r| {
                Ok((
                    r.get(0)?,
                    format!("{}\n{}", r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })();
        let batch = match batch {
            Ok(b) if b.is_empty() => return (done, None),
            Ok(b) => b,
            Err(e) => return (done, Some(e)),
        };
        let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
        let stored = embedder.embed(&texts).and_then(|vs| {
            let tx = ix.conn.transaction()?;
            for ((id, _), v) in batch.iter().zip(&vs) {
                tx.execute(
                    "INSERT OR REPLACE INTO vectors VALUES (?1, ?2, ?3, ?4)",
                    params![
                        id,
                        embedder.model,
                        i64::try_from(v.len()).unwrap_or(0),
                        embed::to_blob(v)
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        });
        match stored {
            Ok(()) => done += batch.len(),
            Err(e) => return (done, Some(e)),
        }
        after = batch.last().map_or(after, |(id, _)| *id);
    }
}

/// Docs, pages and log blocks, one at a time into `f`, so the vault is never held in
/// memory. Unreadable files are warnings, not failures: `brain lint` is where they get fixed.
fn vault_sources(
    vault: &Vault,
    warnings: &mut Vec<String>,
    mut f: impl FnMut(Source) -> Result<()>,
) -> Result<()> {
    for name in entries(vault, "docs")? {
        match doc_source(vault, &name) {
            Ok(s) => f(s)?,
            Err(e) => warnings.push(format!("docs/{name}: {e}")),
        }
    }
    for name in entries(vault, "pages")? {
        let Some(slug) = name.strip_suffix(".md") else {
            continue;
        };
        match page_source(vault, slug) {
            Ok(s) => f(s)?,
            Err(e) => warnings.push(format!("pages/{name}: {e}")),
        }
    }
    for name in entries(vault, "log")? {
        if name.ends_with(".md") {
            let text = vault
                .read(&Path::new("log").join(&name))?
                .unwrap_or_default();
            for s in log_sources(&text) {
                f(s)?;
            }
        }
    }
    Ok(())
}

fn entries(vault: &Vault, dir: &str) -> Result<Vec<String>> {
    let path = vault.path(Path::new(dir));
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for e in io(fs::read_dir(&path), &path)? {
        let name = io(e, &path)?.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn doc_source(vault: &Vault, dir: &str) -> Result<Source> {
    let id = DocId::from_dir(dir)?;
    let rel = Path::new("docs").join(dir);
    let meta_path = rel.join("meta.json");
    let raw = vault.read(&meta_path)?.ok_or_else(|| Error::Invalid {
        path: meta_path.clone(),
        why: "missing".to_owned(),
    })?;
    let meta: Value = serde_json::from_str(&raw).map_err(|source| Error::Json {
        path: meta_path,
        source,
    })?;
    let kind = str_of(&meta, "kind").unwrap_or("note").to_owned();
    let taint = match str_of(&meta, "taint") {
        Some("own") => Taint::Own,
        Some(_) => Taint::External,
        None => Kind::parse(&kind).map_or(Taint::External, |k| Taint::of(k, &id)),
    };
    let date = ["date", "started_at", "captured_at", "added"]
        .iter()
        .find_map(|k| str_of(&meta, k))
        .map(|d| d.get(..10).unwrap_or(d).to_owned())
        .unwrap_or_default();
    let mut text = String::new();
    for (k, head) in [
        ("abstract", "Abstract"),
        ("tldr", "TL;DR"),
        ("summary", "Summary"),
    ] {
        if let Some(s) = str_of(&meta, k) {
            text.push_str(&format!("## {head}\n\n{s}\n\n"));
        }
    }
    if let Some(html) = vault.read(&rel.join("index.html"))? {
        text.push_str(&chunk::html_text(&html));
    } else if let Some(md) = vault.read(&rel.join("text.md"))? {
        text.push_str(&md);
    }
    let id = id.to_string();
    let mut links = Vec::new();
    if let Some(Value::Object(l)) = meta.get("links") {
        links.extend(
            l.iter()
                .filter_map(|(rel, v)| Some((v.as_str()?.to_owned(), rel.clone()))),
        );
    }
    if let Some(Value::Array(edges)) = meta.get("lineage") {
        for e in edges {
            if str_of(e, "src") == Some(id.as_str())
                && let (Some(dst), Some(rel)) = (str_of(e, "dst"), str_of(e, "rel"))
            {
                links.push((dst.to_owned(), rel.to_owned()));
            }
        }
    }
    if let Some(Value::Array(a)) = meta.get("attendees") {
        links.extend(
            a.iter()
                .filter_map(|s| Some((s.as_str()?.to_owned(), "attendee".to_owned()))),
        );
    }
    if let Some(p) = str_of(&meta, "project") {
        links.push((p.to_owned(), "project".to_owned()));
    }
    Ok(Source {
        title: str_of(&meta, "title").unwrap_or(&id).to_owned(),
        id,
        kind,
        date,
        meta,
        taint,
        text,
        links,
    })
}

fn page_source(vault: &Vault, slug: &str) -> Result<Source> {
    let path = Path::new("pages").join(format!("{slug}.md"));
    let raw = vault.read(&path)?.unwrap_or_default();
    let (yaml, body) = page::split(&raw).ok_or_else(|| Error::Invalid {
        path: path.clone(),
        why: "no frontmatter".to_owned(),
    })?;
    let fm = page::parse_yaml(yaml).map_err(|(line, why)| Error::Frontmatter {
        path,
        line: line + 1,
        why,
    })?;
    let fm = Value::Object(fm);
    let mut links: Vec<(String, String)> = Vec::new();
    if let Some(Value::Array(ls)) = fm.get("links") {
        for l in ls {
            if let Some((rel, Value::String(dst))) = l.as_object().and_then(|o| o.iter().next()) {
                links.push((dst.clone(), rel.clone()));
            }
        }
    }
    if let Some(Value::Array(src)) = fm.get("sources") {
        links.extend(
            src.iter()
                .filter_map(|s| Some((s.as_str()?.to_owned(), "cites".to_owned()))),
        );
    }
    links.extend(
        wikilinks(body)
            .into_iter()
            .map(|w| (w, "mentions".to_owned())),
    );
    Ok(Source {
        id: slug.to_owned(),
        kind: str_of(&fm, "kind").unwrap_or("page").to_owned(),
        title: str_of(&fm, "title").unwrap_or(slug).to_owned(),
        date: str_of(&fm, "updated")
            .map(|d| d.get(..10).unwrap_or(d).to_owned())
            .unwrap_or_default(),
        taint: Taint::Own,
        text: body.to_owned(),
        meta: fm,
        links,
    })
}

/// `[[target]]`, `[[target|label]]`, `[[target#span]]`, as targets, in order, once each.
fn wikilinks(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("[[") {
        rest = rest.get(i + 2..).unwrap_or_default();
        let Some(end) = rest.find("]]") else { break };
        let inner = rest.get(..end).unwrap_or_default();
        let target = inner.split(['|', '#']).next().unwrap_or_default().trim();
        if !target.is_empty() && !out.iter().any(|t| t == target) {
            out.push(target.to_owned());
        }
        rest = rest.get(end + 2..).unwrap_or_default();
    }
    out
}

/// One source per C3 block: `log:<at>`, with `-2`, `-3`... for blocks at the same second.
fn log_sources(text: &str) -> Vec<Source> {
    let mut out: Vec<Source> = Vec::new();
    let mut ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut cur: Option<(Entry, Vec<&str>)> = None;
    let mut finish = |cur: Option<(Entry, Vec<&str>)>, out: &mut Vec<Source>| {
        let Some((e, body)) = cur else { return };
        let n = ids.entry(e.at.clone()).or_insert(0);
        *n += 1;
        let id = if *n == 1 {
            format!("log:{}", e.at)
        } else {
            format!("log:{}-{n}", e.at)
        };
        let body = body.join("\n").trim().to_owned();
        out.push(Source {
            id,
            kind: "log".to_owned(),
            date: e.at.get(..10).unwrap_or(&e.at).to_owned(),
            meta: json!({"at": e.at, "tz": e.tz, "place": e.place, "source": e.source}),
            taint: Taint::Own,
            text: if body.is_empty() {
                e.title.clone()
            } else {
                body
            },
            title: e.title,
            links: Vec::new(),
        });
    };
    for line in text.lines() {
        if let Some(e) = Entry::parse_heading(line) {
            finish(cur.take(), &mut out);
            cur = Some((e, Vec::new()));
        } else if let Some((_, body)) = &mut cur {
            body.push(line);
        }
    }
    finish(cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikilinks_are_targets() {
        assert_eq!(wikilinks("a [[x]] b [[y|Y]] [[x#2]] [[ ]]"), ["x", "y"]);
    }

    #[test]
    fn log_blocks_are_sources() {
        let t = "## 2026-10-27T10:00:00+01:00 · Europe/Paris · - · cli · One\n\nbody\n\n## 2026-10-27T10:00:00+01:00 · Europe/Paris · - · cli · Two\n";
        let s = log_sources(t);
        let ids: Vec<_> = s.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "log:2026-10-27T10:00:00+01:00",
                "log:2026-10-27T10:00:00+01:00-2"
            ]
        );
        assert_eq!(s.first().unwrap().text, "body");
        assert_eq!(s.get(1).unwrap().text, "Two");
    }
}
