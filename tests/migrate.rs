//! `brain migrate-paperdb` against a fixture library, journal and readings. Papers with
//! Code is a stub, so nothing here touches the network.

#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test helpers fail the test by panicking"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use brain::error::Error;
use brain::id::ArxivId;
use brain::migrate::{self, Sources};
use brain::pwc::{self, Related};
use brain::time::Now;
use brain::{Result, Vault, lineage};
use rusqlite::Connection;
use serde_json::{Value, json};

fn tmp() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "brain-migrate-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, contents: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn vault(dir: &Path) -> Vault {
    let root = dir.join("vault");
    fs::create_dir_all(&root).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    Vault::open(&root).unwrap()
}

fn now() -> Now {
    Now {
        at: "2026-10-28T10:00:00+09:00".to_owned(),
        tz: "Asia/Tokyo".to_owned(),
        place: None,
    }
}

/// A one-page PDF with a text layer and an Info dictionary.
fn pdf(title: &str, author: &str) -> Vec<u8> {
    let stream = "BT /F1 18 Tf 20 100 Td (Hello reader) Tj ET";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Title ({title}) /Author ({author}) /CreationDate (D:20190101120000Z) >>"),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objs.len() + 1
    ));
    for o in offsets {
        out.push_str(&format!("{o:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objs.len() + 1
    ));
    out.into_bytes()
}

fn epub(dir: &Path, to: &Path) {
    let src = dir.join("epub-src");
    write(&src.join("mimetype"), "application/epub+zip");
    write(
        &src.join("META-INF/container.xml"),
        r#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    );
    write(
        &src.join("OEBPS/content.opf"),
        r#"<package><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title>Bottom Up</dc:title><dc:creator>Unknown</dc:creator></metadata>
            <manifest><item id="b" href="two.html"/><item id="a" href="one.html"/></manifest>
            <spine><itemref idref="a"/><itemref idref="b"/></spine></package>"#,
    );
    write(
        &src.join("OEBPS/one.html"),
        "<html><body><h1>First</h1><p>Alpha</p></body></html>",
    );
    write(
        &src.join("OEBPS/two.html"),
        "<html><body><p>Omega</p></body></html>",
    );
    let ok = Command::new("zip")
        .args(["-q", "-X", "-r"])
        .arg(to)
        .args(["mimetype", "META-INF", "OEBPS"])
        .current_dir(&src)
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

struct Fixture {
    papers: PathBuf,
    journal: PathBuf,
    readings: PathBuf,
}

fn fixture(dir: &Path) -> Fixture {
    let papers = dir.join("papers");
    write(
        &papers.join("papers/2410.24164/paper.json"),
        json!({
            "source": {"arxiv": "2410.24164"},
            "title": "π0: A Vision-Language-Action Flow Model",
            "name": "π0",
            "authors": ["Kevin Black"],
            "published": "2024-10-31",
            "abstract": "Flow matching on a VLM.",
            "tags": ["starred", "vla"],
            "links": ["https://github.com/Physical-Intelligence/openpi", "https://pi.website/blog/pi0"],
            "added": "2026-10-02",
            "card": {"family": "vla", "action_head": "flow-matching", "embodiment": []}
        })
        .to_string(),
    );
    write(
        &papers.join("papers/2410.24164/paper.md"),
        "# π0\n\nAction expert.\n",
    );
    write(
        &papers.join(".cache/pdf/2410.24164.pdf"),
        pdf("pi0", "Black"),
    );
    write(
        &papers.join("papers/some-blog/paper.json"),
        json!({
            "source": {"web": {"slug": "some-blog", "url": "https://www.example.com/blog/s1", "pdf": null}},
            "title": "S1", "authors": [], "published": "", "note": "A blog post.", "added": "2026-10-03"
        })
        .to_string(),
    );
    write(&papers.join("papers/some-blog/paper.md"), "# S1\n");
    write(
        &papers.join("inbox.jsonl"),
        "{\"id\":\"2608.05674\",\"title\":\"A\",\"published\":\"2026-08-06\",\"found\":\"2026-10-04\",\"state\":\"skipped\"}\n\
         {\"id\":\"2608.06332\",\"title\":\"B\",\"published\":\"2026-08-06\",\"found\":\"2026-10-04\",\"state\":\"new\"}\n",
    );

    let journal = dir.join("journal.db");
    let db = Connection::open(&journal).unwrap();
    db.execute_batch(
        "CREATE VIRTUAL TABLE journal USING fts5(day unindexed, machine unindexed, repo, branch, text);
         INSERT INTO journal VALUES ('2026-10-18', 'meerkat', 'drover', 'theo/axi-core', 'what: Made it AXI.\nwhy: Agents parse it.');",
    )
    .unwrap();
    drop(db);

    let readings = dir.join("readings");
    write(
        &readings.join("book.pdf"),
        pdf("A Test Book", "Ada Writer <ada[at]example.org>"),
    );
    epub(dir, &readings.join("bottom.epub"));
    write(
        &readings.join("course.html"),
        r#"<html><head><title>Legged Control</title><meta name="author" content="Jo Doe">
           <meta name="date" content="2021-05-01"><link rel="canonical" href="https://example.org/legged">
           </head><body><h1>LQR</h1><p>Riccati.</p></body></html>"#,
    );
    write(&readings.join(".DS_Store"), "junk");
    Fixture {
        papers,
        journal,
        readings,
    }
}

fn pi0_pwc(id: &ArxivId) -> Result<Option<pwc::Paper>> {
    assert_eq!(id.as_str(), "2410.24164");
    Ok(Some(pwc::Paper {
        tldr: Some("Flow matching on a VLM.".to_owned()),
        successor_papers: Some(vec![Related {
            arxiv_id: Some("2504.16054".to_owned()),
        }]),
        ..pwc::Paper::default()
    }))
}

fn pwc_down(_: &ArxivId) -> Result<Option<pwc::Paper>> {
    Err(Error::Http {
        url: "pwc".to_owned(),
        status: 503,
    })
}

fn meta(v: &Vault, dir: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(v.path(Path::new(dir)).join("meta.json")).unwrap())
        .unwrap()
}

/// Every file under the vault but `.git`, by relative path.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.file_name().unwrap() == ".git" {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_owned(),
                    fs::read(&p).unwrap(),
                );
            }
        }
    }
    out
}

