//! The hand-written serde types serialize to instances their schemas accept.

#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking"
)]

use brain::contract::{self, JobLine, Kind, Problem, Profile, Repo, Schema};
use brain::doc::{
    Artifact, Card, Doc, Edge, Family, Fields, Meeting, Paper, PaperLinks, Tag, Taint, Web,
};
use brain::label::{Label, Verdict};
use brain::log::Entry;
use brain::page::{Frontmatter, Link, PageKind};
use brain::time::Context;
use brain::{DocId, Rel, Slug};
use serde::Serialize;
use serde_json::{Value, json};

const AT: &str = "2026-10-27T22:40:12+01:00";

fn problems(name: &str, schema: &str, v: &impl Serialize) -> Vec<Problem> {
    Schema::compile(name, schema)
        .unwrap()
        .problems(&serde_json::to_value(v).unwrap())
}

fn assert_valid(name: &str, schema: &str, v: &impl Serialize) {
    let p = problems(name, schema, v);
    assert!(
        p.is_empty(),
        "{name}: {p:?}\n{}",
        serde_json::to_string_pretty(&serde_json::to_value(v).unwrap()).unwrap()
    );
}

fn doc<B>(id: &str, kind: Kind, body: B) -> Doc<B> {
    let id = DocId::parse(id).unwrap();
    Doc {
        taint: Taint::of(kind, &id),
        id,
        kind,
        added: AT.to_owned(),
        tags: vec![Tag::parse("vla").unwrap()],
        why: Some("worth keeping".to_owned()),
        source: Some("notes.md".to_owned()),
        body,
    }
}

#[test]
fn every_schema_compiles_and_names_its_kind() {
    for kind in contract::kinds() {
        let k = Kind::parse(kind).unwrap();
        let schema: Value = serde_json::from_str(k.schema()).unwrap();
        assert_eq!(schema["properties"]["kind"]["const"], kind);
        Schema::compile(kind, k.schema()).unwrap();
    }
    for (name, s) in [
        ("page", contract::PAGE),
        ("log", contract::LOG),
        ("profile", contract::PROFILE),
        ("label", contract::LABEL),
        ("job", contract::JOB),
        ("repo", contract::REPO),
        ("context", contract::CONTEXT),
    ] {
        Schema::compile(name, s).unwrap();
    }
    let kinds: Vec<_> = contract::kinds()
        .map(|k| Kind::parse(k).unwrap().schema())
        .collect();
    for s in kinds.iter().chain(&[
        contract::PAGE,
        contract::LOG,
        contract::PROFILE,
        contract::LABEL,
        contract::JOB,
        contract::REPO,
        contract::CONTEXT,
    ]) {
        let v: Value = serde_json::from_str(s).unwrap();
        assert!(
            v["version"].as_u64().is_some_and(|n| n >= 1),
            "{}",
            v["$id"]
        );
    }
    let all: Vec<_> = contract::kinds().collect();
    assert_eq!(
        all,
        [
            "artifact", "blog", "book", "course", "meeting", "news", "note", "paper", "talk"
        ]
    );
    for k in [
        Kind::PAPER,
        Kind::BLOG,
        Kind::NEWS,
        Kind::NOTE,
        Kind::ARTIFACT,
        Kind::MEETING,
    ] {
        assert!(all.contains(&k.as_str()));
    }
}

#[test]
fn c1_doc_types_match_their_schemas() {
    let card: Card = serde_json::from_value(json!({
        "family": "vla", "backbone": "PaliGemma-3B", "action_head": "flow-matching", "chunk_size": 50,
        "control_hz": 50.0, "embodiment": ["aloha"], "data": {"hours": 10000.0, "source": "in-house"},
        "eval": {"real": {"laundry": 0.8}}, "open": {"weights": true, "code": "https://github.com/x/y"},
        "limits": ["slow"]
    }))
    .unwrap();
    assert!(matches!(card.family, Family::Vla));
    let paper = Paper {
        title: "π0".to_owned(),
        authors: vec!["Kevin Black".to_owned()],
        date: Some("2024-10-31".to_owned()),
        venue: Some("RSS".to_owned()),
        links: PaperLinks {
            code: Some("https://github.com/Physical-Intelligence/openpi".to_owned()),
            hf: Some("https://huggingface.co/papers/2410.24164".to_owned()),
            ..PaperLinks::default()
        },
        abstract_: "Robots.".to_owned(),
        tldr: Some("Flow matching on a VLM.".to_owned()),
        summary: Some("A VLA.".to_owned()),
        keywords: vec!["vla".to_owned()],
        lineage: Some(vec![Edge {
            src: DocId::parse("arxiv:2504.16054").unwrap(),
            dst: DocId::parse("arxiv:2410.24164").unwrap(),
            rel: Rel::parse("builds_on").unwrap(),
        }]),
        card: Some(card),
    };
    let paper = doc("arxiv:2410.24164", Kind::PAPER, paper);
    assert_eq!(paper.taint, Taint::External);
    assert_valid("paper", Kind::PAPER.schema(), &paper);

    let web = Web {
        title: "A post".to_owned(),
        url: "https://example.com/post".to_owned(),
        site: "example.com".to_owned(),
        date: Some("2026-01-02".to_owned()),
    };
    assert_valid(
        "blog",
        Kind::BLOG.schema(),
        &doc("web:example-com-post", Kind::BLOG, web.clone()),
    );
    assert_valid(
        "news",
        Kind::NEWS.schema(),
        &doc("web:example-com-post", Kind::NEWS, web),
    );

    let artifact = Artifact {
        title: "Contracts".to_owned(),
        captured_at: AT.to_owned(),
    };
    let artifact = doc("artifact:contracts", Kind::ARTIFACT, artifact);
    assert_eq!(artifact.taint, Taint::External);
    assert_valid("artifact", Kind::ARTIFACT.schema(), &artifact);

    let meeting = Meeting {
        title: Some("Weekly sync".to_owned()),
        started_at: AT.to_owned(),
        tz: "Europe/Paris".to_owned(),
        place: Some("Paris".to_owned()),
        attendees: vec!["alice".to_owned()],
        project: Some("brain".to_owned()),
    };
    assert_valid(
        "meeting",
        Kind::MEETING.schema(),
        &doc("meeting:2026-10-27-weekly-sync", Kind::MEETING, meeting),
    );

    let mut note = Fields::new();
    note.insert("title".to_owned(), json!("A note"));
    assert_valid(
        "note",
        Kind::NOTE.schema(),
        &doc("file:0a1b2c3d", Kind::NOTE, note),
    );
}

