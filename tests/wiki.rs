//! The wiki loop against throwaway vaults: propose, ls, diff, merge, drop, lint.

#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking"
)]

use std::fs;
use std::io::Write as _;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Env {
    dir: PathBuf,
    embed_url: String,
}

impl Env {
    /// A fresh home and vault holding two docs, with the embedder down.
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "brain-wiki-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let embed_url = format!("http://{}/v1/embeddings", l.local_addr().unwrap());
        drop(l);
        let env = Self { dir, embed_url };
        env.ok(&[]);
        for (id, added) in [
            ("arxiv:2410.24164", "2026-10-20T10:00:00+02:00"),
            ("arxiv:2504.16054", "2026-10-21T10:00:00+02:00"),
        ] {
            let dir = id.replace(':', "--");
            env.write(
                &format!("docs/{dir}/meta.json"),
                &json!({"id": id, "kind": "paper", "taint": "external", "added": added,
                        "title": format!("Paper {id}"), "authors": ["A"], "date": "2024-10-31", "links": {}})
                .to_string(),
            );
        }
        env
    }

    fn vault(&self) -> PathBuf {
        self.dir.join("vault")
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.vault().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_brain"));
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.dir)
            .env("TZ", "UTC")
            .env("XDG_CACHE_HOME", self.dir.join("cache"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("BRAIN_VAULT", self.vault())
            .env("BRAIN_EMBED_URL", &self.embed_url)
            .env("BRAIN_COMMIT_DELAY", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .args(args);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
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

    fn propose(&self, target: &str, reply: &str) -> Output {
        let mut child = self
            .cmd(&["wiki", "propose", target])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // brain stops reading at its cap, so a write past it may meet a closed pipe.
        let _ = child.stdin.take().unwrap().write_all(reply.as_bytes());
        child.wait_with_output().unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.vault())
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    fn queue(&self) -> Vec<String> {
        self.ok(&["--json", "wiki", "queue"])
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l)
                    .unwrap()
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    /// Waits for the background committer, which holds the marker or the vault lock.
    fn settle(&self) {
        let held = |p: PathBuf| {
            fs::File::open(p)
                .is_ok_and(|f| matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock)))
        };
        let start = Instant::now();
        while (held(self.vault().join(".git/brain-commit")) || held(self.vault()))
            && start.elapsed() < Duration::from_secs(15)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.settle();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn page(title: &str, sources: &str, body: &str) -> String {
    format!(
        "---\nkind: concept\ntitle: {title}\nsummary: {title} in one line\nstatus: active\nsources: [{sources}]\nupdated: 2026-10-27\n---\n# {title}\n\n{body}\n"
    )
}

const PI0: &str = "arxiv:2410.24164";
const PI05: &str = "arxiv:2504.16054";

#[test]
fn a_reply_becomes_a_branch_then_lands_reindexed_and_logged() {
    let env = Env::new();
    assert_eq!(env.queue(), [PI0, PI05]);
    env.ok(&["reindex"]);

    let reply = format!(
        "Pages follow.\n=== page: pi0\n{}",
        page(
            "Pi0",
            PI0,
            "Pi0 puts a flow matching action expert on a VLM backbone [@arxiv:2410.24164#1]."
        )
    );
    let out = env.propose(PI0, &reply);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "branch: wiki/arxiv--2410.24164\npages: pi0\n"
    );
    // The vault's checkout is untouched, and no worktree is left behind.
    assert!(!env.vault().join("pages/pi0.md").exists());
    assert_eq!(env.git(&["status", "--porcelain"]), "");
    assert_eq!(env.git(&["worktree", "list"]).lines().count(), 1);
    // An open proposal takes its doc out of the queue.
    assert_eq!(env.queue(), [PI05]);

    let ls = env.ok(&["wiki", "ls"]);
    assert!(
        ls.starts_with(
            "branches[1/1]{branch,doc,pages,created}:\n  wiki/arxiv--2410.24164,arxiv:2410.24164,pi0,20"
        ),
        "{ls}"
    );
    let diff = env.ok(&["wiki", "diff", "wiki/arxiv--2410.24164"]);
    assert!(diff.contains("+++ b/pages/pi0.md"), "{diff}");
    assert!(
        diff.contains("+Pi0 puts a flow matching action expert"),
        "{diff}"
    );

    assert_eq!(
        env.ok(&["wiki", "merge", "wiki/arxiv--2410.24164"]),
        "merged: wiki/arxiv--2410.24164\npages: pi0\n"
    );
    assert!(env.vault().join("pages/pi0.md").is_file());
    assert_eq!(env.git(&["branch", "--list", "wiki/*"]), "");
    let hits = env.ok(&["--json", "find", "flow matching action expert"]);
    assert!(
        hits.lines()
            .any(|l| serde_json::from_str::<Value>(l).unwrap()["id"] == "pi0"),
        "{hits}"
    );
    // The log block lands in the merge commit itself, with the page.
    let landed = env.git(&["diff", "--name-only", "HEAD^1", "HEAD"]);
    assert!(
        landed.contains("pages/pi0.md\n") && landed.contains("log/"),
        "{landed}"
    );
    assert_eq!(env.git(&["status", "--porcelain"]), "");
    let day = fs::read_dir(env.vault().join("log"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let block = fs::read_to_string(day.path()).unwrap();
    assert!(
        block.contains(
            " · compile · merged wiki/arxiv--2410.24164\n\ndocs: arxiv:2410.24164\npages: pi0\n"
        ),
        "{block}"
    );
    assert_eq!(env.queue(), [PI05]);
}

#[test]
fn of_two_proposals_on_one_page_the_second_conflicts_and_requeues() {
    let env = Env::new();
    env.write(
        "pages/pi0.md",
        &page("Pi0", PI0, "A VLA [@arxiv:2410.24164]."),
    );
    let first = page("Pi0", PI0, "A flow VLA [@arxiv:2410.24164].");
    let second = page(
        "Pi0",
        &format!("{PI0}, {PI05}"),
        "Superseded by pi0.5 [@arxiv:2504.16054].",
    );
    assert!(
        env.propose(PI0, &format!("=== page: pi0\n{first}"))
            .status
            .success()
    );
    assert!(
        env.propose(PI05, &format!("=== page: pi0\n{second}"))
            .status
            .success()
    );
    assert_eq!(env.queue(), Vec::<String>::new());

    env.ok(&["wiki", "merge", "wiki/arxiv--2410.24164"]);
    let out = env.run(&["wiki", "merge", "wiki/arxiv--2504.16054"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "merged: none\npages: pi0\nconflict: pages/pi0.md\nrequeued: arxiv:2504.16054\n"
    );
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .starts_with("error: wiki/arxiv--2504.16054 conflicts")
    );
    assert_eq!(env.git(&["branch", "--list", "wiki/*"]), "");
    assert_eq!(env.git(&["status", "--porcelain"]), "");
    assert!(!env.vault().join(".git/MERGE_HEAD").exists());
    assert_eq!(
        fs::read_to_string(env.vault().join("pages/pi0.md")).unwrap(),
        first
    );
    assert_eq!(env.queue(), [PI05]);
}

#[test]
fn an_uncited_paragraph_fails_lint_by_line_and_propose_commits_nothing() {
    let env = Env::new();
    let bad = page(
        "Pi0",
        PI0,
        "A flow VLA [@arxiv:2410.24164].\n\nIt folds laundry.",
    );
    env.write("pages/pi0.md", &bad);
    let out = env.run(&["lint"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "errors[1/1]{file,error}:\n  pages/pi0.md,line 13: uncited paragraph: cite a doc as [@<doc-id>]\n"
    );
    fs::remove_file(env.vault().join("pages/pi0.md")).unwrap();

    let out = env.propose(PI0, &format!("=== page: pi0\n{bad}"));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("pages/pi0.md,line 13: uncited paragraph"),
    );
    assert_eq!(env.git(&["branch", "--list", "wiki/*"]), "");
    assert_eq!(env.git(&["worktree", "list"]).lines().count(), 1);
    assert_eq!(env.queue(), [PI0, PI05]);
}

#[test]
fn wiki_lint_finds_dead_links_citations_and_unlisted_sources() {
    let env = Env::new();
    env.write(
        "docs/arxiv--2410.24164/text.md",
        "# Method\n\nA flow matching action expert.\n",
    );
    env.write(
        "pages/pi0.md",
        &page(
            "Pi0",
            PI0,
            "See [[nowhere]] [@arxiv:2410.24164].\n\n- From [@arxiv:2504.16054].\n- Dead [@arxiv:1111.11111].\n- No span [@arxiv:2410.24164#999].\n- A heading [@arxiv:2410.24164#method].",
        ),
    );
    let out = env.run(&["lint"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "errors[4/4]{file,error}:\n  pages/pi0.md,line 11: dead link [[nowhere]]\n  pages/pi0.md,line 13: arxiv:2504.16054 is cited but not in sources[]\n  pages/pi0.md,line 14: dead citation [@arxiv:1111.11111]\n  pages/pi0.md,line 15: dead citation [@arxiv:2410.24164#999]: no such span\n"
    );
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("warning: pages/pi0.md: orphan"), "{err}");

    env.write(
        "pages/old.md",
        "---\nkind: project\ntitle: Old\nsummary: Old\nstatus: active\nsources: []\nupdated: 2020-01-01\n---\nSee [[pi0]].\n",
    );
    let err = String::from_utf8(env.run(&["lint"]).stderr).unwrap();
    assert!(!err.contains("pages/pi0.md: orphan"), "{err}");
    assert!(err.contains("warning: pages/old.md: orphan"), "{err}");
    assert!(
        err.contains("warning: pages/old.md: active, but not updated since 2020-01-01"),
        "{err}"
    );
}

#[test]
fn a_dropped_proposal_puts_its_doc_back_in_the_queue() {
    let env = Env::new();
    let reply = format!(
        "=== page: pi0\n{}",
        page("Pi0", PI0, "A VLA [@arxiv:2410.24164].")
    );
    assert!(env.propose(PI0, &reply).status.success());
    assert_eq!(env.queue(), [PI05]);
    assert_eq!(
        env.ok(&["wiki", "drop", "wiki/arxiv--2410.24164"]),
        "dropped: wiki/arxiv--2410.24164\n"
    );
    assert_eq!(
        env.ok(&["wiki", "ls"]),
        "branches[0/0]{branch,doc,pages,created}:\n"
    );
    assert_eq!(env.queue(), [PI0, PI05]);
    assert!(!env.vault().join("pages/pi0.md").exists());
    assert_eq!(
        env.run(&["wiki", "drop", "wiki/arxiv--2410.24164"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn a_merged_learn_proposal_adds_its_question_to_the_eval_set() {
    let env = Env::new();
    // A pending autosave: propose commits it first instead of racing it.
    env.ok(&["log", "corrected pi0"]);
    let reply = format!(
        "=== page: pi0\n{}=== question: pi0\nWhat does pi0's action\nexpert predict?\n",
        page(
            "Pi0",
            PI0,
            "Its action expert predicts chunks [@arxiv:2410.24164]."
        )
    );
    let out = env.propose("learn", &reply);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let branch = String::from_utf8(out.stdout).unwrap();
    let branch = branch
        .lines()
        .next()
        .unwrap()
        .strip_prefix("branch: ")
        .unwrap()
        .to_owned();
    assert!(branch.starts_with("wiki/learn-20"), "{branch}");
    assert_eq!(env.git(&["status", "--porcelain"]), "");

    let out = env.ok(&["wiki", "merge", &branch]);
    assert!(out.ends_with("pages: pi0\nquestions: 1\n"), "{out}");
    env.settle();
    assert_eq!(
        fs::read_to_string(env.vault().join("eval/questions.jsonl")).unwrap(),
        "{\"q\":\"What does pi0's action expert predict?\",\"expect\":[\"pi0\"],\"from\":\"correction\"}\n"
    );
    // A question outside a learn proposal is refused.
    assert_eq!(env.propose(PI05, &reply).status.code(), Some(2));
}

const REPLY: &str = "=== page: pi0\n";

fn pi0_reply(body: &str) -> String {
    format!("{REPLY}{}", page("Pi0", &format!("{PI0}, {PI05}"), body))
}

#[test]
fn a_branch_that_touches_anything_but_pages_is_refused() {
    let env = Env::new();
    assert!(
        env.propose(PI0, &pi0_reply("A VLA [@arxiv:2410.24164]."))
            .status
            .success()
    );
    // Hand-edit the proposal: delete a doc, replace labels, ignore everything.
    env.git(&["checkout", "-q", "wiki/arxiv--2410.24164"]);
    env.git(&["rm", "-q", "docs/arxiv--2504.16054/meta.json"]);
    env.write(".gitignore", "*\n");
    env.git(&["commit", "-qam", "sneak"]);
    env.git(&["checkout", "-q", "-"]);

    let diff = env.ok(&["wiki", "diff", "wiki/arxiv--2410.24164"]);
    assert!(diff.contains("+++ b/.gitignore"), "{diff}");
    let out = env.run(&["wiki", "merge", "wiki/arxiv--2410.24164"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("outside pages/"), "{err}");
    assert!(
        env.vault()
            .join("docs/arxiv--2504.16054/meta.json")
            .is_file()
    );
    assert!(!env.vault().join("pages/pi0.md").exists());
    assert_eq!(env.git(&["status", "--porcelain"]), "");
}

#[test]
fn merged_pages_are_linted_again_against_the_vault_they_land_in() {
    let env = Env::new();
    assert!(
        env.propose(PI0, &pi0_reply("Then pi0.5 [@arxiv:2504.16054]."))
            .status
            .success()
    );
    // The cited doc goes away on the vault's branch before the merge.
    fs::remove_dir_all(env.vault().join("docs/arxiv--2504.16054")).unwrap();
    let out = env.run(&["wiki", "merge", "wiki/arxiv--2410.24164"]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("dead citation [@arxiv:2504.16054]"),
        "{stdout}"
    );
    assert!(!env.vault().join("pages/pi0.md").exists());
    assert!(env.ok(&["wiki", "ls"]).contains("wiki/arxiv--2410.24164"));
}

#[test]
fn a_reindex_failure_after_the_merge_is_a_warning_and_the_log_block_stays() {
    let env = Env::new();
    let db = env.dir.join("cache/brain/index.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, "not a database").unwrap();
    assert!(
        env.propose(PI0, &pi0_reply("A VLA [@arxiv:2410.24164]."))
            .status
            .success()
    );
    let out = env.run(&["wiki", "merge", "wiki/arxiv--2410.24164"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.starts_with("warning: reindex: "), "{err}");
    assert_eq!(env.queue(), [PI05]);
}

#[test]
fn a_worktree_left_by_a_killed_propose_is_cleaned_up() {
    let env = Env::new();
    env.settle();
    let leaked = env.dir.join("cache/brain/worktrees/wiki-1");
    env.git(&["add", "-A"]);
    env.git(&["commit", "-qm", "docs"]);
    env.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        leaked.to_str().unwrap(),
        "HEAD",
    ]);
    assert_eq!(env.git(&["worktree", "list"]).lines().count(), 2);
    assert!(
        env.propose(PI0, &pi0_reply("A VLA [@arxiv:2410.24164]."))
            .status
            .success()
    );
    assert_eq!(env.git(&["worktree", "list"]).lines().count(), 1);
    assert!(!leaked.exists());
}

#[test]
fn the_autosave_never_commits_an_open_merge() {
    let env = Env::new();
    env.settle();
    let head = env.git(&["rev-parse", "HEAD"]);
    fs::write(env.vault().join(".git/MERGE_HEAD"), head).unwrap();
    let out = env.propose(PI0, &pi0_reply("A VLA [@arxiv:2410.24164]."));
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("a git merge is in progress"), "{err}");
    assert_eq!(env.git(&["branch", "--list", "wiki/*"]), "");
}

#[test]
fn a_conflicting_learn_proposal_is_kept_with_its_question() {
    let env = Env::new();
    env.write(
        "pages/pi0.md",
        &page("Pi0", PI0, "A VLA [@arxiv:2410.24164]."),
    );
    assert!(
        env.propose(PI0, &pi0_reply("A flow VLA [@arxiv:2410.24164]."))
            .status
            .success()
    );
    let learn = format!(
        "{}=== question: pi0\nWhat is pi0?\n",
        pi0_reply("A VLA with an action expert [@arxiv:2410.24164].")
    );
    let out = env.propose("learn", &learn);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let branch = String::from_utf8(out.stdout).unwrap();
    let branch = branch
        .lines()
        .next()
        .unwrap()
        .strip_prefix("branch: ")
        .unwrap()
        .to_owned();

    env.ok(&["wiki", "merge", "wiki/arxiv--2410.24164"]);
    let out = env.run(&["wiki", "merge", &branch]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "merged: none\npages: pi0\nconflict: pages/pi0.md\n"
    );
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("it was kept"), "{err}");
    assert!(env.ok(&["wiki", "ls"]).contains(&branch));
    assert!(
        env.git(&["log", "-1", "--format=%B", &branch])
            .contains("Brain-Question: ")
    );
}

#[test]
fn only_a_wiki_merge_writes_a_compile_block() {
    let env = Env::new();
    let out = env.run(&["log", "--source", "compile", "x\ndocs: arxiv:2410.24164"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(env.queue(), [PI0, PI05]);
}

#[test]
fn a_bad_or_huge_reply_says_how_to_fix_the_reply() {
    let env = Env::new();
    let out = env.propose(PI0, "no blocks here");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.starts_with("error: bad reply on stdin: no `=== page: <slug>` block\nnext: send `=== page: <slug>` blocks"),
        "{err}"
    );
    let huge = format!("{REPLY}{}", "x".repeat(1 << 20));
    let err = String::from_utf8(env.propose(PI0, &huge).stderr).unwrap();
    assert!(
        err.starts_with("error: bad reply on stdin: over 1048576 bytes"),
        "{err}"
    );
}