fn lint_clean_but_books(v: &Vault) {
    let findings = brain::lint::lint(v).unwrap();
    for f in &findings {
        let m = meta(v, &f.file.parent().unwrap().to_string_lossy());
        assert_eq!(m["kind"], "book", "{f:?}");
    }
}

#[test]
fn migrates_papers_journal_readings_and_skips() {
    let dir = tmp();
    let fx = fixture(&dir);
    let v = vault(&dir);
    let src = Sources {
        papers: &fx.papers,
        journal: Some(&fx.journal),
        readings: Some(&fx.readings),
    };
    let r = migrate::run(&v, &src, &now(), &pi0_pwc).unwrap();
    assert_eq!(
        (
            r.papers, r.notes, r.pdfs, r.lineage, r.journal, r.books, r.skips, r.existing
        ),
        (1, 1, 1, 1, 1, 3, 1, 0),
        "{r:?}"
    );

    let p = meta(&v, "docs/arxiv--2410.24164");
    assert_eq!(p["kind"], "paper");
    assert_eq!(p["taint"], "external");
    assert_eq!(p["added"], "2026-10-02T12:00:00+09:00");
    assert_eq!(p["tags"], json!(["starred", "vla"]));
    assert_eq!(p["date"], "2024-10-31");
    assert_eq!(p["card"]["action_head"], "flow-matching");
    assert_eq!(
        p["links"],
        json!({"code": "https://github.com/Physical-Intelligence/openpi", "project": "https://pi.website/blog/pi0"})
    );
    assert_eq!(p["tldr"], "Flow matching on a VLM.");
    assert_eq!(
        p["lineage"],
        json!([{"src": "arxiv:2504.16054", "dst": "arxiv:2410.24164", "rel": "builds_on"}])
    );
    assert!(p.get("name").is_none());
    let pi0 = v.path(Path::new("docs/arxiv--2410.24164"));
    assert_eq!(
        fs::read_to_string(pi0.join("text.md")).unwrap(),
        "# π0\n\nAction expert.\n"
    );
    assert_eq!(
        fs::read(pi0.join("paper.pdf")).unwrap(),
        fs::read(fx.papers.join(".cache/pdf/2410.24164.pdf")).unwrap()
    );

    let n = meta(&v, "docs/web--example-com-blog-s1");
    assert_eq!(n["kind"], "note");
    assert_eq!(n["source"], "https://www.example.com/blog/s1");
    assert_eq!(n["why"], "A blog post.");

    let log = v.read(Path::new("log/2026-10-18.md")).unwrap().unwrap();
    assert_eq!(
        log,
        "## 2026-10-18T12:00:00+09:00 · Asia/Tokyo · - · journal · drover theo/axi-core\n\n\
         what: Made it AXI.\nwhy: Agents parse it.\n"
    );
    assert!(!log.contains("meerkat"));

    let labels = v.read(Path::new("labels.jsonl")).unwrap().unwrap();
    assert_eq!(
        labels,
        "{\"id\":\"arxiv:2608.05674\",\"verdict\":\"skip\",\"ts\":\"2026-10-04T12:00:00+09:00\",\"surface\":\"paperdb\"}\n"
    );

    let books: BTreeMap<String, Value> = fs::read_dir(v.path(Path::new("docs")))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("file--"))
        .map(|n| {
            let m = meta(&v, &format!("docs/{n}"));
            (m["source"].as_str().unwrap().to_owned(), m)
        })
        .collect();
    assert_eq!(books.len(), 3);
    let pdf = &books["book.pdf"];
    assert_eq!(pdf["kind"], "book");
    assert_eq!(pdf["title"], "A Test Book");
    assert_eq!(pdf["authors"], json!(["Ada Writer"]));
    assert_eq!(pdf["year"], 2019);
    assert!(pdf.get("links").is_none());
    let html = &books["course.html"];
    assert_eq!(
        (
            &html["title"],
            &html["authors"],
            &html["year"],
            &html["links"]
        ),
        (
            &json!("Legged Control"),
            &json!(["Jo Doe"]),
            &json!(2021),
            &json!({"home": "https://example.org/legged"})
        )
    );
    let ep = &books["bottom.epub"];
    assert_eq!(ep["title"], "Bottom Up");
    assert!(ep.get("authors").is_none(), "Unknown is no author");
    for (src, m) in &books {
        let d = v.path(&Path::new("docs").join(m["id"].as_str().unwrap().replace(':', "--")));
        let ext = src.rsplit('.').next().unwrap();
        assert_eq!(
            fs::read(d.join(format!("source.{ext}"))).unwrap(),
            fs::read(fx.readings.join(src)).unwrap()
        );
        let text = fs::read_to_string(d.join("text.md")).unwrap();
        let want = match ext {
            "pdf" => "Hello reader",
            "epub" => "Alpha",
            _ => "Riccati.",
        };
        assert!(text.contains(want), "{src}: {text}");
    }
    let ep_text = fs::read_to_string(
        v.path(&Path::new("docs").join(ep["id"].as_str().unwrap().replace(':', "--")))
            .join("text.md"),
    )
    .unwrap();
    assert!(ep_text.find("Alpha") < ep_text.find("Omega"), "spine order");
    let mut missing: Vec<String> = r.missing.iter().map(|(_, m)| m.join(" ")).collect();
    missing.sort();
    assert_eq!(missing, ["authors year links.home", "links.home"]);

    lint_clean_but_books(&v);
}

