//! `brain migrate-paperdb` (B10): the paperdb library, the drover journal and the
//! readings become vault docs, log blocks and labels. Sources are only read. Idempotent:
//! a doc, block or label already in the vault is left alone, so a second run writes nothing;
//! a source that changed since is reported, not applied.
//!
//! One bad entry, row or file is a failure in the report, never the end of the run.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::ops::Range;
use std::path::Path;
use std::process::Command;

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contract::Kind;
use crate::doc::{Artifact, Card, Doc, Paper, PaperLinks, Tag, Taint};
use crate::error::{Error, Result, io};
use crate::id::{ArxivId, DocId, Slug};
use crate::ingest::Slot;
use crate::label::{self, Label, Verdict};
use crate::lineage::{self, Answer};
use crate::log::{self, Entry};
use crate::time::Now;
use crate::vault::{Vault, read_json};
use crate::{chunk, hal, ingest};

/// paperdb and drover stored days, not times: each becomes noon of that day in the
/// current zone, which keeps the day whatever zone it is later read in.
const NOON: &str = "12:00";
/// Papers with Code failures in a row after which it counts as down for this run: one
/// slow reply must not cost every paper after it its lineage.
const PWC_GIVE_UP: u32 = 3;
const DATE_FORMAT: &str = "+%Y-%m-%dT%H:%M:%S%z";

#[derive(Clone, Copy, Debug)]
pub struct Sources<'a> {
    /// The paperdb library: `papers/<id>/`, `.cache/pdf/`, `inbox.jsonl`.
    pub papers: &'a Path,
    /// drover's `journal.db`.
    pub journal: Option<&'a Path>,
    /// A directory of pdf, epub and html books.
    pub readings: Option<&'a Path>,
}

/// The network the migration reads: Papers with Code for arXiv papers, HAL for HAL deposits.
#[derive(Clone, Copy)]
pub struct Lookups<'a> {
    pub pwc: lineage::Lookup<'a>,
    pub hal: hal::Lookup<'a>,
}

/// What this run wrote; a second run reports zeros.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// arXiv papers, and HAL deposits whose record gives authors and a date.
    pub papers: usize,
    /// Other non-arXiv entries: dated by when they entered paperdb.
    pub blogs: usize,
    pub pdfs: usize,
    /// Papers PwC sent lineage for; the rest wait for `reindex`.
    pub lineage: usize,
    pub journal: usize,
    pub books: usize,
    /// Html readings: pages, not books, kept as they are.
    pub artifacts: usize,
    pub skips: usize,
    /// Docs already in the vault, left alone.
    pub existing: usize,
    /// Every book or artifact, migrated now or before, with the required metadata it still lacks.
    pub missing: Vec<(DocId, Vec<&'static str>)>,
    /// Data not carried over, or a source that changed since it was migrated.
    pub warnings: Vec<String>,
    /// Entries, rows or files not migrated at all.
    pub failed: Vec<String>,
}

/// Paper PDFs go to `pdfs` as `<doc dir>.pdf`, not to the vault: arXiv can serve them again.
pub fn run(vault: &Vault, src: &Sources<'_>, pdfs: &Path, now: &Now, net: Lookups<'_>) -> Report {
    let mut r = Report::default();
    let mut pwc_misses = 0;
    match names(&src.papers.join("papers")) {
        Ok(names) => {
            for name in names {
                if let Err(e) = entry(
                    vault,
                    src.papers,
                    pdfs,
                    &name,
                    now,
                    net,
                    &mut pwc_misses,
                    &mut r,
                ) {
                    r.failed.push(format!("papers/{name}: {e}"));
                }
            }
        }
        Err(e) => r.failed.push(e.to_string()),
    }
    if let Some(db) = src.journal
        && let Err(e) = journal(vault, db, &now.tz, &mut r)
    {
        r.failed.push(format!("journal: {e}"));
    }
    if let Some(dir) = src.readings {
        readings(vault, dir, now, &mut r);
    }
    let inbox = src.papers.join("inbox.jsonl");
    if inbox.is_file()
        && let Err(e) = skips(vault, &inbox, now, &mut r)
    {
        r.failed.push(format!("inbox.jsonl: {e}"));
    }
    r
}

