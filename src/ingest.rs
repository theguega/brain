//! `brain add` (B2, B13, B14): a source becomes `docs/<id>/`. Idempotent: an id already
//! in the vault is left untouched.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::Value;

use crate::contract::{Kind, Problem, Schema};
use crate::doc::{Artifact, Doc, Fields, Meeting, Paper, PaperLinks, Tag, Taint, Web};
use crate::error::{Error, Result, io};
use crate::id::{ArxivId, DocId};
use crate::time::{self, Now};
use crate::vault::{Vault, read_json};
use crate::{alphaxiv, arxiv, hub, lineage, net, page, pwc};

/// Set by brain, never by a file's frontmatter: the envelope of every doc.
const ENVELOPE: &[&str] = &["id", "kind", "taint", "added", "tags", "why", "source"];

/// Hosts whose URLs name an arXiv paper.
const PAPER_HOSTS: &[&str] = &[
    "arxiv.org",
    "export.arxiv.org",
    "huggingface.co",
    "alphaxiv.org",
];

#[derive(Clone, Debug, Default)]
pub struct AddOpts {
    pub kind: Option<Kind>,
    pub why: Option<String>,
    pub tags: Vec<Tag>,
}

#[derive(Debug)]
pub struct Added {
    pub id: DocId,
    /// `false` when the id was already in the vault and nothing was written.
    pub created: bool,
    /// What the kind's schema still wants.
    pub missing: Vec<Problem>,
    /// `docs/<dir>/meta.json`, relative to the vault.
    pub meta: PathBuf,
    /// A later add fetched the text an earlier one could not.
    pub text_added: bool,
    /// Enrichment that failed without failing the add.
    pub warnings: Vec<String>,
}

/// The id `add` would give an id or URL, without fetching anything.
pub fn resolve(input: &str) -> Result<DocId> {
    match web_url(input) {
        Some(url) => DocId::web(url).ok_or_else(|| Error::BadRef(input.to_owned())),
        None => DocId::parse(input),
    }
}

/// `Some` for an http(s) URL that does not name an arXiv paper.
fn web_url(input: &str) -> Option<&str> {
    let rest = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!PAPER_HOSTS.contains(&host)).then_some(input)
}

pub fn add(vault: &Vault, input: &str, opts: &AddOpts) -> Result<Added> {
    let path = Path::new(input);
    if path.is_file() {
        return add_file(vault, path, opts);
    }
    if let Some(url) = web_url(input) {
        return add_web(vault, url, opts);
    }
    let id = DocId::parse(input)?;
    match id {
        DocId::Arxiv(aid) if vault.has_doc(&id) => retry_text(vault, &aid),
        _ if vault.has_doc(&id) => existing(vault, id),
        DocId::Arxiv(aid) => add_paper(vault, &aid, opts),
        other => Err(Error::Usage(format!(
            "{other} is not in the vault; add it by its URL or file"
        ))),
    }
}

/// A paper already in the vault: fetch its text if an earlier add could not.
fn retry_text(vault: &Vault, aid: &ArxivId) -> Result<Added> {
    let id = DocId::Arxiv(aid.clone());
    let text = Vault::doc_dir(&id).join("text.md");
    let mut added = existing(vault, id)?;
    if vault.path(&text).is_file() {
        return Ok(added);
    }
    match paper_text(aid) {
        Ok(Some(t)) => {
            vault.write(&text, &t)?;
            added.text_added = true;
        }
        Ok(None) => {}
        Err(e) => added.warnings.push(format!("no text: {e}")),
    }
    Ok(added)
}

