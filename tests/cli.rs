//! The binary against throwaway vaults. Nothing here touches ~/brain or the network,
//! except `add_pi0_from_the_network` (ignored by default).

#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Env {
    dir: PathBuf,
}

impl Env {
    /// A fresh home with a C15 context: `tz` and `place`, or none at all.
    fn new(context: Option<(&str, &str)>) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "brain-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("state")).unwrap();
        if let Some((tz, place)) = context {
            let ctx = serde_json::json!({"tz": tz, "place": place, "updated_at": "2026-10-27T10:00:00+09:00", "source": "test"});
            fs::write(dir.join("state/context.json"), ctx.to_string()).unwrap();
        }
        Self { dir }
    }

    fn vault(&self) -> PathBuf {
        self.dir.join("vault")
    }

    fn cmd(&self, delay: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_brain"));
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.dir)
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("BRAIN_VAULT", self.vault())
            .env("BRAIN_COMMIT_DELAY", delay)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .current_dir(&self.dir);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd("0").args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.vault().join(rel)).unwrap()
    }

    /// The doc's note frontmatter, as JSON.
    fn meta(&self, dir: &str) -> Value {
        let text = self.read(&format!("docs/{dir}/{dir}.md"));
        Value::Object(brain::note::parse(brain::note::split(&text).unwrap().0).unwrap())
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.vault())
            .args(args)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    /// Whether a committer holds the marker, i.e. a commit is pending.
    fn pending(&self) -> bool {
        fs::File::open(self.vault().join(".git/brain-commit"))
            .is_ok_and(|f| matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock)))
    }

    /// Whether a write or a commit holds the vault lock.
    fn busy(&self) -> bool {
        fs::File::open(self.vault())
            .is_ok_and(|f| matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock)))
    }

    /// Waits for a pending background commit to land. The committer takes the vault lock
    /// before it releases the marker, so one of the two is held until it is done.
    fn settle(&self) {
        let start = Instant::now();
        while (self.pending() || self.busy()) && start.elapsed() < Duration::from_secs(15) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Runs `brain` with `args` in `n` processes at once; all must succeed.
    fn parallel(&self, n: usize, args: impl Fn(usize) -> Vec<String>) {
        let children: Vec<_> = (0..n)
            .map(|i| {
                self.cmd("1")
                    .args(args(i))
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for c in children {
            let out = c.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // A committer still running would recreate files under a removed tree.
        self.settle();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const PI0: &str = "---
id: arxiv:2410.24164
kind: paper
taint: external
added: \"2026-10-27T10:00:00+09:00\"
title: π0
authors:
- Kevin Black
date: \"2024-10-31\"
links: {}
---
# π0
";
const PI0_NOTE: &str = "vault/docs/arxiv--2410.24164/arxiv--2410.24164.md";

#[test]
fn bare_brain_creates_a_local_vault_with_no_remote() {
    let env = Env::new(None);
    assert_eq!(env.ok(&[]), "profile: none\n");
    assert!(env.vault().join(".git").is_dir());
    assert_eq!(env.git(&["remote"]), "");
    assert_eq!(env.git(&["log", "--format=%s"]), "brain: create vault\n");
    fs::write(env.vault().join("profile.md"), "focus: brain core\n").unwrap();
    assert_eq!(env.ok(&[]), "focus: brain core\n");
    assert_eq!(
        env.ok(&["--json"]),
        "{\"profile\":\"focus: brain core\\n\"}\n"
    );
}

#[test]
fn adding_the_same_arxiv_id_twice_changes_nothing() {
    let env = Env::new(None);
    env.ok(&[]);
    env.write(PI0_NOTE, PI0);
    env.git(&["add", "-A"]);
    env.git(&["-c", "commit.gpgsign=false", "commit", "-qm", "seed"]);
    for input in [
        "2410.24164",
        "https://arxiv.org/abs/2410.24164v2",
        "arxiv:2410.24164",
    ] {
        let out = env.ok(&["add", input]);
        assert_eq!(
            out,
            "id: arxiv:2410.24164\nnoop: already in the vault\nmissing: none\nhelp: brain get arxiv:2410.24164\n"
        );
    }
    assert_eq!(env.read("docs/arxiv--2410.24164/arxiv--2410.24164.md"), PI0);
    assert_eq!(env.git(&["status", "--porcelain"]), "");
    assert!(
        !env.vault().join(".git/brain-commit-pending").exists(),
        "a noop schedules no commit"
    );
}

#[test]
fn lint_names_a_missing_required_field_and_exits_1() {
    let env = Env::new(None);
    env.ok(&[]);
    assert_eq!(env.ok(&["lint"]), "errors[0/0]{file,error}:\n");
    env.write(PI0_NOTE, &PI0.replace("authors:\n- Kevin Black\n", ""));
    env.write("vault/pages/jepa.md", "---\nkind: concept\ntitle: JEPA\nstatus: active\nsources: []\nupdated: 2026-10-27\n---\n# JEPA\n");
    let out = env.run(&["lint"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "errors[2/2]{file,error}:\n  docs/arxiv--2410.24164/arxiv--2410.24164.md,missing required authors\n  pages/jepa.md,missing required summary\n"
    );
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.starts_with("error: 2 lint errors\nnext: "), "{err}");
    let json = env.run(&["lint", "--json"]);
    assert_eq!(
        String::from_utf8(json.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        r#"{"error":"missing required authors","file":"docs/arxiv--2410.24164/arxiv--2410.24164.md"}"#
    );
}

#[test]
fn lint_warns_about_unknown_page_keys_and_still_exits_0() {
    let env = Env::new(None);
    env.ok(&[]);
    let fm = "kind: concept\nsummary: Joint embeddings.\nstatus: active\nsources: []\nupdated: 2026-10-27\ntags: [ml/ssl, jepa]\n";
    env.write(
        "vault/pages/jepa.md",
        &format!("---\ntitle: JEPA\n{fm}cssclasses: [wide]\n---\n# JEPA\n"),
    );
    let out = env.run(&["lint"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "errors[0/0]{file,error}:\n"
    );
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("warning: pages/jepa.md: unknown frontmatter key `cssclasses`\n"),
        "{err}"
    );
    assert!(!err.contains("`tags`"), "{err}");

    env.write("vault/pages/jepa.md", &format!("---\n{fm}---\n# JEPA\n"));
    let out = env.run(&["lint"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "errors[1/1]{file,error}:\n  pages/jepa.md,missing required title\n"
    );
}

#[test]
fn lint_without_a_vault_is_an_error_with_next() {
    let env = Env::new(None);
    let out = env.run(&["lint", "--vault", env.dir.join("nowhere").to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.starts_with("error: no vault at "), "{err}");
    assert!(err.contains("\nnext: "), "{err}");
    assert!(!env.dir.join("nowhere").exists());
}

#[test]
fn usage_errors_exit_2() {
    let env = Env::new(None);
    assert_eq!(
        env.run(&["add", "x.md", "--kind", "podcast"]).status.code(),
        Some(2)
    );
    assert_eq!(env.run(&["search", "x"]).status.code(), Some(2));
    assert_eq!(
        env.run(&["label", "2410.24164", "love"]).status.code(),
        Some(2)
    );
}

#[test]
fn log_block_carries_offset_zone_and_place() {
    let env = Env::new(Some(("Asia/Tokyo", "Tokyo")));
    let out = env.ok(&[
        "log",
        "Shipped brain core\nAll green.",
        "--source",
        "shutdown",
    ]);
    let at = out
        .lines()
        .next()
        .unwrap()
        .strip_prefix("at: ")
        .unwrap()
        .to_owned();
    assert!(at.ends_with("+09:00") && at.len() == 25, "{at}");
    let file = format!("log/{}.md", &at[..10]);
    assert_eq!(out, format!("at: {at}\nfile: {file}\n"));
    assert_eq!(
        env.read(&file),
        format!("## {at} · Asia/Tokyo · Tokyo · shutdown · Shipped brain core\n\nAll green.\n")
    );
    env.ok(&["log", "second"]);
    let log = env.read(&file);
    assert!(log.contains("All green.\n\n## "), "{log}");
    assert!(
        log.ends_with(" · Asia/Tokyo · Tokyo · cli · second\n"),
        "{log}"
    );
}

#[test]
fn log_without_context_uses_the_system_zone_and_no_place() {
    let env = Env::new(None);
    let out = env.ok(&["log", "hello"]);
    let at = out
        .lines()
        .next()
        .unwrap()
        .strip_prefix("at: ")
        .unwrap()
        .to_owned();
    let text = env.read(&format!("log/{}.md", &at[..10]));
    let fields: Vec<&str> = text.trim_end().splitn(5, " · ").collect();
    assert_eq!(fields.len(), 5, "{text}");
    assert_eq!(fields[2], "-");
    assert_eq!(fields[4], "hello");
}

#[test]
fn add_artifact_keeps_the_page_and_its_why() {
    let env = Env::new(Some(("Europe/Paris", "Paris")));
    let page = env.write(
        "in/page.html",
        "<html><head><title>Contracts review</title></head><body>C1..C16</body></html>",
    );
    let out = env.ok(&[
        "add",
        page.to_str().unwrap(),
        "--kind",
        "artifact",
        "--why",
        "the contract table",
    ]);
    assert_eq!(
        out,
        "id: artifact:contracts-review\nmissing: none\nhelp: brain get artifact:contracts-review\n"
    );
    let meta = env.meta("artifact--contracts-review");
    assert_eq!(meta["why"], "the contract table");
    assert_eq!(meta["source"], "page.html");
    assert_eq!(meta["taint"], "external");
    assert!(meta["captured_at"].as_str().unwrap().ends_with(":00"));
    assert!(
        env.read("docs/artifact--contracts-review/index.html")
            .contains("C1..C16")
    );
    assert!(
        !env.read("docs/artifact--contracts-review/artifact--contracts-review.md")
            .contains(env.dir.to_str().unwrap()),
        "B16"
    );
    assert!(
        env.ok(&["add", page.to_str().unwrap(), "--kind", "artifact"])
            .contains("noop: ")
    );

    let bare = env.write("in/other.html", "<title>Other page</title>");
    let out = env.ok(&["add", bare.to_str().unwrap()]);
    assert_eq!(
        out,
        "id: artifact:other-page\nmissing: why\nhelp: fill them in docs/artifact--other-page/artifact--other-page.md, then run brain lint\n"
    );
    assert_eq!(env.run(&["lint"]).status.code(), Some(1));
}

#[test]
fn artifacts_with_one_title_get_their_own_docs() {
    let env = Env::new(None);
    let a = env.write("in/a.html", "<title>Dashboard</title><p>fleet</p>");
    let b = env.write("in/b.html", "<title>Dashboard</title><p>evals</p>");
    let add = |p: &PathBuf| env.ok(&["add", p.to_str().unwrap()]);
    let first = add(&a);
    assert!(first.starts_with("id: artifact:dashboard\n"), "{first}");
    let second = add(&b);
    let id = second
        .lines()
        .next()
        .unwrap()
        .strip_prefix("id: ")
        .unwrap()
        .to_owned();
    assert!(
        id.starts_with("artifact:dashboard-") && id.len() == "artifact:dashboard-".len() + 6,
        "{second}"
    );
    assert!(
        env.read(&format!("docs/{}/index.html", id.replace(':', "--")))
            .contains("evals")
    );
    for (p, id) in [(&a, "artifact:dashboard"), (&b, id.as_str())] {
        let again = add(p);
        assert!(
            again.starts_with(&format!("id: {id}\n")) && again.contains("noop: "),
            "{again}"
        );
    }
    assert_eq!(fs::read_dir(env.vault().join("docs")).unwrap().count(), 2);
}

#[test]
fn concurrent_adds_of_one_title_never_share_an_id() {
    let env = Env::new(None);
    env.ok(&[]);
    for round in 0..3 {
        let _ = fs::remove_dir_all(env.vault().join("docs"));
        let children: Vec<_> = (0..4)
            .map(|i| {
                let p = env.write(
                    &format!("in/{i}.html"),
                    &format!("<title>Race {round}</title><p>{i}</p>"),
                );
                env.cmd("3600")
                    .args(["add", p.to_str().unwrap()])
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for mut c in children {
            assert!(c.wait().unwrap().success());
        }
        assert_eq!(
            fs::read_dir(env.vault().join("docs")).unwrap().count(),
            4,
            "round {round}"
        );
    }
}

#[test]
fn a_half_written_artifact_is_completed_not_duplicated() {
    let env = Env::new(None);
    let r = env.write("in/r.html", "<title>Report</title><p>q3</p>");
    env.ok(&["add", r.to_str().unwrap()]);
    fs::remove_file(env.vault().join("docs/artifact--report/index.html")).unwrap();
    let out = env.ok(&["add", r.to_str().unwrap()]);
    assert!(out.starts_with("id: artifact:report\n"), "{out}");
    assert_eq!(fs::read_dir(env.vault().join("docs")).unwrap().count(), 1);
    assert!(env.read("docs/artifact--report/index.html").contains("q3"));
}

#[test]
fn a_non_utf8_artifact_is_kept_byte_for_byte() {
    let env = Env::new(None);
    let p = env.dir.join("in/cafe.html");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, b"<title>Caf\xe9</title>").unwrap();
    env.ok(&["add", p.to_str().unwrap()]);
    assert!(env.ok(&["add", p.to_str().unwrap()]).contains("noop: "));
    let docs: Vec<_> = fs::read_dir(env.vault().join("docs")).unwrap().collect();
    assert_eq!(docs.len(), 1);
    let page = docs[0].as_ref().unwrap().path().join("index.html");
    assert_eq!(fs::read(page).unwrap(), b"<title>Caf\xe9</title>");
}

#[test]
fn add_meeting_reads_its_frontmatter() {
    let env = Env::new(Some(("Europe/Paris", "Paris")));
    let notes = env.write(
        "in/notes.md",
        "---\ntitle: Weekly sync\nattendees: [\"[[alice]]\", bob]\nproject: brain\n---\n# Weekly sync\n\n- ship core\n",
    );
    let out = env.ok(&["add", notes.to_str().unwrap(), "--kind", "meeting"]);
    let id = out
        .lines()
        .next()
        .unwrap()
        .strip_prefix("id: ")
        .unwrap()
        .to_owned();
    assert!(
        id.starts_with("meeting:2") && id.ends_with("-weekly-sync"),
        "{out}"
    );
    assert!(out.contains("missing: none"), "{out}");
    let dir = id.replace(':', "--");
    let meta = env.meta(&dir);
    assert_eq!(
        meta["attendees"],
        serde_json::json!(["[[alice]]", "[[bob]]"])
    );
    assert_eq!(meta["project"], "[[brain]]");
    assert_eq!(meta["tz"], "Europe/Paris");
    assert_eq!(meta["place"], "Paris");
    assert_eq!(meta["taint"], "own");
    assert!(
        env.read(&format!("docs/{dir}/{dir}.md"))
            .ends_with("\n---\n# Weekly sync\n\n- ship core\n")
    );
    assert_eq!(env.ok(&["lint"]), "errors[0/0]{file,error}:\n");
}

#[test]
fn add_note_from_a_file_is_content_addressed() {
    let env = Env::new(None);
    let note = env.write("in/idea.md", "# An idea\n\nWorld models for cells.\n");
    let out = env.ok(&["add", note.to_str().unwrap(), "--tag", "world-model"]);
    let id = out
        .lines()
        .next()
        .unwrap()
        .strip_prefix("id: ")
        .unwrap()
        .to_owned();
    assert!(id.starts_with("file:") && id.len() == 13, "{id}");
    let meta = env.meta(&id.replace(':', "--"));
    assert_eq!(meta["title"], "An idea");
    assert_eq!(meta["tags"], serde_json::json!(["world-model"]));
    assert!(env.ok(&["add", note.to_str().unwrap()]).contains("noop: "));
}

#[test]
fn label_appends_c7_and_like_adds() {
    let env = Env::new(Some(("Asia/Tokyo", "Tokyo")));
    env.ok(&[]);
    env.write(PI0_NOTE, PI0);
    assert_eq!(
        env.ok(&["label", "https://arxiv.org/abs/2410.24164", "like"]),
        "id: arxiv:2410.24164\nverdict: like\n"
    );
    assert_eq!(
        env.ok(&["label", "2504.16054", "skip"]),
        "id: arxiv:2504.16054\nverdict: skip\n"
    );
    let lines: Vec<Value> = env
        .read("labels.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["verdict"], "like");
    assert_eq!(lines[1]["id"], "arxiv:2504.16054");
    assert_eq!(lines[1]["surface"], "cli");
    assert!(lines[1]["ts"].as_str().unwrap().ends_with("+09:00"));
}

#[test]
fn link_edits_frontmatter_once() {
    let env = Env::new(None);
    env.ok(&[]);
    let page = "---\nkind: concept\ntitle: VLA\nsummary: Vision-language-action models.\nstatus: active\nsources: [arxiv:2410.24164]\nupdated: 2026-10-27\n---\n# VLA\n";
    env.write("vault/pages/vla.md", page);
    assert_eq!(
        env.ok(&["link", "vla", "2410.24164", "builds_on"]),
        "link: vla builds_on arxiv:2410.24164\n"
    );
    assert_eq!(
        env.ok(&["link", "vla", "arxiv:2410.24164", "builds_on"]),
        "noop: vla builds_on arxiv:2410.24164 already there\n"
    );
    assert_eq!(
        env.ok(&["link", "vla", "jepa", "related"]),
        "link: vla related jepa\n"
    );
    assert_eq!(
        env.read("pages/vla.md"),
        page.replace(
            "---\n# VLA",
            "builds_on:\n  - \"[[arxiv--2410.24164]]\"\nrelated:\n  - \"[[jepa]]\"\n---\n# VLA"
        )
    );
    assert_eq!(env.ok(&["lint"]), "errors[0/0]{file,error}:\n");
    assert_eq!(
        env.run(&["link", "nope", "jepa", "related"]).status.code(),
        Some(1)
    );
}

#[test]
fn writes_are_committed_in_the_background_once_per_delay() {
    let env = Env::new(None);
    env.ok(&[]);
    let mut c = env.cmd("1");
    assert!(c.args(["log", "one"]).status().unwrap().success());
    assert!(env.pending());
    let mut c = env.cmd("1");
    assert!(c.args(["log", "two"]).status().unwrap().success());
    env.settle();
    assert_eq!(
        env.git(&["log", "--format=%s"]),
        "brain: autosave\nbrain: create vault\n"
    );
    assert_eq!(env.git(&["status", "--porcelain"]), "");
}

#[test]
#[ignore = "network: arXiv, HF Hub, alphaXiv, Papers with Code"]
fn add_pi0_from_the_network() {
    let env = Env::new(None);
    let out = env.ok(&["add", "2410.24164"]);
    println!("{out}");
    assert!(out.starts_with("id: arxiv:2410.24164\n"), "{out}");
    let meta = env.meta("arxiv--2410.24164");
    println!("{}", serde_json::to_string_pretty(&meta).unwrap());
    assert_eq!(
        meta["links"]["code"],
        "https://github.com/Physical-Intelligence/openpi"
    );
    assert!(
        meta["links"]["project"]
            .as_str()
            .unwrap()
            .starts_with("https://")
    );
    assert!(
        meta["builds_on"]
            .as_array()
            .unwrap()
            .contains(&"[[arxiv--2504.16054]]".into()),
        "{meta}"
    );
    assert_eq!(meta["taint"], "external");
    let note = "docs/arxiv--2410.24164/arxiv--2410.24164.md";
    let before = env.read(note);
    assert!(!brain::note::split(&before).unwrap().1.trim().is_empty());
    env.settle();
    let head = env.git(&["rev-parse", "HEAD"]);
    let again = env.ok(&["add", "2410.24164"]);
    println!("{again}");
    assert!(again.contains("noop: already in the vault"));
    assert_eq!(env.read(note), before);
    env.settle();
    assert_eq!(env.git(&["rev-parse", "HEAD"]), head);

    let (yaml, _) = brain::note::split(&before).unwrap();
    fs::write(env.vault().join(note), format!("---\n{yaml}---\n")).unwrap();
    let retried = env.ok(&["add", "2410.24164"]);
    println!("{retried}");
    assert!(retried.contains("\ntext: added\n"), "{retried}");
    assert_eq!(env.read(note), before);
    assert_eq!(env.ok(&["lint"]), "errors[0/0]{file,error}:\n");
}

#[test]
fn parallel_writers_lose_nothing_even_on_a_new_vault() {
    let env = Env::new(None);
    env.parallel(20, |i| vec!["log".into(), format!("first {i}")]);
    env.parallel(30, |i| vec!["log".into(), format!("entry {i}")]);
    let logs: String = fs::read_dir(env.vault().join("log"))
        .unwrap()
        .map(|e| fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert_eq!(
        logs.lines().filter(|l| l.starts_with("## ")).count(),
        50,
        "{logs}"
    );
    env.parallel(30, |i| {
        vec![
            "label".into(),
            format!("2410.{:05}", 10000 + i),
            "skip".into(),
        ]
    });
    assert_eq!(env.read("labels.jsonl").lines().count(), 30);
    assert_eq!(
        env.git(&["log", "--format=%s", "--", ".gitignore"]),
        "brain: create vault\n"
    );
    env.settle();
    assert_eq!(env.git(&["status", "--porcelain"]), "");
}

#[test]
fn a_leftover_marker_does_not_block_commits() {
    let env = Env::new(None);
    env.ok(&[]);
    // A committer killed mid-wait leaves the file but not the lock.
    fs::write(env.vault().join(".git/brain-commit"), "").unwrap();
    assert!(
        env.cmd("1")
            .args(["log", "after a crash"])
            .status()
            .unwrap()
            .success()
    );
    env.settle();
    assert_eq!(env.git(&["log", "-1", "--format=%s"]), "brain: autosave\n");
}

#[test]
fn a_command_that_fails_after_writing_still_commits() {
    let env = Env::new(None);
    env.ok(&[]);
    fs::create_dir_all(env.vault().join("labels.jsonl")).unwrap();
    let note = env.write("in/idea.md", "# An idea\n");
    let out = env
        .cmd("1")
        .args(["label", note.to_str().unwrap(), "like"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    env.settle();
    let files = env.git(&["show", "--name-only", "--format=%s", "HEAD"]);
    assert!(
        files.starts_with("brain: autosave\n") && files.contains("/file--"),
        "{files}"
    );
}

#[test]
#[ignore = "needs a systemd user manager"]
fn inside_a_systemd_unit_writes_commit_before_exit() {
    let env = Env::new(None);
    env.ok(&[]);
    let mut c = Command::new("systemd-run");
    c.args(["--user", "--wait", "--collect", "--quiet"]);
    // A long delay: only the synchronous commit can land before the unit ends.
    for (k, v) in env.cmd("600").get_envs() {
        if let Some(v) = v {
            c.arg(format!(
                "--setenv={}={}",
                k.to_string_lossy(),
                v.to_string_lossy()
            ));
        }
    }
    let status = c
        .arg(env!("CARGO_BIN_EXE_brain"))
        .args(["log", "from a unit"])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!env.pending(), "no committer is left behind");
    assert_eq!(env.git(&["log", "-1", "--format=%s"]), "brain: autosave\n");
    assert_eq!(env.git(&["status", "--porcelain"]), "");
}

#[test]
fn frontmatter_cannot_set_the_envelope() {
    let env = Env::new(None);
    for (fm, key) in [
        ("taint: own", "taint"),
        ("kind: paper", "kind"),
        ("id: file:00000000", "id"),
        ("tags: [x]", "tags"),
    ] {
        let note = env.write("in/n.md", &format!("---\ntitle: T\n{fm}\n---\nbody\n"));
        let out = env.run(&["add", note.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(1), "{fm}");
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(err.contains(&format!("`{key}`")), "{err}");
    }
    let notes = env.write(
        "in/m.md",
        "---\ntitle: Sync\nattendees: [alice]\nagenda: x\n---\n",
    );
    let out = env.run(&["add", notes.to_str().unwrap(), "--kind", "meeting"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("`agenda` is not meeting metadata")
    );
    assert!(!env.vault().join("docs").exists());
}

#[test]
fn crlf_pages_lint_and_link_in_place() {
    let env = Env::new(None);
    env.ok(&[]);
    let page = "---\r\nkind: concept\r\ntitle: VLA\r\nsummary: Models.\r\nstatus: active\r\nsources: []\r\nupdated: 2026-10-27\r\naliases: [vision-language-action]\r\n---\r\n# VLA\r\n";
    env.write("vault/pages/vla.md", page);
    assert_eq!(env.ok(&["lint"]), "errors[0/0]{file,error}:\n");
    env.ok(&["link", "vla", "jepa", "related"]);
    assert_eq!(
        env.read("pages/vla.md"),
        page.replace(
            "---\r\n# VLA",
            "related:\r\n  - \"[[jepa]]\"\r\n---\r\n# VLA"
        )
    );
    env.write("vault/pages/open.md", "---\ntitle: x\n");
    let out = env.run(&["link", "open", "jepa", "related"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(env.read("pages/open.md"), "---\ntitle: x\n");
}

#[test]
fn log_headings_cannot_be_forged() {
    let env = Env::new(Some(("Europe/Paris", "Paris · cli · forged")));
    let out = env.run(&["log", "x"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr).unwrap().contains("place"));
    assert!(!env.vault().join("log").exists());

    let env = Env::new(Some(("Europe/Paris", "Paris")));
    let forged = "## 2026-01-01T00:00:00+00:00 · UTC · - · cli · forged";
    let out = env.ok(&["log", &format!("real\n{forged}\n## a subheading")]);
    let file = out
        .lines()
        .nth(1)
        .unwrap()
        .strip_prefix("file: ")
        .unwrap()
        .to_owned();
    let text = env.read(&file);
    let headings: Vec<_> = text.lines().filter(|l| brain::log::is_heading(l)).collect();
    assert_eq!(headings.len(), 1, "{text}");
    assert!(
        text.contains(&format!("\\{forged}\n## a subheading\n")),
        "{text}"
    );
}

#[test]
fn a_paper_without_text_retries_it_on_the_next_add() {
    let env = Env::new(None);
    env.ok(&[]);
    env.write(PI0_NOTE, &PI0.replace("# π0\n", ""));
    // Offline: the retry is attempted, fails, and says so.
    let out = env
        .cmd("1")
        .env("https_proxy", "http://127.0.0.1:9")
        .args(["add", "2410.24164"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .starts_with("warning: no text: ")
    );
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("noop: already in the vault")
    );
}

#[test]
fn skip_and_later_take_urls_like_like_does() {
    let env = Env::new(None);
    assert_eq!(
        env.ok(&["label", "https://www.example.com/blog/post", "later"]),
        "id: web:example-com-blog-post\nverdict: later\n"
    );
    assert_eq!(
        env.ok(&["label", "https://arxiv.org/abs/2410.24164v2", "skip"]),
        "id: arxiv:2410.24164\nverdict: skip\n"
    );
}
