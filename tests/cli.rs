//! The binary against throwaway vaults. Nothing here touches ~/brain or the network,
//! except `add_pi0_from_the_network` (ignored by default).

#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
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

    fn meta(&self, dir: &str) -> Value {
        serde_json::from_str(&self.read(&format!("docs/{dir}/meta.json"))).unwrap()
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

    /// Waits for a pending background commit to land.
    fn settle(&self) {
        let start = Instant::now();
        let wait = |done: &dyn Fn() -> bool| {
            while !done() && start.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let marker = self.vault().join(".git/brain-commit-pending");
        if marker.exists() {
            wait(&|| !marker.exists());
            wait(&|| self.git(&["status", "--porcelain"]).is_empty());
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

const PI0: &str = r#"{
  "id": "arxiv:2410.24164",
  "kind": "paper",
  "taint": "external",
  "added": "2026-10-27T10:00:00+09:00",
  "title": "π0",
  "authors": ["Kevin Black"],
  "date": "2024-10-31",
  "links": {}
}
"#;

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
    env.write("vault/docs/arxiv--2410.24164/meta.json", PI0);
    env.write("vault/docs/arxiv--2410.24164/text.md", "# π0\n");
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
    assert_eq!(env.read("docs/arxiv--2410.24164/meta.json"), PI0);
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
    env.write(
        "vault/docs/arxiv--2410.24164/meta.json",
        &PI0.replace("  \"authors\": [\"Kevin Black\"],\n", ""),
    );
    env.write("vault/pages/jepa.md", "---\nkind: concept\ntitle: JEPA\nstatus: active\nsources: []\nupdated: 2026-10-27\n---\nbody\n");
    let out = env.run(&["lint"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "errors[2/2]{file,error}:\n  docs/arxiv--2410.24164/meta.json,missing required authors\n  pages/jepa.md,missing required summary\n"
    );
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.starts_with("error: 2 schema errors\nnext: "), "{err}");
    let json = env.run(&["lint", "--json"]);
    assert_eq!(
        String::from_utf8(json.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        r#"{"error":"missing required authors","file":"docs/arxiv--2410.24164/meta.json"}"#
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
    assert_eq!(meta["taint"], "own");
    assert!(meta["captured_at"].as_str().unwrap().ends_with(":00"));
    assert!(
        env.read("docs/artifact--contracts-review/index.html")
            .contains("C1..C16")
    );
    assert!(
        !env.read("docs/artifact--contracts-review/meta.json")
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
        "id: artifact:other-page\nmissing: why\nhelp: fill them in docs/artifact--other-page/meta.json, then run brain lint\n"
    );
    assert_eq!(env.run(&["lint"]).status.code(), Some(1));
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
    assert_eq!(meta["attendees"], serde_json::json!(["alice", "bob"]));
    assert_eq!(meta["project"], "brain");
    assert_eq!(meta["tz"], "Europe/Paris");
    assert_eq!(meta["place"], "Paris");
    assert_eq!(meta["taint"], "own");
    assert_eq!(
        env.read(&format!("docs/{dir}/text.md")),
        "# Weekly sync\n\n- ship core\n"
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
    env.write("vault/docs/arxiv--2410.24164/meta.json", PI0);
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
            "links:\n  - builds_on: arxiv:2410.24164\n  - related: jepa\n---\n# VLA"
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
    assert!(env.vault().join(".git/brain-commit-pending").exists());
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
    let pi05 = serde_json::json!({"src": "arxiv:2504.16054", "dst": "arxiv:2410.24164", "rel": "builds_on"});
    assert!(
        meta["lineage"].as_array().unwrap().contains(&pi05),
        "{meta}"
    );
    assert_eq!(meta["taint"], "external");
    assert!(env.vault().join("docs/arxiv--2410.24164/text.md").is_file());
    env.settle();
    let before = env.read("docs/arxiv--2410.24164/meta.json");
    let head = env.git(&["rev-parse", "HEAD"]);
    let again = env.ok(&["add", "2410.24164"]);
    println!("{again}");
    assert!(again.contains("noop: already in the vault"));
    assert_eq!(env.read("docs/arxiv--2410.24164/meta.json"), before);
    env.settle();
    assert_eq!(env.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(env.ok(&["lint"]), "errors[0/0]{file,error}:\n");
}