/// `papers/<id>/paper.json` in paperdb.
#[derive(Deserialize)]
struct Old {
    source: OldSource,
    title: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    published: String,
    #[serde(default, rename = "abstract")]
    abstract_: String,
    /// `starred` is already one of them.
    #[serde(default)]
    tags: Vec<Tag>,
    #[serde(default)]
    note: String,
    #[serde(default)]
    links: Vec<String>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    keywords: Vec<String>,
    added: String,
    #[serde(default)]
    card: Option<Card>,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum OldSource {
    Arxiv(ArxivId),
    /// `slug` was paperdb's id; the vault derives its own from `url`.
    Web {
        url: String,
        pdf: Option<String>,
    },
}

/// A non-arXiv entry with no publication date: a blog post, dated when it entered paperdb.
#[derive(Serialize)]
struct Blog {
    title: String,
    url: String,
    site: String,
    date: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    authors: Vec<String>,
    #[serde(rename = "abstract", skip_serializing_if = "String::is_empty")]
    abstract_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    card: Option<Card>,
}

/// Fields that come from the source alone: what a later run compares to spot a changed source.
const FROM_SOURCE: &[&str] = &[
    "title", "tags", "why", "abstract", "summary", "keywords", "links", "card", "url",
];

#[expect(
    clippy::too_many_arguments,
    reason = "one call site; a struct for it would only rename the arguments"
)]
fn entry(
    vault: &Vault,
    root: &Path,
    pdfs: &Path,
    name: &str,
    now: &Now,
    net: Lookups<'_>,
    pwc_misses: &mut u32,
    r: &mut Report,
) -> Result<()> {
    let dir = root.join("papers").join(name);
    if !dir.is_dir() {
        r.warnings
            .push(format!("papers/{name}: not an entry directory; skipped"));
        return Ok(());
    }
    let old: Old = read_json(&dir.join("paper.json"))?;
    let id = match &old.source {
        OldSource::Arxiv(aid) => DocId::Arxiv(aid.clone()),
        OldSource::Web { url, .. } => ingest::web_slot(vault, url)?.id(),
    };
    let text = dir.join("paper.md");
    let pdf = root.join(".cache/pdf").join(format!("{name}.pdf"));
    let added = noon(&now.tz, &old.added)?;
    let doc_dir = Vault::doc_dir(&id);
    let cached = pdfs.join(format!("{}.pdf", id.dir()));
    let mut doc = convert(&id, &old, added)?;
    if vault.has_doc(&id) {
        r.existing += 1;
        let mut changed = changed_fields(&vault.path(&doc_dir.join("meta.json")), &doc)?;
        for (from, ours, what) in [
            (&text, vault.path(&doc_dir.join("text.md")), "text.md"),
            (&pdf, cached.clone(), "pdf"),
        ] {
            if from.is_file() && fs::read(from).ok() != fs::read(&ours).ok() {
                changed.push(what.to_owned());
            }
        }
        if !changed.is_empty() {
            r.warnings.push(format!(
                "{id}: papers/{name} changed since it was migrated ({}); not updated",
                changed.join(", ")
            ));
        }
        return Ok(());
    }
    for url in classify(&old.links).1 {
        r.warnings.push(format!(
            "{id}: second link of its kind not carried over: {url}"
        ));
    }
    for key in old.unknown.keys() {
        r.warnings
            .push(format!("{id}: paper.json field `{key}` not carried over"));
    }
    if let OldSource::Web { url, pdf: Some(p) } = &old.source
        && p != url
    {
        r.warnings
            .push(format!("{id}: its PDF link {p} is not carried over"));
    }
    match &old.source {
        OldSource::Arxiv(aid) => {
            if *pwc_misses < PWC_GIVE_UP {
                match (net.pwc)(aid) {
                    Ok(found) => {
                        *pwc_misses = 0;
                        Answer::of(&id, found).fill(&mut doc.body)?;
                        r.lineage += usize::from(doc.body.get("lineage").is_some());
                    }
                    Err(e) => {
                        *pwc_misses += 1;
                        let then = if *pwc_misses == PWC_GIVE_UP {
                            "PwC looks down; reindex will retry lineage for the rest"
                        } else {
                            "reindex will retry its lineage"
                        };
                        r.warnings
                            .push(format!("{id}: papers with code: {e}; {then}"));
                    }
                }
            }
            r.papers += 1;
        }
        OldSource::Web { url, .. } => match hal::id_of(url).map(|h| (net.hal)(&h)) {
            Some(Ok(Some(rec))) if !rec.authors.is_empty() && rec.date.is_some() => {
                doc.kind = Kind::PAPER;
                doc.body = hal_paper(&old, rec)?;
                r.papers += 1;
            }
            Some(Err(e)) => {
                r.warnings
                    .push(format!("{id}: HAL: {e}; migrated as a blog post"));
                r.blogs += 1;
            }
            _ => r.blogs += 1,
        },
    }
    if doc.kind == Kind::BLOG && !old.name.is_empty() && !in_title(&old.name, &old.title) {
        r.warnings.push(format!(
            "{id}: name {:?} not carried over (a blog has no keywords)",
            old.name
        ));
    }
    // Text and PDF before meta.json: a doc counts as migrated once its meta exists.
    if text.is_file() {
        vault.copy(&text, &doc_dir.join("text.md"))?;
    }
    if pdf.is_file() {
        cache_copy(&pdf, &cached)?;
        r.pdfs += 1;
    }
    vault.write_json(&doc_dir.join("meta.json"), &doc)
}

