//! `brain migrate-paperdb` (B10): the paperdb library, the drover journal and the
//! readings become vault docs, log blocks and labels. Sources are only read. Idempotent:
//! a doc, block or label already in the vault is left alone, so a second run writes nothing.

use std::collections::BTreeSet;
use std::fs;
use std::ops::Range;
use std::path::Path;
use std::process::Command;

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contract::Kind;
use crate::doc::{Card, Doc, Fields, Paper, PaperLinks, Tag, Taint};
use crate::error::{Error, Result, io};
use crate::id::{ArxivId, DocId, Slug};
use crate::label::{self, Label, Verdict};
use crate::lineage::{self, Lookup};
use crate::log::{self, Entry};
use crate::time::Now;
use crate::vault::{Vault, read_json};
use crate::{chunk, ingest};

/// paperdb and drover stored days, not times: each becomes noon of that day in the
/// current zone, which keeps the day whatever zone it is later read in.
const NOON: &str = "12:00";

#[derive(Clone, Copy, Debug)]
pub struct Sources<'a> {
    /// The paperdb library: `papers/<id>/`, `.cache/pdf/`, `inbox.jsonl`.
    pub papers: &'a Path,
    /// drover's `journal.db`.
    pub journal: Option<&'a Path>,
    /// A directory of pdf, epub and html books.
    pub readings: Option<&'a Path>,
}

/// What this run wrote; a second run reports zeros.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub papers: usize,
    /// Non-arXiv paperdb entries (company blogs, a HAL paper): no authors or date in
    /// paperdb, so `kind: note` with the URL as `source`.
    pub notes: usize,
    pub pdfs: usize,
    /// Papers PwC answered for; the rest wait for `reindex`.
    pub lineage: usize,
    pub journal: usize,
    pub books: usize,
    pub skips: usize,
    /// Docs already in the vault, left alone.
    pub existing: usize,
    /// Every book, migrated now or before, with the required metadata it still lacks.
    pub missing: Vec<(DocId, Vec<&'static str>)>,
    pub warnings: Vec<String>,
}

pub fn run(vault: &Vault, src: &Sources<'_>, now: &Now, lookup: Lookup<'_>) -> Result<Report> {
    let mut r = Report::default();
    let mut pwc_up = true;
    let papers = src.papers.join("papers");
    for name in names(&papers)? {
        paper(vault, src.papers, &name, now, lookup, &mut pwc_up, &mut r)?;
    }
    if let Some(db) = src.journal {
        journal(vault, db, &now.tz, &mut r)?;
    }
    if let Some(dir) = src.readings {
        for name in names(dir)? {
            book(vault, &dir.join(name), now, &mut r)?;
        }
    }
    let inbox = src.papers.join("inbox.jsonl");
    if inbox.is_file() {
        skips(vault, &inbox, now, &mut r)?;
    }
    Ok(r)
}

