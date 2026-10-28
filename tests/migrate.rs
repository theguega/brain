//! `brain migrate-paperdb` against a fixture library, journal and readings. Papers with
//! Code and HAL are stubs, so nothing here touches the network.

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
use brain::hal;
use brain::id::ArxivId;
use brain::migrate::{self, Lookups, Report, Sources};
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

/// An epub whose spine lists `one.html` before `t[w]o.html` (a name unzip would read as a pattern).
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
            <dc:title>Bottom Up</dc:title><dc:creator>Unknown</dc:creator><dc:date>2018-03-01</dc:date></metadata>
            <manifest><item id="b" href="t[w]o.html#start"/><item id="a" href="one.html"/></manifest>
            <spine><itemref idref="a"/><itemref idref="b"/></spine></package>"#,
    );
    write(
        &src.join("OEBPS/one.html"),
        "<html><body><h1>First</h1><p>Alpha</p></body></html>",
    );
    write(
        &src.join("OEBPS/t[w]o.html"),
        "<html><body><p>Omega</p></body></html>",
    );
    write(
        &src.join("OEBPS/tao.html"),
        "<html><body><p>Wrong member</p></body></html>",
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

fn journal_db(path: &Path, rows: &[(&str, &str, &str, &str)]) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS journal USING fts5(day unindexed, machine unindexed, repo, branch, text);",
    )
    .unwrap();
    for (day, repo, branch, text) in rows {
        db.execute(
            "INSERT INTO journal VALUES (?1, 'meerkat', ?2, ?3, ?4)",
            [day, repo, branch, text],
        )
        .unwrap();
    }
}

struct Fixture {
    papers: PathBuf,
    journal: PathBuf,
    readings: PathBuf,
}

impl Fixture {
    fn sources(&self) -> Sources<'_> {
        Sources {
            papers: &self.papers,
            journal: Some(&self.journal),
            readings: Some(&self.readings),
        }
    }
}

fn entry(papers: &Path, name: &str, json: Value, text: &str) {
    write(
        &papers.join("papers").join(name).join("paper.json"),
        json.to_string(),
    );
    write(&papers.join("papers").join(name).join("paper.md"), text);
}