/// Copies through a temp file, so a cut copy never passes for the PDF.
fn cache_copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(dir) = to.parent() {
        io(fs::create_dir_all(dir), dir)?;
    }
    let tmp = to.with_extension("pdf.tmp");
    let copied = io(fs::copy(from, &tmp), from).and_then(|_| io(fs::rename(&tmp, to), to));
    if copied.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    copied
}

/// The doc paperdb's entry becomes before any network: an arXiv paper, or a blog post.
fn convert(id: &DocId, old: &Old, added: String) -> Result<Doc<Value>> {
    let (kind, source, body) = match &old.source {
        OldSource::Arxiv(_) => (Kind::PAPER, None, to_value(arxiv_paper(old))?),
        OldSource::Web { url, .. } => {
            let host = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
            let host = host.split(['/', '?', '#']).next().unwrap_or(host);
            let blog = Blog {
                title: old.title.clone(),
                url: url.clone(),
                site: host.strip_prefix("www.").unwrap_or(host).to_owned(),
                date: old.added.clone(),
                authors: old.authors.clone(),
                abstract_: old.abstract_.clone(),
                card: old.card.clone(),
            };
            (Kind::BLOG, Some(url.clone()), to_value(blog)?)
        }
    };
    Ok(Doc {
        taint: Taint::of(kind, id),
        id: id.clone(),
        kind,
        added,
        tags: old.tags.clone(),
        why: Some(old.note.clone()).filter(|n| !n.is_empty()),
        source,
        body,
    })
}

fn arxiv_paper(old: &Old) -> Paper {
    let (links, _) = classify(&old.links);
    Paper {
        title: old.title.clone(),
        authors: old.authors.clone(),
        date: Some(old.published.clone()).filter(|d| !d.is_empty()),
        links,
        abstract_: old.abstract_.clone(),
        summary: Some(old.summary.clone()).filter(|s| !s.is_empty()),
        keywords: keywords(old),
        card: old.card.clone(),
        ..Paper::default()
    }
}

/// A HAL deposit: authors, date and venue from HAL, the rest from paperdb.
fn hal_paper(old: &Old, rec: hal::Record) -> Result<Value> {
    let OldSource::Web { url, .. } = &old.source else {
        return Err(Error::BadRef(old.title.clone()));
    };
    let mut links = classify(&old.links).0;
    links.project = links.project.or_else(|| Some(url.clone()));
    to_value(Paper {
        title: old.title.clone(),
        authors: rec.authors,
        date: rec.date,
        venue: rec.venue,
        links,
        abstract_: old.abstract_.clone(),
        summary: Some(old.summary.clone()).filter(|s| !s.is_empty()),
        keywords: keywords(old),
        card: old.card.clone(),
        ..Paper::default()
    })
}

/// paperdb's short name (π0, DP3) leads the keywords, unless the title already says it.
fn keywords(old: &Old) -> Vec<String> {
    let mut k = old.keywords.clone();
    if !old.name.is_empty() && !in_title(&old.name, &old.title) && !k.contains(&old.name) {
        k.insert(0, old.name.clone());
    }
    k
}

fn in_title(name: &str, title: &str) -> bool {
    title.to_lowercase().contains(&name.to_lowercase())
}