#[test]
fn a_second_run_changes_nothing() {
    let dir = tmp();
    let fx = fixture(&dir);
    let v = vault(&dir);
    let src = Sources {
        papers: &fx.papers,
        journal: Some(&fx.journal),
        readings: Some(&fx.readings),
    };
    let first = migrate::run(&v, &src, &now(), &pi0_pwc).unwrap();
    let before = snapshot(v.root());
    let sources = snapshot(&dir.join("papers"));
    let second = migrate::run(&v, &src, &now(), &pi0_pwc).unwrap();
    assert_eq!(snapshot(v.root()), before);
    assert_eq!(snapshot(&dir.join("papers")), sources);
    assert_eq!(
        (
            second.papers,
            second.notes,
            second.pdfs,
            second.journal,
            second.books,
            second.skips
        ),
        (0, 0, 0, 0, 0, 0)
    );
    assert_eq!(second.existing, 2 + 3);
    assert_eq!(second.missing, first.missing);
}

#[test]
fn lineage_waits_for_papers_with_code_then_reindex_fills_it() {
    let dir = tmp();
    let fx = fixture(&dir);
    let v = vault(&dir);
    let src = Sources {
        papers: &fx.papers,
        journal: None,
        readings: None,
    };
    let r = migrate::run(&v, &src, &now(), &pwc_down).unwrap();
    assert_eq!((r.papers, r.lineage), (1, 0));
    assert!(r.warnings.iter().any(|w| w.contains("reindex will retry")));
    assert!(meta(&v, "docs/arxiv--2410.24164").get("lineage").is_none());

    let index = dir.join("index.db");
    let down = lineage::retry(&v, &index, 10, &pwc_down).unwrap();
    assert_eq!((down.filled, down.pending), (0, 1));
    assert!(down.warning.is_some());
    // Tried within the day: not asked again, even with PwC back.
    let soon = lineage::retry(&v, &index, 10, &pi0_pwc).unwrap();
    assert_eq!((soon.filled, soon.pending, soon.warning), (0, 1, None));

    let fresh = lineage::retry(&v, &dir.join("fresh.db"), 10, &pi0_pwc).unwrap();
    assert_eq!((fresh.filled, fresh.pending), (1, 0));
    let p = meta(&v, "docs/arxiv--2410.24164");
    assert_eq!(p["lineage"][0]["src"], "arxiv:2504.16054");
    assert_eq!(p["tldr"], "Flow matching on a VLM.");
    let none = lineage::retry(&v, &dir.join("fresh.db"), 0, &pwc_down).unwrap();
    assert_eq!(none, lineage::Retried::default());
}