fn fixture(dir: &Path) -> Fixture {
    let papers = dir.join("papers");
    entry(
        &papers,
        "2410.24164",
        json!({
            "source": {"arxiv": "2410.24164"},
            "title": "$π_0$: A Vision-Language-Action Flow Model",
            "name": "π0",
            "authors": ["Kevin Black"],
            "published": "2024-10-31",
            "abstract": "Flow matching on a VLM.",
            "tags": ["starred", "vla"],
            "links": ["https://github.com/Physical-Intelligence/openpi", "https://pi.website/blog/pi0", "https://pi.website/other"],
            "keywords": ["vla"],
            "added": "2026-10-02",
            "card": {"family": "vla", "action_head": "flow-matching", "embodiment": []},
            "rating": 5
        }),
        "# π0\n\nAction expert.\n",
    );
    write(
        &papers.join(".cache/pdf/2410.24164.pdf"),
        pdf("pi0", "Black"),
    );
    entry(
        &papers,
        "2501.09747",
        json!({
            "source": {"arxiv": "2501.09747"}, "title": "FAST: Efficient Action Tokenization",
            "name": "FAST", "authors": ["Karl Pertsch"], "published": "2025-01-16", "added": "2026-10-02"
        }),
        "# FAST\n",
    );
    entry(
        &papers,
        "skild-s1",
        json!({
            "source": {"web": {"slug": "skild-s1", "url": "https://www.skild.ai/blogs/s1", "pdf": "https://www.skild.ai/blogs/s1"}},
            "title": "Skild S1", "name": "Skild-S1", "authors": [], "published": "", "note": "A blog post.",
            "added": "2026-10-03", "card": {"family": "vla"}
        }),
        "# S1\n",
    );
    entry(
        &papers,
        "pinocchio",
        json!({
            "source": {"web": {"slug": "pinocchio", "url": "https://laas.hal.science/hal-01866228v2", "pdf": "https://laas.hal.science/hal-01866228v2/document"}},
            "title": "Pinocchio: fast rigid body dynamics", "name": "Pinocchio", "authors": [], "published": "",
            "added": "2026-10-02", "card": {"family": "library"}
        }),
        "# Pinocchio\n",
    );
    write(&papers.join("papers/README"), "not an entry");
    write(
        &papers.join("inbox.jsonl"),
        "{\"id\":\"2608.05674\",\"title\":\"A\",\"published\":\"2026-08-06\",\"found\":\"2026-10-04\",\"state\":\"skipped\"}\n\
         {\"id\":\"2608.06332\",\"title\":\"B\",\"published\":\"2026-08-06\",\"found\":\"2026-10-04\",\"state\":\"new\"}\n",
    );

    let journal = dir.join("journal.db");
    journal_db(
        &journal,
        &[
            (
                "2026-10-18",
                "drover",
                "theo/axi-core",
                "what: Made it AXI.\nwhy: Agents parse it.",
            ),
            (
                "2026-10-18",
                "drover",
                "theo/axi-core",
                "what: Second pass on the same branch.",
            ),
            ("2026-10-18", "", "", "what: A row with no repo."),
        ],
    );

    let readings = dir.join("readings");
    write(
        &readings.join("book.pdf"),
        pdf("A Test Book", "Ada Writer <ada[at]example.org>"),
    );
    write(
        &readings.join("copy of book.pdf"),
        pdf("A Test Book", "Ada Writer <ada[at]example.org>"),
    );
    write(&readings.join("hack.pdf"), pdf("hack2e_03.book", "user"));
    write(&readings.join("broken.epub"), "not a zip");
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

/// PwC: π0 has lineage, FAST is unknown (404).
fn pwc_stub(id: &ArxivId) -> Result<Option<pwc::Paper>> {
    Ok((id.as_str() == "2410.24164").then(|| pwc::Paper {
        arxiv_id: Some("2410.24164".to_owned()),
        tldr: Some("Flow matching on a VLM.".to_owned()),
        conference_name: Some("RSS".to_owned()),
        predecessor_papers: Some(vec![]),
        successor_papers: Some(vec![Related {
            arxiv_id: Some("2504.16054".to_owned()),
        }]),
    }))
}

fn pwc_down(_: &ArxivId) -> Result<Option<pwc::Paper>> {
    Err(Error::Http {
        url: "pwc".to_owned(),
        status: 503,
    })
}

fn hal_stub(id: &str) -> Result<Option<hal::Record>> {
    assert_eq!(id, "hal-01866228");
    Ok(Some(hal::Record {
        authors: vec!["Justin Carpentier".to_owned()],
        date: Some("2019-01-14".to_owned()),
        venue: Some("SII 2019".to_owned()),
    }))
}

fn hal_none(_: &str) -> Result<Option<hal::Record>> {
    Ok(None)
}

const NET: Lookups<'static> = Lookups {
    pwc: &pwc_stub,
    hal: &hal_stub,
};

fn meta(v: &Vault, dir: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(v.path(Path::new(dir)).join("meta.json")).unwrap())
        .unwrap()
}

/// Every file under `root` but `.git`, by relative path.
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

fn books(v: &Vault) -> BTreeMap<String, Value> {
    fs::read_dir(v.path(Path::new("docs")))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("file--"))
        .map(|n| {
            let m = meta(v, &format!("docs/{n}"));
            (m["source"].as_str().unwrap().to_owned(), m)
        })
        .collect()
}

fn warned(r: &Report, needle: &str) -> bool {
    r.warnings.iter().any(|w| w.contains(needle))
}