fn to_value<T: Serialize>(v: T) -> Result<Value> {
    serde_json::to_value(v).map_err(|source| Error::Decode {
        what: "doc",
        source,
    })
}

/// The [`FROM_SOURCE`] fields where the stored meta and `doc` disagree.
fn changed_fields(meta: &Path, doc: &Doc<Value>) -> Result<Vec<String>> {
    let stored: Value = read_json(meta)?;
    let fresh = to_value(doc)?;
    // A HAL deposit became a paper after its record came in: its URL moved into links.
    let rekinded = stored.get("kind") != fresh.get("kind");
    Ok(FROM_SOURCE
        .iter()
        .filter(|k| !(rekinded && matches!(**k, "url" | "links")))
        .filter(|k| stored.get(**k) != fresh.get(**k))
        .map(|k| (*k).to_owned())
        .collect())
}

/// paperdb's `links[]` by host. A second link for a slot is returned, not stored.
fn classify(urls: &[String]) -> (PaperLinks, Vec<String>) {
    let mut links = PaperLinks::default();
    let mut extra = Vec::new();
    for url in urls {
        let host = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
        let host = host.split(['/', '?', '#']).next().unwrap_or(host);
        let host = host.strip_prefix("www.").unwrap_or(host);
        let slot = match host {
            "github.com" | "gitlab.com" | "bitbucket.org" => &mut links.code,
            "huggingface.co" | "hf.co" => &mut links.hf,
            "openreview.net" => &mut links.openreview,
            "youtube.com" | "youtu.be" | "m.youtube.com" | "vimeo.com" => &mut links.video,
            _ => &mut links.project,
        };
        if slot.is_none() {
            *slot = Some(url.clone());
        } else {
            extra.push(url.clone());
        }
    }
    (links, extra)
}

/// drover's journal, an FTS5 table `journal(day, machine, repo, branch, text)`, opened
/// read-only. The machine is dropped: no hostname in the vault (B16). A row is in the
/// vault when its whole block (heading and body) is, as many times as the journal has it.
fn journal(vault: &Vault, db: &Path, tz: &str, r: &mut Report) -> Result<()> {
    let mut uri = String::from("file:");
    for c in db.to_string_lossy().chars() {
        match c {
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            c => uri.push(c),
        }
    }
    uri.push_str("?mode=ro");
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let mut stmt =
        conn.prepare("SELECT rowid, day, repo, branch, text FROM journal ORDER BY rowid")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let source = Slug::parse("journal")?;
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (rowid, day, repo, branch, text) in rows {
        let mut block = || -> Result<Option<Entry>> {
            let at = Now {
                at: noon(tz, &day)?,
                tz: tz.to_owned(),
                place: None,
            };
            let title = format!("{repo} {branch}");
            let title = if title.trim().is_empty() {
                "journal"
            } else {
                title.trim()
            };
            let entry = Entry::new(at, source.clone(), &format!("{title}\n{text}"))?;
            let key = (entry.heading(), entry.body.clone());
            let wanted = seen.entry(key.clone()).or_default();
            *wanted += 1;
            let have = blocks(&vault.read(&entry.file())?.unwrap_or_default())
                .filter(|b| *b == key)
                .count();
            Ok((have < *wanted).then_some(entry))
        };
        match block() {
            Ok(Some(entry)) => {
                log::append(vault, &entry)?;
                r.journal += 1;
            }
            Ok(None) => {}
            Err(e) => r.failed.push(format!("journal row {rowid}: {e}")),
        }
    }
    Ok(())
}

/// A log file's blocks as (heading, trimmed body).
fn blocks(file: &str) -> impl Iterator<Item = (String, String)> + '_ {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in file.lines() {
        if log::is_heading(line) {
            out.push((line.to_owned(), String::new()));
        } else if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out.into_iter().map(|(h, b)| (h, b.trim().to_owned()))
}

#[derive(Default, Serialize)]
struct Book {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    links: Option<BookLinks>,
}

#[derive(Serialize)]
struct BookLinks {
    home: String,
}