fn add_paper(vault: &Vault, aid: &ArxivId, opts: &AddOpts) -> Result<Added> {
    if opts.kind.is_some_and(|k| k != Kind::PAPER) {
        return Err(Error::Usage("an arXiv id adds as --kind paper".to_owned()));
    }
    let id = DocId::Arxiv(aid.clone());
    let mut warnings = Vec::new();
    let entry = arxiv::fetch(std::slice::from_ref(aid))?.into_iter().next();
    // Everything past arXiv is enrichment: an outage must not block the add.
    let hub = match hub::info(aid) {
        Ok(h) => h,
        Err(e) if entry.is_some() => {
            warnings.push(format!("no Hugging Face extras: {e}"));
            None
        }
        Err(e) => return Err(e),
    };
    let ax = alphaxiv::info(aid).unwrap_or_else(|e| {
        warnings.push(format!("no alphaXiv extras: {e}"));
        None
    });
    // PwC down leaves `lineage` unset, not an error: the next reindex retries it.
    let pwc = pwc::paper(aid).ok();

    let (comment_code, comment_project) = entry.as_ref().map_or((None, None), |e| {
        let (code, other): (Vec<&str>, Vec<&str>) =
            e.comment_urls().partition(|u| u.contains("://github.com/"));
        (
            code.first().map(|u| (*u).to_owned()),
            other.first().map(|u| (*u).to_owned()),
        )
    });
    let mut p = match (entry, &hub) {
        (Some(e), _) => Paper {
            title: e.title,
            authors: e.authors,
            date: Some(e.published).filter(|d| !d.is_empty()),
            abstract_: e.abstract_,
            ..Paper::default()
        },
        (None, Some(h)) => Paper {
            title: squash(&h.title),
            authors: h.authors.iter().map(|a| a.name.clone()).collect(),
            date: h.published_at.get(..10).map(str::to_owned),
            abstract_: squash(&h.summary),
            ..Paper::default()
        },
        (None, None) => return Err(Error::Unknown(id.to_string())),
    };
    let ax = ax.unwrap_or_default();
    if let Some(h) = hub {
        p.links = PaperLinks {
            code: h.github_repo.filter(|u| !u.is_empty()),
            project: h.project_page.filter(|u| !u.is_empty()),
            hf: Some(format!("https://huggingface.co/papers/{aid}")),
            ..PaperLinks::default()
        };
        p.summary = Some(h.ai_summary).filter(|s| !s.is_empty());
        p.keywords = h.ai_keywords;
    }
    p.links.code = p.links.code.or(ax.code).or(comment_code);
    p.links.project = p.links.project.or(comment_project);
    p.summary = p.summary.or(ax.summary);
    if let Some(pwc) = pwc {
        let pwc = pwc.unwrap_or_default();
        p.lineage = Some(lineage::edges(&id, &pwc));
        p.venue = pwc.conference_name.filter(|s| !s.is_empty());
        p.tldr = pwc.tldr.filter(|s| !s.is_empty());
    }

    let text = paper_text(aid).unwrap_or_else(|e| {
        warnings.push(format!("no text: {e}"));
        None
    });
    let now = time::now()?;
    let doc = envelope(id, Kind::PAPER, opts, &now, None, p);
    let mut added = write(vault, &doc, text.as_deref().map(|t| ("text.md", t)))?;
    added.warnings = warnings;
    Ok(added)
}

/// Hub markdown when the Hub has it (no PDF needed), else the PDF through the converter chain.
fn paper_text(aid: &ArxivId) -> Result<Option<String>> {
    if let Some(md) = hub::markdown(aid)? {
        return Ok(Some(md));
    }
    let pdf = std::env::temp_dir().join(format!("brain-{}-{aid}.pdf", std::process::id()));
    net::download(&format!("https://arxiv.org/pdf/{aid}"), &pdf)?;
    let text = pdf_to_text(&pdf);
    let _ = std::fs::remove_file(&pdf);
    text.map(Some).map_err(|why| Error::NoText {
        id: format!("arxiv:{aid}"),
        why,
    })
}