#[test]
fn migrates_papers_journal_readings_and_skips() {
    let dir = tmp();
    let fx = fixture(&dir);
    let v = vault(&dir);
    let r = migrate::run(&v, &fx.sources(), &now(), NET);
    assert_eq!(
        (
            r.papers, r.blogs, r.pdfs, r.lineage, r.journal, r.books, r.skips, r.existing
        ),
        (3, 1, 1, 1, 3, 4, 1, 0),
        "{r:#?}"
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
    assert!(warned(
        &r,
        "second link of its kind not carried over: https://pi.website/other"
    ));
    assert!(warned(
        &r,
        "arxiv:2410.24164: paper.json field `rating` not carried over"
    ));
    assert_eq!(
        p["keywords"],
        json!(["π0", "vla"]),
        "the name leads the keywords"
    );
    assert_eq!(p["tldr"], "Flow matching on a VLM.");
    assert_eq!(p["venue"], "RSS");
    assert_eq!(
        p["lineage"],
        json!([{"src": "arxiv:2504.16054", "dst": "arxiv:2410.24164", "rel": "builds_on"}])
    );
    let pi0 = v.path(Path::new("docs/arxiv--2410.24164"));
    assert_eq!(
        fs::read_to_string(pi0.join("text.md")).unwrap(),
        "# π0\n\nAction expert.\n"
    );
    assert_eq!(
        fs::read(pi0.join("paper.pdf")).unwrap(),
        fs::read(fx.papers.join(".cache/pdf/2410.24164.pdf")).unwrap()
    );

    let fast = meta(&v, "docs/arxiv--2501.09747");
    assert!(fast.get("lineage").is_none(), "a PwC 404 is no answer");
    assert!(
        fast.get("keywords").is_none(),
        "FAST is already in the title"
    );

    let blog = meta(&v, "docs/web--skild-ai-blogs-s1");
    assert_eq!(
        (
            &blog["kind"],
            &blog["taint"],
            &blog["date"],
            &blog["site"],
            &blog["url"]
        ),
        (
            &json!("blog"),
            &json!("external"),
            &json!("2026-10-03"),
            &json!("skild.ai"),
            &json!("https://www.skild.ai/blogs/s1")
        )
    );
    assert_eq!(blog["card"]["family"], "vla");
    assert_eq!(blog["why"], "A blog post.");
    assert!(warned(&r, "name \"Skild-S1\" not carried over"));

    let pin = meta(&v, "docs/web--laas-hal-science-hal-01866228v2");
    assert_eq!(pin["kind"], "paper");
    assert_eq!(pin["authors"], json!(["Justin Carpentier"]));
    assert_eq!(pin["date"], "2019-01-14");
    assert_eq!(pin["venue"], "SII 2019");
    assert_eq!(pin["card"]["family"], "library");
    assert_eq!(
        pin["links"]["project"],
        "https://laas.hal.science/hal-01866228v2"
    );
    assert!(warned(
        &r,
        "its PDF link https://laas.hal.science/hal-01866228v2/document"
    ));
    assert!(warned(&r, "papers/README: not an entry directory"));

    let log = v.read(Path::new("log/2026-10-18.md")).unwrap().unwrap();
    assert_eq!(
        log,
        "## 2026-10-18T12:00:00+09:00 · Asia/Tokyo · - · journal · drover theo/axi-core\n\n\
         what: Made it AXI.\nwhy: Agents parse it.\n\n\
         ## 2026-10-18T12:00:00+09:00 · Asia/Tokyo · - · journal · drover theo/axi-core\n\n\
         what: Second pass on the same branch.\n\n\
         ## 2026-10-18T12:00:00+09:00 · Asia/Tokyo · - · journal · journal\n\n\
         what: A row with no repo.\n"
    );

    let labels = v.read(Path::new("labels.jsonl")).unwrap().unwrap();
    assert_eq!(
        labels,
        "{\"id\":\"arxiv:2608.05674\",\"verdict\":\"skip\",\"ts\":\"2026-10-04T12:00:00+09:00\",\"surface\":\"paperdb\"}\n"
    );

    let books = books(&v);
    assert_eq!(books.len(), 4, "{:?}", books.keys());
    assert!(
        r.failed.iter().any(|f| f.starts_with("broken.epub")),
        "{:?}",
        r.failed
    );
    assert!(warned(&r, "copy of book.pdf: the same file as book.pdf"));
    let pdf = &books["book.pdf"];
    assert_eq!(pdf["kind"], "book");
    assert_eq!(pdf["title"], "A Test Book");
    assert_eq!(pdf["authors"], json!(["Ada Writer"]));
    assert!(
        pdf.get("year").is_none(),
        "CreationDate is no publication year"
    );
    let hack = &books["hack.pdf"];
    assert!(
        hack.get("title").is_none() && hack.get("authors").is_none(),
        "{hack}"
    );
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
    assert_eq!(
        (&ep["title"], &ep["year"]),
        (&json!("Bottom Up"), &json!(2018))
    );
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
        if ext == "epub" {
            assert!(
                text.find("Alpha") < text.find("Omega"),
                "spine order: {text}"
            );
            assert!(!text.contains("Wrong member"), "{text}");
        }
    }
    let mut missing: Vec<String> = r.missing.iter().map(|(_, m)| m.join(" ")).collect();
    missing.sort();
    assert_eq!(
        missing,
        [
            "authors links.home",
            "title authors year links.home",
            "year links.home"
        ]
    );

    for f in brain::lint::lint(&v).unwrap() {
        let m = meta(&v, &f.file.parent().unwrap().to_string_lossy());
        assert_eq!(m["kind"], "book", "{f:?}");
    }
}