fn readings(vault: &Vault, dir: &Path, now: &Now, r: &mut Report) {
    let names = match names(dir) {
        Ok(n) => n,
        Err(e) => return r.failed.push(e.to_string()),
    };
    // Readings already in the vault by file name: one now under another id changed.
    let mut migrated: BTreeMap<String, DocId> = BTreeMap::new();
    for d in names_of(&vault.path(Path::new("docs"))) {
        if let Ok(id @ (DocId::File(_) | DocId::Artifact(_))) = DocId::from_dir(&d)
            && let Ok(m) = read_json::<Value>(&vault.path(&Vault::doc_dir(&id).join("meta.json")))
            && let Some(src) = m.get("source").and_then(Value::as_str)
        {
            migrated.insert(src.to_owned(), id);
        }
    }
    let mut seen: BTreeMap<DocId, String> = BTreeMap::new();
    for name in names {
        if let Err(e) = book(vault, &dir.join(&name), now, &migrated, &mut seen, r) {
            r.failed.push(format!("{name}: {e}"));
        }
    }
}

/// A reading: kept as `source.<ext>`, its text in `text.md`, its metadata only from the
/// file itself (pdfinfo, the epub's OPF). Nothing is guessed.
fn book(
    vault: &Vault,
    path: &Path,
    now: &Now,
    migrated: &BTreeMap<String, DocId>,
    seen: &mut BTreeMap<DocId, String>,
    r: &mut Report,
) -> Result<()> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !path.is_file() || !matches!(ext.as_str(), "pdf" | "epub" | "html" | "htm") {
        r.warnings
            .push(format!("{name}: not a pdf, epub or html file; skipped"));
        return Ok(());
    }
    if matches!(ext.as_str(), "html" | "htm") {
        return artifact(vault, path, name, now, migrated, r);
    }
    let id = DocId::file(&ingest::sha256(path)?).ok_or_else(|| Error::BadRef(name.clone()))?;
    if let Some(first) = seen.get(&id) {
        r.warnings
            .push(format!("{name}: the same file as {first}, one doc {id}"));
        return Ok(());
    }
    seen.insert(id.clone(), name.clone());
    let dir = Vault::doc_dir(&id);
    if vault.has_doc(&id) {
        r.existing += 1;
    } else if let Some(old) = migrated.get(&name) {
        r.warnings.push(format!(
            "{name}: changed since it was migrated as {old}; not migrated again"
        ));
        return Ok(());
    } else {
        let (body, text) = match ext.as_str() {
            "pdf" => (pdf_meta(path)?, ingest::pdf_to_text(path)),
            _ => epub(path)?,
        };
        vault.copy(path, &dir.join(format!("source.{ext}")))?;
        match text {
            Ok(t) => vault.write(&dir.join("text.md"), &t)?,
            Err(why) => r.warnings.push(format!("{id} ({name}): no text: {why}")),
        }
        let doc = Doc {
            taint: Taint::of(Kind::BOOK, &id),
            id: id.clone(),
            kind: Kind::BOOK,
            added: now.at.clone(),
            tags: Vec::new(),
            why: None,
            source: Some(name),
            body,
        };
        vault.write_json(&dir.join("meta.json"), &doc)?;
        r.books += 1;
    }
    let meta: Value = read_json(&vault.path(&dir.join("meta.json")))?;
    let lacks: Vec<&'static str> = [
        ("title", meta.get("title")),
        ("authors", meta.get("authors")),
        ("year", meta.get("year")),
        ("links.home", meta.get("links").and_then(|l| l.get("home"))),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.is_none().then_some(k))
    .collect();
    if !lacks.is_empty() {
        r.missing.push((id, lacks));
    }
    Ok(())
}

/// An html reading is a page, so an artifact as `brain add` makes one: `artifact:<title
/// slug>`, the page as `index.html`, captured when the file was last written. Its `why`
/// is not in the file, so it is reported missing.
fn artifact(
    vault: &Vault,
    path: &Path,
    name: String,
    now: &Now,
    migrated: &BTreeMap<String, DocId>,
    r: &mut Report,
) -> Result<()> {
    let bytes = io(fs::read(path), path)?;
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let title = known(&ingest::html_title(&html)).unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let slot = ingest::artifact_slot(vault, &title, &name, "index.html", &bytes)?;
    let held = matches!(slot, Slot::Held(_));
    let id = slot.id();
    let dir = Vault::doc_dir(&id);
    let page = dir.join("index.html");
    if held {
        r.existing += 1;
    } else if let Some(old) = migrated.get(&name) {
        r.warnings.push(format!(
            "{name}: changed since it was migrated as {old}; not migrated again"
        ));
        return Ok(());
    } else {
        vault.copy(path, &page)?;
        let doc = Doc {
            taint: Taint::of(Kind::ARTIFACT, &id),
            id: id.clone(),
            kind: Kind::ARTIFACT,
            added: now.at.clone(),
            tags: Vec::new(),
            why: None,
            source: Some(name),
            body: Artifact {
                title,
                captured_at: modified(&now.tz, path)?,
            },
        };
        vault.write_json(&dir.join("meta.json"), &doc)?;
        r.artifacts += 1;
    }
    let meta: Value = read_json(&vault.path(&dir.join("meta.json")))?;
    if meta.get("why").is_none() {
        r.missing.push((id, vec!["why"]));
    }
    Ok(())
}