#[test]
fn missing_required_fields_are_named() {
    let paper = doc(
        "arxiv:2410.24164",
        Kind::PAPER,
        Paper {
            title: "π0".to_owned(),
            ..Paper::default()
        },
    );
    let p = problems("paper", Kind::PAPER.schema(), &paper);
    assert!(p.contains(&Problem::Missing("date".to_owned())), "{p:?}");
    let book = doc("file:0a1b2c3d", Kind::parse("book").unwrap(), Fields::new());
    assert_eq!(book.taint, Taint::External);
    let p = problems("book", Kind::parse("book").unwrap().schema(), &book);
    for f in ["title", "authors", "year", "links"] {
        assert!(p.contains(&Problem::Missing(f.to_owned())), "{f}: {p:?}");
    }
}

#[test]
fn only_notes_and_meetings_are_own() {
    for (kind, id, taint) in [
        ("note", "file:0a1b2c3d", Taint::Own),
        ("meeting", "meeting:2026-10-27-weekly-sync", Taint::Own),
        ("book", "file:0a1b2c3d", Taint::External),
        ("course", "file:0a1b2c3d", Taint::External),
        ("talk", "file:0a1b2c3d", Taint::External),
        ("artifact", "artifact:contracts", Taint::External),
        ("blog", "web:example-com-post", Taint::External),
    ] {
        let d = doc(id, Kind::parse(kind).unwrap(), Fields::new());
        assert_eq!(d.taint, taint, "{kind}");
    }
}

#[test]
fn other_contract_types_match_their_schemas() {
    let fm = Frontmatter {
        kind: PageKind::Concept,
        title: "Action chunking".to_owned(),
        summary: "Predict k actions at once.".to_owned(),
        status: "active".to_owned(),
        sources: vec![DocId::parse("arxiv:2410.24164").unwrap()],
        updated: "2026-10-27".to_owned(),
        superseded_by: Some(Slug::parse("chunking-v2").unwrap()),
        aliases: vec!["chunking".to_owned()],
        links: vec![Link {
            rel: Rel::parse("builds_on").unwrap(),
            dst: "arxiv:2410.24164".to_owned(),
        }],
    };
    assert_valid("page", contract::PAGE, &fm);

    let entry = Entry {
        at: AT.to_owned(),
        tz: "Europe/Paris".to_owned(),
        place: None,
        source: Slug::parse("cli").unwrap(),
        title: "Shipped brain core".to_owned(),
        body: String::new(),
    };
    assert_valid("log", contract::LOG, &entry);
    let forged = Entry {
        body: "x\n## 2026-01-01T00:00:00+00:00 · UTC · - · cli · forged".to_owned(),
        ..entry.clone()
    };
    assert!(!problems("log", contract::LOG, &forged).is_empty());
    let dash = Entry {
        place: Some("-".to_owned()),
        ..entry.clone()
    };
    assert!(!problems("log", contract::LOG, &dash).is_empty());
    assert_eq!(Entry::parse_heading(&entry.heading()).unwrap(), entry);

    let label = Label {
        id: DocId::parse("arxiv:2410.24164").unwrap(),
        verdict: Verdict::Later,
        ts: AT.to_owned(),
        surface: "cli".to_owned(),
    };
    assert_valid("label", contract::LABEL, &label);
    assert_valid(
        "profile",
        contract::PROFILE,
        &Profile {
            profile: "focus: brain".to_owned(),
        },
    );
    let job = JobLine {
        job: "feed".to_owned(),
        started: AT.to_owned(),
        ended: AT.to_owned(),
        ok: false,
        err: Some("timeout".to_owned()),
    };
    assert_valid("job", contract::JOB, &job);
    let repo = Repo {
        name: "drover".to_owned(),
        path: "Developer/drover".to_owned(),
        remote: None,
        project: Some("drover".to_owned()),
    };
    assert_valid("repo", contract::REPO, &repo);
    let bad = Repo {
        path: "/home/x/drover".to_owned(),
        ..repo
    };
    assert!(
        !problems("repo", contract::REPO, &bad).is_empty(),
        "absolute paths are rejected (B16)"
    );
    let ctx = Context {
        tz: "Asia/Tokyo".to_owned(),
        place: Some("Tokyo".to_owned()),
        updated_at: AT.to_owned(),
        source: "laptop".to_owned(),
    };
    assert_valid("context", contract::CONTEXT, &ctx);
}