#[test]
fn a_second_run_changes_nothing_and_new_rows_still_land() {
    let dir = tmp();
    let fx = fixture(&dir);
    let v = vault(&dir);
    let first = migrate::run(&v, &fx.sources(), &now(), NET);
    let before = snapshot(v.root());
    let sources = snapshot(&dir.join("papers"));
    let second = migrate::run(&v, &fx.sources(), &now(), NET);
    assert_eq!(snapshot(v.root()), before);
    assert_eq!(snapshot(&dir.join("papers")), sources);
    assert_eq!(
        (
            second.papers,
            second.blogs,
            second.pdfs,
            second.journal,
            second.books,
            second.skips
        ),
        (0, 0, 0, 0, 0, 0)
    );
    assert_eq!(second.existing, 4 + 4);
    assert_eq!(second.missing, first.missing);
    assert!(!warned(&second, "changed since"), "{:?}", second.warnings);

    // drover adds a row on a branch it already wrote about: the new block lands.
    journal_db(
        &fx.journal,
        &[
            ("2026-10-18", "drover", "theo/axi-core", "what: Third pass."),
            ("2026-10-18", "drover", "theo/axi-core", "what: Third pass."),
        ],
    );
    let third = migrate::run(&v, &fx.sources(), &now(), NET);
    assert_eq!(third.journal, 2, "an identical row is a block of its own");
    let log = v.read(Path::new("log/2026-10-18.md")).unwrap().unwrap();
    assert_eq!(log.matches("what: Third pass.").count(), 2);
    assert_eq!(migrate::run(&v, &fx.sources(), &now(), NET).journal, 0);
}

#[test]
fn a_changed_source_is_reported_not_applied() {
    let dir = tmp();
    let fx = fixture(&dir);
    let v = vault(&dir);
    migrate::run(&v, &fx.sources(), &now(), NET);
    let before = snapshot(v.root());
    write(
        &fx.papers.join("papers/2410.24164/paper.md"),
        "# π0, edited\n",
    );
    let mut j: Value = serde_json::from_str(
        &fs::read_to_string(fx.papers.join("papers/skild-s1/paper.json")).unwrap(),
    )
    .unwrap();
    j["title"] = json!("Skild S1, renamed");
    write(&fx.papers.join("papers/skild-s1/paper.json"), j.to_string());
    write(
        &fx.readings.join("course.html"),
        "<html><head><title>Edited</title></head></html>",
    );
    let r = migrate::run(&v, &fx.sources(), &now(), NET);
    assert_eq!(snapshot(v.root()), before);
    assert!(
        warned(
            &r,
            "arxiv:2410.24164: papers/2410.24164 changed since it was migrated (text.md)"
        ),
        "{:?}",
        r.warnings
    );
    assert!(warned(
        &r,
        "web:skild-ai-blogs-s1: papers/skild-s1 changed since it was migrated (title)"
    ));
    assert!(warned(
        &r,
        "course.html: changed since it was migrated as file:"
    ));
}