/// A metadata value that says something. Placeholders count as missing: Calibre's
/// `Unknown`, a PDF author left at the account name `user`, a title that is the
/// layout file's name (`hack2e_03.book`).
fn known(s: &str) -> Option<String> {
    const PLACEHOLDERS: &[&str] = &["unknown", "user", "untitled", "anonymous", "author"];
    const LAYOUT_FILES: &[&str] = &[".book", ".indd", ".fm", ".doc", ".docx", ".tex", ".dvi"];
    let s = ingest::squash(s);
    let lower = s.to_ascii_lowercase();
    let says = !s.is_empty()
        && !PLACEHOLDERS.contains(&lower.as_str())
        && !LAYOUT_FILES.iter().any(|e| lower.ends_with(e));
    says.then_some(s)
}

fn year(s: &str) -> Option<i64> {
    s.get(..4)
        .filter(|y| y.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
}

/// `pdfinfo`'s Title and Author (an `<email>` dropped). No year: CreationDate is when
/// the file was made, not when the book was published.
fn pdf_meta(path: &Path) -> Result<Book> {
    let out = run_cmd(Command::new("pdfinfo").args(["-enc", "UTF-8"]).arg(path))?;
    let field = |key: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(known)
    };
    let author = field("Author").and_then(|a| known(a.split('<').next().unwrap_or(&a)));
    Ok(Book {
        title: field("Title"),
        authors: author.into_iter().collect(),
        year: None,
        links: None,
    })
}

/// The OPF's `dc:` metadata, and the text of the spine's documents in order.
fn epub(path: &Path) -> Result<(Book, Result<String, String>)> {
    let container = unzip(path, "META-INF/container.xml")?;
    let opf_path = start_tags(&container, "rootfile")
        .into_iter()
        .find_map(|t| attr(t, "full-path"))
        .ok_or_else(|| Error::Invalid {
            path: path.to_owned(),
            why: "no rootfile in META-INF/container.xml".to_owned(),
        })?
        .to_owned();
    let opf = unzip(path, &opf_path)?;
    let text_of = |xml: &str| known(&chunk::html_text(xml));
    let book = Book {
        title: elements(&opf, "dc:title").into_iter().find_map(text_of),
        authors: elements(&opf, "dc:creator")
            .into_iter()
            .filter_map(text_of)
            .collect(),
        year: elements(&opf, "dc:date")
            .into_iter()
            .find_map(|d| year(d.trim())),
        links: elements(&opf, "dc:source")
            .into_iter()
            .map(str::trim)
            .find(|s| s.starts_with("http://") || s.starts_with("https://"))
            .map(|home| BookLinks {
                home: home.to_owned(),
            }),
    };
    let base = opf_path.rsplit_once('/').map_or("", |(d, _)| d);
    let items: Vec<(&str, &str)> = start_tags(&opf, "item")
        .into_iter()
        .filter_map(|t| Some((attr(t, "id")?, attr(t, "href")?)))
        .collect();
    let mut text = String::new();
    for idref in start_tags(&opf, "itemref")
        .into_iter()
        .filter_map(|t| attr(t, "idref"))
    {
        let Some((_, href)) = items.iter().find(|(id, _)| *id == idref) else {
            continue;
        };
        let href = href.split('#').next().unwrap_or(href);
        let member = if base.is_empty() {
            href.to_owned()
        } else {
            format!("{base}/{href}")
        };
        match unzip(path, &member) {
            Ok(html) => {
                text.push_str(&chunk::html_text(&html));
                text.push_str("\n\n");
            }
            Err(e) => return Ok((book, Err(e.to_string()))),
        }
    }
    Ok((book, Ok(text)))
}

