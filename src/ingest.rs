//! Turning a reference into a library entry: metadata, then full text.

use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::date::Day;
use crate::error::{Error, Result};
use crate::id::{ArxivId, PaperId};
use crate::library::Library;
use crate::paper::{Paper, Source};
use crate::{arxiv, hub, net};

/// Metadata for an arXiv paper: the arXiv API first, the Hub for extras or when arXiv misses.
pub fn describe(id: &ArxivId, added: Day) -> Result<Paper> {
    let entry = arxiv::fetch(std::slice::from_ref(id))?.into_iter().next();
    // Hub data is enrichment: a Hub outage must not block adding a paper arXiv knows.
    let info = match hub::info(id) {
        Ok(info) => info,
        Err(e) if entry.is_some() => {
            eprintln!("warning: skipping Hugging Face extras for {id}: {e}");
            None
        }
        Err(e) => return Err(e),
    };
    let mut p = match (entry, &info) {
        (Some(e), _) => {
            let mut p = Paper::new(Source::Arxiv(e.id), e.title, added);
            p.authors = e.authors;
            p.published = e.published;
            p.abstract_ = e.abstract_;
            p
        }
        (None, Some(h)) => {
            let mut p = Paper::new(Source::Arxiv(id.clone()), squash(&h.title), added);
            p.authors = h.authors.iter().map(|a| a.name.clone()).collect();
            p.published = h.published_at.get(..10).unwrap_or_default().to_owned();
            p.abstract_ = squash(&h.summary);
            p
        }
        (None, None) => return Err(Error::Unknown(id.to_string())),
    };
    if let Some(h) = info {
        p.summary = h.ai_summary;
        p.keywords = h.ai_keywords;
        p.links.extend(
            h.project_page
                .into_iter()
                .chain(h.github_repo)
                .filter(|l| !l.is_empty()),
        );
    }
    Ok(p)
}

/// How to get text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Route {
    /// Hub markdown when available (fast, no PDF), else the PDF.
    Auto,
    /// Always the PDF. Slower, but keeps table structure the Hub flattens.
    Pdf,
}

/// Where the text came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Via {
    Hub,
    Pymupdf4llm,
    Pdftotext,
    /// Jina's reader (`r.jina.ai`): any web page as markdown.
    Reader,
}

impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hub => "hub",
            Self::Pymupdf4llm => "pymupdf4llm",
            Self::Pdftotext => "pdftotext",
            Self::Reader => "reader",
        })
    }
}

/// Fetch and store full text for `paper`.
pub fn fetch_text(lib: &Library, paper: &Paper, route: Route) -> Result<Via> {
    let id = paper.id();
    if let (Route::Auto, Source::Arxiv(aid)) = (route, &paper.source)
        && let Some(md) = hub::markdown(aid)?
    {
        lib.save_text(&id, &md)?;
        return Ok(Via::Hub);
    }
    let url = match (&paper.source, paper.pdf_url()) {
        (_, Some(pdf)) => pdf,
        (Source::Web { url, .. }, None) => {
            let md = read_page(url).ok_or_else(|| {
                no_text(
                    &id,
                    "the page reader was blocked; pipe the text in with `paperdb text <id> -`",
                )
            })?;
            lib.save_text(&id, &md)?;
            return Ok(Via::Reader);
        }
        (Source::Arxiv(_), None) => return Err(no_text(&id, "no PDF URL on record")),
    };
    let pdf = lib.cache().join("pdf").join(format!("{id}.pdf"));
    if !is_pdf(&pdf) {
        net::download(&url, &pdf)?;
        if !is_pdf(&pdf) {
            return Err(no_text(&id, &format!("{url} did not return a PDF")));
        }
    }
    let (text, via) = pdf_to_text(&pdf).map_err(|why| no_text(&id, &why))?;
    lib.save_text(&id, &text)?;
    Ok(via)
}

/// A web page (blog post, report) as markdown via Jina's reader. `None` when the
/// site blocked it: bot walls come back as a 200 with a warning and a stub page.
fn read_page(url: &str) -> Option<String> {
    const MIN_BYTES: usize = 2000;
    let r = net::get(&format!("https://r.jina.ai/{url}")).ok()?;
    let md = String::from_utf8_lossy(&r.body).into_owned();
    let blocked = md.contains("Warning: Target URL returned error") || md.len() < MIN_BYTES;
    ((200..300).contains(&r.status) && !blocked).then_some(md)
}

/// pymupdf4llm (via `uvx`, keeps tables as markdown) if uv is installed, else poppler's `pdftotext`.
fn pdf_to_text(pdf: &Path) -> Result<(String, Via), String> {
    // OCR off: pymupdf4llm turns it on whenever Tesseract is installed, and fails
    // outright when Tesseract has no language data. arXiv PDFs have a text layer.
    const PY: &str = "import sys, pymupdf4llm; sys.stdout.write(\
        pymupdf4llm.to_markdown(sys.argv[1], show_progress=False, use_ocr=False))";
    let path = pdf.to_string_lossy();
    let tries: [(Via, &str, Vec<&str>); 2] = [
        (
            Via::Pymupdf4llm,
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
        (Via::Pdftotext, "pdftotext", vec!["-layout", &path, "-"]),
    ];
    let mut errors = Vec::new();
    for (via, bin, args) in tries {
        match Command::new(bin).args(&args).output() {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                return Ok((String::from_utf8_lossy(&out.stdout).into_owned(), via));
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

fn is_pdf(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == b"%PDF"
}

fn no_text(id: &PaperId, why: &str) -> Error {
    Error::NoText {
        id: id.to_string(),
        why: why.to_owned(),
    }
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