fn add_web(vault: &Vault, url: &str, opts: &AddOpts) -> Result<Added> {
    let kind = opts.kind.unwrap_or(Kind::BLOG);
    if kind != Kind::BLOG && kind != Kind::NEWS {
        return Err(Error::Usage(
            "a web URL adds as --kind blog or news".to_owned(),
        ));
    }
    let id = DocId::web(url).ok_or_else(|| Error::BadRef(url.to_owned()))?;
    if vault.has_doc(&id) {
        return existing(vault, id);
    }
    let page = read_page(url).ok_or_else(|| Error::NoText {
        id: id.to_string(),
        why: "the page reader was blocked".to_owned(),
    })?;
    let header = |key: &str| {
        page.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let host = url.split_once("://").map_or(url, |(_, r)| r);
    let host = host.split(['/', '?', '#']).next().unwrap_or(host);
    let body = Web {
        title: header("Title:").unwrap_or_else(|| host.to_owned()),
        url: url.to_owned(),
        site: host.strip_prefix("www.").unwrap_or(host).to_owned(),
        date: header("Published Time:").and_then(|d| d.get(..10).map(str::to_owned)),
    };
    let text = page
        .split_once("Markdown Content:\n")
        .map_or(page.as_str(), |(_, t)| t);
    let now = time::now()?;
    let doc = envelope(id, kind, opts, &now, Some(url.to_owned()), body);
    write(vault, &doc, Some(("text.md", text)))
}

fn add_file(vault: &Vault, path: &Path, opts: &AddOpts) -> Result<Added> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let html = matches!(ext.as_str(), "html" | "htm");
    let kind = opts
        .kind
        .unwrap_or(if html { Kind::ARTIFACT } else { Kind::NOTE });
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bytes = io(std::fs::read(path), path)?;

    let (text, mut fields) = if ext == "pdf" {
        let text = pdf_to_text(path).map_err(|why| Error::NoText {
            id: name.clone(),
            why,
        })?;
        (text, Fields::new())
    } else {
        let s = String::from_utf8_lossy(&bytes).into_owned();
        match page::split(&s) {
            Some((yaml, body)) if !html => {
                let fm = page::parse_yaml(yaml).map_err(|(line, why)| Error::Frontmatter {
                    path: name.clone().into(),
                    line: line + 1,
                    why,
                })?;
                (body.to_owned(), fm)
            }
            _ => (s, Fields::new()),
        }
    };
    if let Some(key) = ENVELOPE.iter().find(|k| fields.contains_key(**k)) {
        return Err(Error::Invalid {
            path: name.into(),
            why: format!("frontmatter sets `{key}`, which brain sets (use --kind, --tag, --why)"),
        });
    }
    let title = match fields.remove("title") {
        Some(Value::String(t)) if !t.is_empty() => t,
        _ if html => html_title(&text),
        _ => text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(str::trim)
            .map(str::to_owned)
            .unwrap_or_default(),
    };
    let title = if title.is_empty() { stem } else { title };
    let now = time::now()?;
    let text_file = if html { "index.html" } else { "text.md" };
    let take = |fields: &mut Fields, key: &str| match fields.remove(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s),
        _ => None,
    };

    let id = match kind {
        Kind::ARTIFACT => DocId::artifact(&title),
        Kind::MEETING => {
            let started = fields
                .get("started_at")
                .and_then(Value::as_str)
                .unwrap_or(&now.at);
            DocId::meeting(started.get(..10).unwrap_or(now.day()), &title)
        }
        _ => DocId::file(&sha256(path)?),
    }
    .ok_or_else(|| Error::BadRef(name.clone()))?;
    if vault.has_doc(&id) {
        return existing(vault, id);
    }
    let source = Some(name.clone());
    match kind {
        Kind::ARTIFACT => {
            let body = Artifact {
                title,
                captured_at: now.at.clone(),
            };
            write(
                vault,
                &envelope(id, kind, opts, &now, source, body),
                Some((text_file, &text)),
            )
        }
        Kind::MEETING => {
            let attendees = match fields.remove("attendees") {
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim_start_matches("[[").trim_end_matches("]]").to_owned())
                    .collect(),
                _ => Vec::new(),
            };
            let body = Meeting {
                started_at: take(&mut fields, "started_at").unwrap_or_else(|| now.at.clone()),
                tz: take(&mut fields, "tz").unwrap_or_else(|| now.tz.clone()),
                place: take(&mut fields, "place").or_else(|| now.place.clone()),
                project: take(&mut fields, "project")
                    .map(|p| p.trim_start_matches("[[").trim_end_matches("]]").to_owned()),
                title: Some(title),
                attendees,
            };
            if let Some(key) = fields.keys().next() {
                return Err(Error::Invalid {
                    path: name.into(),
                    why: format!("frontmatter `{key}` is not meeting metadata"),
                });
            }
            write(
                vault,
                &envelope(id, kind, opts, &now, source, body),
                Some((text_file, &text)),
            )
        }
        _ => {
            fields.insert("title".to_owned(), Value::String(title));
            write(
                vault,
                &envelope(id, kind, opts, &now, source, fields),
                Some((text_file, &text)),
            )
        }
    }
}

fn envelope<B>(
    id: DocId,
    kind: Kind,
    opts: &AddOpts,
    now: &Now,
    source: Option<String>,
    body: B,
) -> Doc<B> {
    Doc {
        taint: Taint::of(kind, &id),
        id,
        kind,
        added: now.at.clone(),
        tags: opts.tags.clone(),
        why: opts.why.clone(),
        source,
        body,
    }
}