/// Every `<name ...>` start tag, `<` to `>`, matched without regard to case.
fn start_tags<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    tag_spans(xml, name)
        .into_iter()
        .filter_map(|r| xml.get(r))
        .collect()
}

fn tag_spans(xml: &str, name: &str) -> Vec<Range<usize>> {
    let lower = xml.to_ascii_lowercase();
    let open = format!("<{}", name.to_ascii_lowercase());
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = lower
        .get(from..)
        .and_then(|s| s.find(&open))
        .map(|i| from + i)
    {
        let after = i + open.len();
        from = after;
        if !lower
            .as_bytes()
            .get(after)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/')
        {
            continue;
        }
        let Some(end) = lower.get(after..).and_then(|s| s.find('>')) else {
            break;
        };
        from = after + end + 1;
        out.push(i..from);
    }
    out
}

/// The inner text of every `<name ...>...</name>`.
fn elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let lower = xml.to_ascii_lowercase();
    let close = format!("</{}>", name.to_ascii_lowercase());
    tag_spans(xml, name)
        .into_iter()
        .filter(|r| !xml.get(r.clone()).is_some_and(|t| t.ends_with("/>")))
        .filter_map(|r| {
            let end = lower.get(r.end..)?.find(&close)?;
            xml.get(r.end..r.end + end)
        })
        .collect()
}

/// `name="value"` or `name='value'` inside a start tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let key = format!("{}=", name.to_ascii_lowercase());
    let mut from = 0;
    while let Some(i) = lower.get(from..)?.find(&key).map(|i| from + i) {
        from = i + key.len();
        if !lower
            .as_bytes()
            .get(i.wrapping_sub(1))
            .is_some_and(u8::is_ascii_whitespace)
        {
            continue;
        }
        let quote = *tag.as_bytes().get(from)?;
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        let rest = tag.get(from + 1..)?;
        return rest.find(char::from(quote)).and_then(|e| rest.get(..e));
    }
    None
}

/// One member, named exactly: unzip reads `*`, `?` and `[` in a name as wildcards.
fn unzip(archive: &Path, member: &str) -> Result<String> {
    let mut exact = String::with_capacity(member.len());
    for c in member.chars() {
        match c {
            '*' | '?' | '[' => {
                exact.push('[');
                exact.push(c);
                exact.push(']');
            }
            c => exact.push(c),
        }
    }
    run_cmd(Command::new("unzip").arg("-p").arg(archive).arg(exact))
}