#[test]
fn hal_without_a_record_stays_a_blog() {
    let dir = tmp();
    let fx = fixture(&dir);
    let v = vault(&dir);
    let src = Sources {
        papers: &fx.papers,
        journal: None,
        readings: None,
    };
    let r = migrate::run(
        &v,
        &src,
        &now(),
        Lookups {
            pwc: &pwc_stub,
            hal: &hal_none,
        },
    );
    assert_eq!((r.papers, r.blogs), (2, 2));
    let pin = meta(&v, "docs/web--laas-hal-science-hal-01866228v2");
    assert_eq!(
        (&pin["kind"], &pin["date"]),
        (&json!("blog"), &json!("2026-10-02"))
    );
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
    let down = Lookups {
        pwc: &pwc_down,
        hal: &hal_stub,
    };
    let r = migrate::run(&v, &src, &now(), down);
    assert_eq!((r.papers, r.lineage), (3, 0));
    assert!(warned(&r, "reindex will retry"));
    assert!(meta(&v, "docs/arxiv--2410.24164").get("lineage").is_none());

    let index = dir.join("index.db");
    let tried = lineage::retry(&v, &index, 10, &pwc_down).unwrap();
    assert_eq!((tried.filled, tried.pending), (0, 2));
    assert!(tried.warning.is_some());
    // Tried within the day: not asked again, even with PwC back.
    let soon = lineage::retry(&v, &index, 10, &pwc_stub).unwrap();
    assert_eq!((soon.filled, soon.pending, soon.warning), (0, 2, None));

    let fresh = dir.join("fresh.db");
    let back = lineage::retry(&v, &fresh, 10, &pwc_stub).unwrap();
    assert_eq!(
        (back.filled, back.pending),
        (1, 1),
        "FAST is a 404: still pending"
    );
    let p = meta(&v, "docs/arxiv--2410.24164");
    assert_eq!(p["lineage"][0]["src"], "arxiv:2504.16054");
    assert_eq!(
        (&p["tldr"], &p["venue"]),
        (&json!("Flow matching on a VLM."), &json!("RSS"))
    );
    assert!(meta(&v, "docs/arxiv--2501.09747").get("lineage").is_none());
}

#[test]
fn only_a_real_pwc_answer_sets_lineage() {
    let id = brain::DocId::parse("arxiv:2410.24164").unwrap();
    assert_eq!(lineage::Answer::of(&id, None).lineage, None, "404");
    assert_eq!(
        lineage::Answer::of(&id, Some(pwc::Paper::default())).lineage,
        None,
        "{{}}"
    );
    let empty = pwc::Paper {
        predecessor_papers: Some(vec![]),
        successor_papers: Some(vec![]),
        ..pwc::Paper::default()
    };
    assert_eq!(lineage::Answer::of(&id, Some(empty)).lineage, Some(vec![]));
}

#[test]
fn a_failed_copy_leaves_no_temp_file() {
    let dir = tmp();
    let v = vault(&dir);
    assert!(
        v.copy(&dir.join("absent"), Path::new("docs/x/paper.pdf"))
            .is_err()
    );
    let left: Vec<_> = fs::read_dir(v.path(Path::new("docs/x"))).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_pwc_record_with_null_lists_is_an_answer() {
    let id = brain::DocId::parse("arxiv:2410.24164").unwrap();
    let parse = |s: &str| serde_json::from_str::<pwc::Paper>(s).unwrap();
    let none = parse(
        r#"{"arxiv_id": "2410.24164", "predecessor_papers": null, "successor_papers": null}"#,
    );
    assert_eq!(lineage::Answer::of(&id, Some(none)).lineage, Some(vec![]));
    assert_eq!(lineage::Answer::of(&id, Some(parse("{}"))).lineage, None);
}

/// Fails for π0, answers with no lineage for FAST.
fn pwc_flaky(id: &ArxivId) -> Result<Option<pwc::Paper>> {
    if id.as_str() == "2410.24164" {
        return pwc_down(id);
    }
    Ok(Some(pwc::Paper {
        arxiv_id: Some(id.as_str().to_owned()),
        ..pwc::Paper::default()
    }))
}

#[test]
fn one_pwc_failure_costs_only_that_paper() {
    let dir = tmp();
    let fx = fixture(&dir);
    let v = vault(&dir);
    let src = Sources {
        papers: &fx.papers,
        journal: None,
        readings: None,
    };
    let net = Lookups {
        pwc: &pwc_flaky,
        hal: &hal_stub,
    };
    let r = migrate::run(&v, &src, &now(), net);
    assert_eq!(r.lineage, 1);
    assert!(warned(&r, "arxiv:2410.24164: papers with code"));
    assert!(meta(&v, "docs/arxiv--2410.24164").get("lineage").is_none());
    assert_eq!(meta(&v, "docs/arxiv--2501.09747")["lineage"], json!([]));
}