/// Writes `meta.json` and the text, then reports what the schema still wants.
fn write<B: Serialize>(vault: &Vault, doc: &Doc<B>, text: Option<(&str, &str)>) -> Result<Added> {
    let dir = Vault::doc_dir(&doc.id);
    vault.write_json(&dir.join("meta.json"), doc)?;
    if let Some((name, text)) = text {
        vault.write(&dir.join(name), text)?;
    }
    let mut added = existing(vault, doc.id.clone())?;
    added.created = true;
    Ok(added)
}

/// A doc already in the vault: validated, not touched.
fn existing(vault: &Vault, id: DocId) -> Result<Added> {
    let meta = Vault::doc_dir(&id).join("meta.json");
    let value: Value = read_json(&vault.path(&meta))?;
    let missing = match value.get("kind").and_then(Value::as_str).map(Kind::parse) {
        Some(Ok(kind)) => Schema::compile(kind.as_str(), kind.schema())?.problems(&value),
        _ => vec![Problem::Missing("kind".to_owned())],
    };
    Ok(Added {
        id,
        created: false,
        missing,
        meta,
        text_added: false,
        warnings: Vec::new(),
    })
}

/// A web page as markdown via Jina's reader (`r.jina.ai`). `None` when the site
/// blocked it: bot walls come back as a 200 with a warning and a stub page.
fn read_page(url: &str) -> Option<String> {
    const MIN_BYTES: usize = 2000;
    let r = net::get(&format!("https://r.jina.ai/{url}")).ok()?;
    let md = String::from_utf8_lossy(&r.body).into_owned();
    let blocked = md.contains("Warning: Target URL returned error") || md.len() < MIN_BYTES;
    ((200..300).contains(&r.status) && !blocked).then_some(md)
}

/// pymupdf4llm (via `uvx`, keeps tables as markdown) if uv is installed, else poppler's `pdftotext`.
pub fn pdf_to_text(pdf: &Path) -> Result<String, String> {
    // OCR off: pymupdf4llm turns it on whenever Tesseract is installed, and fails
    // outright when Tesseract has no language data. arXiv PDFs have a text layer.
    const PY: &str = "import sys, pymupdf4llm; sys.stdout.write(\
        pymupdf4llm.to_markdown(sys.argv[1], show_progress=False, use_ocr=False))";
    let path = pdf.to_string_lossy();
    let tries: [(&str, Vec<&str>); 2] = [
        (
            "uvx",
            vec![
                "--quiet",
                "--from",
                "pymupdf4llm==1.28.2", // pinned: same text on every machine
                "python",
                "-c",
                PY,
                &path,
            ],
        ),
        ("pdftotext", vec!["-layout", &path, "-"]),
    ];
    let mut errors = Vec::new();
    for (bin, args) in tries {
        match Command::new(bin).args(&args).output() {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            Ok(out) => errors.push(format!(
                "{bin}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => errors.push(format!("{bin}: {e}")),
        }
    }
    Err(format!(
        "no PDF converter worked (install uv or poppler): {}",
        errors.join("; ")
    ))
}

/// Hex SHA-256 of a file, by coreutils `sha256sum` or macOS `shasum`.
pub(crate) fn sha256(path: &Path) -> Result<String> {
    let mut last = String::new();
    for (bin, args) in [("sha256sum", &[][..]), ("shasum", &["-a", "256"][..])] {
        match Command::new(bin).args(args).arg(path).output() {
            Ok(out) if out.status.success() => {
                let s = String::from_utf8_lossy(&out.stdout);
                return Ok(s.split_whitespace().next().unwrap_or_default().to_owned());
            }
            Ok(out) => last = String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            Err(e) => last = e.to_string(),
        }
    }
    Err(Error::Command {
        cmd: "sha256sum".to_owned(),
        detail: last,
    })
}

/// Text of `<title>`, entities for `&<>"'` decoded, whitespace collapsed.
pub(crate) fn html_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(start) = lower
        .find("<title")
        .and_then(|i| lower.get(i..)?.find('>').map(|j| i + j + 1))
    else {
        return String::new();
    };
    let end = lower
        .get(start..)
        .and_then(|s| s.find("</title>"))
        .map_or(start, |e| start + e);
    let t = html.get(start..end).unwrap_or_default();
    squash(
        &t.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&"),
    )
}

pub(crate) fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_html_titles() {
        assert_eq!(
            html_title("<html><head><TITLE>\n Brain &amp; contracts </title>"),
            "Brain & contracts"
        );
        assert_eq!(html_title("<p>none</p>"), "");
    }
}