fn run_cmd(cmd: &mut Command) -> Result<String> {
    let name = format!("{cmd:?}");
    let out = cmd.output().map_err(|e| Error::Command {
        cmd: name.clone(),
        detail: e.to_string(),
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(Error::Command {
            cmd: name,
            detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

/// paperdb's `inbox.jsonl`: only skips are kept, as C7 labels; the rest is the feed's.
#[derive(Deserialize)]
struct Candidate {
    id: ArxivId,
    #[serde(default)]
    found: String,
    state: String,
}

fn skips(vault: &Vault, inbox: &Path, now: &Now, r: &mut Report) -> Result<()> {
    let have: BTreeSet<DocId> = vault
        .read(Path::new(label::FILE))?
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Label>(l).ok())
        .filter(|l| l.verdict == Verdict::Skip)
        .map(|l| l.id)
        .collect();
    let raw = io(fs::read_to_string(inbox), inbox)?;
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let c: Candidate = serde_json::from_str(line).map_err(|source| Error::Json {
            path: inbox.to_owned(),
            source,
        })?;
        let id = DocId::Arxiv(c.id);
        if c.state != "skipped" || have.contains(&id) {
            continue;
        }
        let ts = if c.found.is_empty() {
            now.at.clone()
        } else {
            noon(&now.tz, &c.found)?
        };
        label::append(
            vault,
            &Label {
                id,
                verdict: Verdict::Skip,
                ts,
                surface: "paperdb".to_owned(),
            },
        )?;
        r.skips += 1;
    }
    Ok(())
}

/// `<day>T12:00:00±hh:mm` in `tz`. The offset comes from `date`, GNU (`-d`) or BSD
/// (`-j -f`), whichever this host has: reading it from the TZif file would mean parsing
/// the POSIX rule in its footer, which slim zoneinfo files (macOS, recent distros) rely on.
fn noon(tz: &str, day: &str) -> Result<String> {
    let when = format!("{day} {NOON}");
    let gnu = run_cmd(
        Command::new("date")
            .env("TZ", tz)
            .args(["-d", &when, DATE_FORMAT]),
    );
    let s = match gnu {
        Ok(s) => s,
        Err(_) => run_cmd(Command::new("date").env("TZ", tz).args([
            "-j",
            "-f",
            "%Y-%m-%d %H:%M",
            &when,
            DATE_FORMAT,
        ]))?,
    };
    stamp(&s)
        .filter(|t| t.starts_with(day))
        .ok_or_else(|| Error::Command {
            cmd: "date".to_owned(),
            detail: format!("unexpected output {s:?} for day {day:?}"),
        })
}

/// When `path` was last written, as `YYYY-MM-DDTHH:MM:SS±hh:mm` in `tz`. `date -r` reads
/// a file's mtime on GNU and BSD alike.
fn modified(tz: &str, path: &Path) -> Result<String> {
    let s = run_cmd(
        Command::new("date")
            .env("TZ", tz)
            .arg("-r")
            .arg(path)
            .arg(DATE_FORMAT),
    )?;
    stamp(&s).ok_or_else(|| Error::Command {
        cmd: "date".to_owned(),
        detail: format!("unexpected output {s:?} for {}", path.display()),
    })
}

/// `date`'s `…±hhmm` as RFC 3339's `…±hh:mm`.
fn stamp(s: &str) -> Option<String> {
    let s = s.trim();
    let (head, mm) = s.split_at_checked(s.len().checked_sub(2)?)?;
    (head.len() == 22).then(|| format!("{head}:{mm}"))
}

/// Entry names of a directory that may not exist yet; none then.
fn names_of(dir: &Path) -> Vec<String> {
    if dir.is_dir() {
        names(dir).unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// Non-hidden entry names of a directory, sorted.
fn names(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for e in io(fs::read_dir(dir), dir)? {
        let name = io(e, dir)?.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_classified_by_host() {
        let (l, extra) = classify(&[
            "https://github.com/a/b".to_owned(),
            "https://www.youtube.com/watch?v=x".to_owned(),
            "https://openreview.net/forum?id=y".to_owned(),
            "https://huggingface.co/org/model".to_owned(),
            "https://site.github.io/".to_owned(),
            "https://other.example/".to_owned(),
        ]);
        assert_eq!(l.code.as_deref(), Some("https://github.com/a/b"));
        assert_eq!(
            l.video.as_deref(),
            Some("https://www.youtube.com/watch?v=x")
        );
        assert_eq!(
            l.openreview.as_deref(),
            Some("https://openreview.net/forum?id=y")
        );
        assert_eq!(l.hf.as_deref(), Some("https://huggingface.co/org/model"));
        assert_eq!(l.project.as_deref(), Some("https://site.github.io/"));
        assert_eq!(extra, ["https://other.example/"]);
    }

    #[test]
    fn reads_tags_and_attributes() {
        let opf = r#"<metadata><dc:title>A &amp; B</dc:title><dc:creator opf:role="aut">X</dc:creator>
            <dc:creatorx>no</dc:creatorx></metadata><item id='c1' href="ch1.html"/><itemref idref="c1"/>"#;
        assert_eq!(elements(opf, "dc:title"), ["A &amp; B"]);
        assert_eq!(elements(opf, "dc:creator"), ["X"]);
        let item = start_tags(opf, "item");
        assert_eq!(item.len(), 1);
        assert_eq!(attr(item[0], "href"), Some("ch1.html"));
        assert_eq!(attr(item[0], "id"), Some("c1"));
        assert_eq!(attr(r#"<x data-id="no" id="yes">"#, "id"), Some("yes"));
    }

    #[test]
    fn days_become_noon_with_their_offset() {
        assert_eq!(
            noon("America/Los_Angeles", "2026-10-18").unwrap(),
            "2026-10-18T12:00:00-07:00"
        );
        assert_eq!(
            noon("America/Los_Angeles", "2026-12-18").unwrap(),
            "2026-12-18T12:00:00-08:00"
        );
        assert!(noon("UTC", "not a day").is_err());
    }
}