/// `papers/<id>/paper.json` in paperdb.
#[derive(Deserialize)]
struct Old {
    source: OldSource,
    title: String,
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
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum OldSource {
    Arxiv(ArxivId),
    Web { url: String },
}

fn paper(
    vault: &Vault,
    root: &Path,
    name: &str,
    now: &Now,
    lookup: Lookup<'_>,
    pwc_up: &mut bool,
    r: &mut Report,
) -> Result<()> {
    let dir = root.join("papers").join(name);
    let old: Old = read_json(&dir.join("paper.json"))?;
    let id = match &old.source {
        OldSource::Arxiv(aid) => DocId::Arxiv(aid.clone()),
        OldSource::Web { url } => DocId::web(url).ok_or_else(|| Error::BadRef(url.clone()))?,
    };
    if vault.has_doc(&id) {
        r.existing += 1;
        return Ok(());
    }
    let doc_dir = Vault::doc_dir(&id);
    let added = noon(&now.tz, &old.added)?;
    let why = Some(old.note.clone()).filter(|n| !n.is_empty());
    let (kind, source) = match &old.source {
        OldSource::Arxiv(_) => (Kind::PAPER, None),
        OldSource::Web { url } => (Kind::NOTE, Some(url.clone())),
    };
    // Text and PDF before meta.json: a doc counts as migrated once its meta exists.
    let text = dir.join("paper.md");
    if text.is_file() {
        vault.copy(&text, &doc_dir.join("text.md"))?;
    }
    let pdf = root.join(".cache/pdf").join(format!("{name}.pdf"));
    if pdf.is_file() {
        vault.copy(&pdf, &doc_dir.join("paper.pdf"))?;
        r.pdfs += 1;
    }
    let body = match &old.source {
        OldSource::Arxiv(aid) => {
            r.papers += 1;
            paper_body(&id, aid, &old, lookup, pwc_up, r)?
        }
        OldSource::Web { .. } => {
            r.notes += 1;
            let mut body = Fields::new();
            body.insert("title".to_owned(), old.title.clone().into());
            Value::Object(body)
        }
    };
    let doc = Doc {
        taint: Taint::of(kind, &id),
        id: id.clone(),
        kind,
        added,
        tags: old.tags,
        why,
        source,
        body,
    };
    vault.write_json(&doc_dir.join("meta.json"), &doc)
}

fn paper_body(
    id: &DocId,
    aid: &ArxivId,
    old: &Old,
    lookup: Lookup<'_>,
    pwc_up: &mut bool,
    r: &mut Report,
) -> Result<Value> {
    let (links, extra) = classify(&old.links);
    r.warnings.extend(
        extra
            .into_iter()
            .map(|u| format!("{id}: second link of its kind dropped: {u}")),
    );
    let mut p = Paper {
        title: old.title.clone(),
        authors: old.authors.clone(),
        date: Some(old.published.clone()).filter(|d| !d.is_empty()),
        links,
        abstract_: old.abstract_.clone(),
        summary: Some(old.summary.clone()).filter(|s| !s.is_empty()),
        keywords: old.keywords.clone(),
        card: old.card.clone(),
        ..Paper::default()
    };
    if *pwc_up {
        match lookup(aid) {
            Ok(found) => {
                let found = found.unwrap_or_default();
                p.lineage = Some(lineage::edges(id, &found));
                p.tldr = found.tldr.filter(|t| !t.is_empty());
                r.lineage += 1;
            }
            // Down: the rest would fail too, and `reindex` retries them all.
            Err(e) => {
                *pwc_up = false;
                r.warnings
                    .push(format!("papers with code: {e}; reindex will retry lineage"));
            }
        }
    }
    serde_json::to_value(p).map_err(|source| Error::Decode {
        what: "paper",
        source,
    })
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
/// read-only. The machine is dropped: no hostname in the vault (B16).
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
    let mut stmt = conn.prepare("SELECT day, repo, branch, text FROM journal ORDER BY rowid")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let source = Slug::parse("journal")?;
    for (day, repo, branch, text) in rows {
        let at = Now {
            at: noon(tz, &day)?,
            tz: tz.to_owned(),
            place: None,
        };
        let entry = Entry::new(at, source.clone(), &format!("{repo} {branch}\n{text}"))?;
        let heading = entry.heading();
        let there = vault
            .read(&entry.file())?
            .is_some_and(|s| s.lines().any(|l| l == heading));
        if !there {
            log::append(vault, &entry)?;
            r.journal += 1;
        }
    }
    Ok(())
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

/// A reading: kept as `source.<ext>`, its text in `text.md`, its metadata only from the
/// file itself (pdfinfo, the epub's OPF, the html head). Nothing is guessed.
fn book(vault: &Vault, path: &Path, now: &Now, r: &mut Report) -> Result<()> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "pdf" | "epub" | "html" | "htm") {
        r.warnings
            .push(format!("{name}: not a pdf, epub or html; skipped"));
        return Ok(());
    }
    let id = DocId::file(&ingest::sha256(path)?).ok_or_else(|| Error::BadRef(name.clone()))?;
    let dir = Vault::doc_dir(&id);
    if vault.has_doc(&id) {
        r.existing += 1;
    } else {
        let (body, text) = match ext.as_str() {
            "pdf" => (pdf_meta(path)?, ingest::pdf_to_text(path)),
            "epub" => epub(path)?,
            _ => {
                let html = String::from_utf8_lossy(&io(fs::read(path), path)?).into_owned();
                (html_meta(&html), Ok(chunk::html_text(&html)))
            }
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

/// A metadata value that says nothing: Calibre writes `Unknown` for no author.
fn known(s: &str) -> Option<String> {
    let s = ingest::squash(s);
    (!s.is_empty() && !s.eq_ignore_ascii_case("unknown")).then_some(s)
}

fn year(s: &str) -> Option<i64> {
    s.get(..4)
        .filter(|y| y.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
}

/// `pdfinfo`'s Title, Author (an `<email>` dropped) and the year of CreationDate.
fn pdf_meta(path: &Path) -> Result<Book> {
    let out = run_cmd(
        Command::new("pdfinfo")
            .args(["-isodates", "-enc", "UTF-8"])
            .arg(path),
    )?;
    let field = |key: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(known)
    };
    let author = field("Author").and_then(|a| known(a.split('<').next().unwrap_or(&a)));
    Ok(Book {
        title: field("Title"),
        authors: author.into_iter().collect(),
        year: field("CreationDate").as_deref().and_then(year),
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
        let member = if base.is_empty() {
            (*href).to_owned()
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

/// `<title>`, `<meta name="author">`, a date meta's year and `<link rel="canonical">`.
fn html_meta(html: &str) -> Book {
    let head = html
        .to_ascii_lowercase()
        .find("</head>")
        .and_then(|i| html.get(..i))
        .unwrap_or(html);
    let metas = start_tags(head, "meta");
    let meta = |names: &[&str]| {
        metas.iter().find_map(|t| {
            let n = attr(t, "name").or_else(|| attr(t, "property"))?;
            names
                .iter()
                .any(|x| n.eq_ignore_ascii_case(x))
                .then(|| attr(t, "content"))?
        })
    };
    Book {
        title: known(&ingest::html_title(head)),
        authors: meta(&["author", "citation_author", "dc.creator"])
            .and_then(known)
            .into_iter()
            .collect(),
        year: meta(&[
            "citation_publication_date",
            "citation_date",
            "dc.date",
            "date",
        ])
        .and_then(year),
        links: start_tags(head, "link")
            .into_iter()
            .filter(|t| attr(t, "rel").is_some_and(|r| r.eq_ignore_ascii_case("canonical")))
            .find_map(|t| attr(t, "href"))
            .filter(|h| h.starts_with("http://") || h.starts_with("https://"))
            .map(|h| BookLinks { home: h.to_owned() }),
    }
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

fn unzip(archive: &Path, member: &str) -> Result<String> {
    run_cmd(Command::new("unzip").arg("-p").arg(archive).arg(member))
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

/// `<day>T12:00:00±hh:mm` in `tz`, by GNU `date -d`.
fn noon(tz: &str, day: &str) -> Result<String> {
    let s = run_cmd(Command::new("date").env("TZ", tz).args([
        "-d",
        &format!("{day} {NOON}"),
        "+%Y-%m-%dT%H:%M:%S%z",
    ]))?;
    let s = s.trim();
    match s.split_at_checked(s.len().saturating_sub(2)) {
        Some((head, mm)) if head.len() == 22 && head.starts_with(day) => Ok(format!("{head}:{mm}")),
        _ => Err(Error::Command {
            cmd: "date".to_owned(),
            detail: format!("unexpected output {s:?} for day {day:?}"),
        }),
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
