//! tick, profile, eval and migrate against throwaway vaults and XDG dirs, with a fake
//! embedder on localhost. Nothing here touches ~/brain, the real state dir or the network.

#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking"
)]

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{Value, json};

/// Serves `/v1/embeddings` for the life of the test process: one fixed vector per text.
fn fake_embedder() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/embeddings", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if lower.starts_with("expect: 100-continue") {
                    stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let req: Value = serde_json::from_slice(&body).unwrap();
            let n = req.get("input").and_then(Value::as_array).unwrap().len();
            let data: Vec<Value> = (0..n)
                .map(|i| json!({"index": i, "embedding": [1.0, 0.0, 0.0, 0.0]}))
                .collect();
            let resp = json!({"data": data}).to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                resp.len()
            );
        }
    });
    url
}

/// A URL nothing listens on.
fn closed_port() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    format!("http://{addr}/v1/embeddings")
}

fn sh(cmd: &str, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = Command::new(cmd)
        .args(args)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A zone where it is about noon now, so cron times before it have passed all test long.
fn noon_zone() -> String {
    let h: i64 = sh("date", &["-u", "+%H"], &[]).parse().unwrap();
    let mut off = (12 - h).rem_euclid(24);
    if off > 14 {
        off -= 24;
    }
    match off {
        0 => "UTC".to_owned(),
        o if o > 0 => format!("Etc/GMT-{o}"),
        o => format!("Etc/GMT+{}", -o),
    }
}

fn today(tz: &str, ago: u32) -> String {
    sh(
        "date",
        &["-d", &format!("{ago} days ago"), "+%F"],
        &[("TZ", tz)],
    )
}

struct Env {
    dir: PathBuf,
    tz: String,
    embed_url: String,
    path: String,
}

impl Env {
    fn new(embed_url: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "brain-ops-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("state")).unwrap();
        let tz = noon_zone();
        let ctx = json!({"tz": tz, "updated_at": "2026-10-27T10:00:00+09:00", "source": "test"});
        fs::write(dir.join("state/context.json"), ctx.to_string()).unwrap();
        // No GPU unless a test says so: the host's own must not decide the result.
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("nvidia-smi"), "#!/bin/sh\nexit 1\n").unwrap();
        sh(
            "chmod",
            &["+x", bin.join("nvidia-smi").to_str().unwrap()],
            &[],
        );
        let env = Self {
            path: format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
            dir,
            tz,
            embed_url: embed_url.to_owned(),
        };
        fs::create_dir_all(env.vault()).unwrap();
        env.git(&["init", "-q"]);
        env.commit();
        env
    }

    fn vault(&self) -> PathBuf {
        self.dir.join("vault")
    }

    fn git(&self, args: &[&str]) -> String {
        sh(
            "git",
            &[&["-C", self.vault().to_str().unwrap()], args].concat(),
            &[
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_AUTHOR_NAME", "test"),
                ("GIT_AUTHOR_EMAIL", "test@example.com"),
                ("GIT_COMMITTER_NAME", "test"),
                ("GIT_COMMITTER_EMAIL", "test@example.com"),
            ],
        )
    }

    fn commit(&self) {
        self.git(&["add", "-A"]);
        self.git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "seed",
            "--allow-empty",
        ]);
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.vault().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn note(&self, name: &str, text: &str) {
        self.write(
            &format!("docs/file--{name}/meta.json"),
            &json!({"id": format!("file:{name}"), "kind": "note", "taint": "own",
                "added": "2026-10-21T09:00:00+02:00", "title": name})
            .to_string(),
        );
        self.write(&format!("docs/file--{name}/text.md"), text);
    }

    fn state(&self, file: &str) -> String {
        fs::read_to_string(self.dir.join("state/brain").join(file)).unwrap_or_default()
    }

    fn lines(&self, file: &str) -> Vec<Value> {
        self.state(file)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_brain"))
            .env_clear()
            .env("PATH", &self.path)
            .env("HOME", &self.dir)
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("BRAIN_VAULT", self.vault())
            .env("BRAIN_EMBED_URL", &self.embed_url)
            .env("BRAIN_COMMIT_DELAY", "0")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8(out.stdout).unwrap();
        self.settle();
        s
    }

    /// Waits for the background commit of a write to land.
    fn settle(&self) {
        let marker = self.vault().join(".git/brain-commit");
        for _ in 0..300 {
            let pending = fs::File::open(&marker)
                .is_ok_and(|f| matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock)));
            let busy = fs::File::open(self.vault())
                .is_ok_and(|f| matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock)));
            if !pending && !busy {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.settle();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn jobs(env: &Env) -> Vec<String> {
    env.lines("jobs.jsonl")
        .iter()
        .map(|l| l["job"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_tick_with_nothing_new_runs_no_job_and_writes_exactly_its_own_line() {
    let env = Env::new(&fake_embedder());
    env.note("aaaa0001", "# Alpha\n\nLeader arm latency.\n");
    env.commit();
    // The first ticks catch up: index, profile, vectors, and the commit the profile makes.
    for _ in 0..3 {
        env.ok(&["tick"]);
    }
    let before = jobs(&env);
    assert!(before.iter().any(|j| j == "reindex") && before.iter().any(|j| j == "embed"));
    assert!(before.iter().any(|j| j == "profile"));
    let out = env.ok(&["tick"]);
    assert!(out.starts_with("ran[0/0]{job,ok,took}:\n"), "{out}");
    let after = jobs(&env);
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after.last().map(String::as_str), Some("tick"));
    // C10: the exact keys, nothing else.
    let last = env.lines("jobs.jsonl").pop().unwrap();
    let mut keys: Vec<_> = last.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["ended", "err", "job", "ok", "started"]);
    assert_eq!(last["ok"], true);
}

#[test]
fn dry_run_lists_what_is_due_with_a_reason_and_runs_nothing() {
    let env = Env::new(&closed_port());
    env.note("aaaa0001", "# Alpha\n\nLeader arm latency.\n");
    env.commit();
    let out = env.ok(&["tick", "--dry-run"]);
    assert!(out.starts_with("due[2/2]{job,why}:\n"), "{out}");
    assert!(
        out.contains("profile,03:00 local has passed; last run never"),
        "{out}"
    );
    assert!(out.contains("reindex,no index yet"), "{out}");
    assert!(env.state("jobs.jsonl").is_empty() && env.state("tick.json").is_empty());
    assert!(!env.dir.join(".cache/brain/index.db").exists());
    assert!(!env.vault().join("profile.md").exists());
}

#[test]
fn model_jobs_and_other_components_jobs_are_listed_as_skipped() {
    let env = Env::new(&closed_port());
    for args in [&["tick"][..], &["tick", "--dry-run"]] {
        let out = env.ok(args);
        assert!(
            out.contains("skipped: needs drover v2: plan, review, learn, wiki-lint, compile\n"),
            "{out}"
        );
        assert!(
            out.contains("skipped: not in step 1: feed, shutdown, backup, ranker, news queries\n"),
            "{out}"
        );
    }
}

#[test]
fn a_cron_job_runs_once_per_local_day_in_the_zone_of_context_json() {
    let env = Env::new(&closed_port());
    env.ok(&["tick"]);
    env.ok(&["tick"]);
    let ran: Vec<_> = jobs(&env).into_iter().filter(|j| j == "profile").collect();
    assert_eq!(ran.len(), 1);
    let state: Value = serde_json::from_str(&env.state("tick.json")).unwrap();
    assert_eq!(state["days"]["profile"], today(&env.tz, 0).as_str());
    // The line is stamped with that zone's offset, not the system's.
    let started = env.lines("jobs.jsonl")[0]["started"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(started.get(..10), Some(today(&env.tz, 0).as_str()));
    // A run recorded for another day is due again: the day is what counts.
    fs::write(
        env.dir.join("state/brain/tick.json"),
        json!({"version": 1, "days": {"profile": "2000-01-01"}}).to_string(),
    )
    .unwrap();
    env.ok(&["tick"]);
    assert_eq!(jobs(&env).iter().filter(|j| *j == "profile").count(), 2);
}

#[test]
fn a_busy_gpu_skips_the_embed_job_and_logs_it() {
    let mut env = Env::new(&closed_port());
    env.note("aaaa0001", "# Alpha\n\nLeader arm latency.\n");
    env.commit();
    let bin = env.dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let smi = bin.join("nvidia-smi");
    fs::write(&smi, "#!/bin/sh\necho '999999, 20000'\n").unwrap();
    sh("chmod", &["+x", smi.to_str().unwrap()], &[]);
    env.path = format!("{}:{}", bin.display(), env.path);
    let out = env.ok(&["tick"]);
    assert!(out.contains("skipped: gpu busy: embed\n"), "{out}");
    let line = env
        .lines("jobs.jsonl")
        .into_iter()
        .find(|l| l["job"] == "embed")
        .unwrap();
    assert_eq!(
        (line["ok"].clone(), line["err"].clone()),
        (json!(true), json!("skipped: gpu busy"))
    );
    // Still due: it runs when the GPU frees up.
    assert!(env.ok(&["tick", "--dry-run"]).contains("embed,"));
}

fn lines_of(env: &Env, job: &str) -> Vec<Value> {
    env.lines("jobs.jsonl")
        .into_iter()
        .filter(|l| l["job"] == job)
        .collect()
}

#[test]
fn a_down_embedder_is_a_skip_not_a_failure() {
    let env = Env::new(&closed_port());
    env.note("aaaa0001", "# Alpha\n\nLeader arm latency.\n");
    env.commit();
    let out = env.run(&["tick"]);
    env.settle();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("skipped: embedder down: embed\n"));
    let embed = &lines_of(&env, "embed")[0];
    assert_eq!(
        (embed["ok"].clone(), embed["err"].clone()),
        (json!(true), json!("skipped: embedder down"))
    );
    assert!(
        env.ok(&["tick", "--dry-run"])
            .contains("embed,1 chunks without vectors")
    );
}

#[test]
fn a_failing_job_fails_the_tick_once_and_waits_for_its_inputs_to_change() {
    let env = Env::new(&closed_port());
    env.note("aaaa0001", "# Alpha\n\nLeader arm latency.\n");
    env.write("eval/questions.jsonl", "not json\n");
    env.commit();
    let out = env.run(&["tick"]);
    env.settle();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("error: eval:"));
    let eval = &lines_of(&env, "eval")[0];
    assert_eq!(eval["ok"], false);
    let last = env.lines("jobs.jsonl").pop().unwrap();
    assert_eq!(
        (last["job"].clone(), last["ok"].clone()),
        (json!("tick"), json!(false))
    );
    // Same inputs: no second try, and the tick is green.
    let out = env.run(&["tick"]);
    env.settle();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(lines_of(&env, "eval").len(), 1);
    assert!(env.ok(&["tick", "--dry-run"]).contains("eval,"));
    // A fixed questions file is new inputs.
    env.write(
        "eval/questions.jsonl",
        &format!("{}\n", json!({"q": "latency", "expect": ["file:aaaa0001"]})),
    );
    env.commit();
    env.ok(&["tick"]);
    assert_eq!(lines_of(&env, "eval").len(), 2);
    assert_eq!(lines_of(&env, "eval")[1]["ok"], true);
}

#[test]
fn one_jobs_error_does_not_stop_the_jobs_after_it() {
    let mut env = Env::new(&closed_port());
    env.note("aaaa0001", "# Alpha\n\nLeader arm latency.\n");
    env.commit();
    env.ok(&["reindex"]);
    env.embed_url = fake_embedder();
    // reindex and the repos due test both read this file; embed does not.
    env.write("repos.toml", "=== not toml\n");
    env.commit();
    let out = env.run(&["tick"]);
    env.settle();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(lines_of(&env, "reindex")[0]["ok"], false);
    assert_eq!(lines_of(&env, "repos")[0]["ok"], false);
    assert_eq!(lines_of(&env, "embed")[0]["ok"], true);
    assert!(String::from_utf8_lossy(&out.stdout).contains("embed,true,"));
}

#[test]
fn eval_waits_for_an_index_with_docs() {
    let env = Env::new(&closed_port());
    env.write(
        "eval/questions.jsonl",
        &format!("{}\n", json!({"q": "x", "expect": ["file:aaaa0001"]})),
    );
    env.commit();
    let out = env.run(&["tick"]);
    env.settle();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(lines_of(&env, "eval").is_empty());
}

#[test]
fn a_degraded_eval_is_redone_once_the_vectors_are_back() {
    let mut env = Env::new(&closed_port());
    env.note(
        "aaaa0001",
        "# Alpha\n\nLeader arm latency over the bridge.\n",
    );
    env.write(
        "eval/questions.jsonl",
        &format!(
            "{}\n",
            json!({"q": "leader arm latency", "expect": ["file:aaaa0001"]})
        ),
    );
    env.commit();
    env.run(&["tick"]);
    env.settle();
    assert_eq!(env.lines("eval.jsonl")[0]["degraded"], "no vectors");
    env.embed_url = fake_embedder();
    let out = env.ok(&["tick"]);
    assert!(
        out.contains("embed,true,") && out.contains("eval,true,"),
        "{out}"
    );
    let runs = env.lines("eval.jsonl");
    assert_eq!(runs.len(), 2);
    assert!(runs[1]["degraded"].is_null());
    assert!(!env.ok(&["tick", "--dry-run"]).contains("eval,"));
}

#[test]
fn a_second_tick_while_one_runs_says_so_and_exits_0() {
    let env = Env::new(&closed_port());
    fs::create_dir_all(env.dir.join("state/brain")).unwrap();
    let held = fs::File::create(env.dir.join("state/brain/tick.lock")).unwrap();
    held.lock().unwrap();
    let out = env.run(&["tick"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ran[0/0]{job,ok,took}:\nskipped: tick running\n"
    );
    let lines = env.lines("jobs.jsonl");
    assert_eq!(lines.len(), 1);
    assert_eq!(
        (
            lines[0]["job"].clone(),
            lines[0]["ok"].clone(),
            lines[0]["err"].clone()
        ),
        (json!("tick"), json!(true), json!("skipped: tick running"))
    );
    assert!(!env.vault().join("profile.md").exists());
    held.unlock().unwrap();
    assert!(env.ok(&["tick"]).starts_with("ran["));
}

/// Fake `nvidia-smi` and `ss` first on PATH: ollama's 6380 MiB under a pid of this test,
/// its listener visible only as a uid, as it is for a service user without root.
fn gpu_env(env: &mut Env, listener_uid: &str) {
    let bin = env.dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = |name: &str, body: String| {
        let p = bin.join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        sh("chmod", &["+x", p.to_str().unwrap()], &[]);
    };
    script("nvidia-smi", format!("echo '{}, 6380'", std::process::id()));
    script(
        "ss",
        format!("echo 'LISTEN 0 4096 *:11434 *:* uid:{listener_uid} ino:1 sk:2'"),
    );
    env.path = format!("{}:{}", bin.display(), env.path);
}

#[test]
fn the_embedder_servers_own_gpu_memory_is_not_a_busy_gpu() {
    let mut env = Env::new(&fake_embedder());
    env.note("aaaa0001", "# Alpha\n\nLeader arm latency.\n");
    env.commit();
    // The memory is held by a process of the listener's user: the server's.
    gpu_env(&mut env, &sh("id", &["-u"], &[]));
    let out = env.ok(&["tick"]);
    assert!(out.contains("embed,true,"), "{out}");
    assert!(!out.contains("gpu busy"), "{out}");
}

#[test]
fn memory_of_another_user_than_the_listener_is_a_busy_gpu() {
    let mut env = Env::new(&fake_embedder());
    env.note("aaaa0001", "# Alpha\n\nLeader arm latency.\n");
    env.commit();
    gpu_env(&mut env, "1");
    let out = env.ok(&["tick"]);
    assert!(out.contains("skipped: gpu busy: embed\n"), "{out}");
}

#[test]
fn embed_stops_at_its_budget_and_the_next_call_continues() {
    let env = Env::new(&fake_embedder());
    for i in 0..70 {
        env.note(
            &format!("{i:08x}"),
            &format!("# Note {i}\n\nWords about topic {i}.\n"),
        );
    }
    env.commit();
    let embedder = brain::embed::Embedder {
        url: env.embed_url.clone(),
        model: "m".to_owned(),
        batch_secs: 10,
    };
    let db = env.dir.join("idx/index.db");
    let vault = brain::Vault::open(&env.vault()).unwrap();
    let c = brain::index::reindex_text(&vault, &db, &embedder).unwrap();
    assert!(c.chunks >= 70);
    let (n, err) = brain::index::embed(&db, &embedder, std::time::Duration::ZERO).unwrap();
    assert!(err.is_none());
    assert!(n > 0 && n < c.chunks, "{n} of {}", c.chunks);
    let left = brain::index::unembedded(&db, "m").unwrap();
    assert_eq!(left, c.chunks - n);
    let (rest, _) =
        brain::index::embed(&db, &embedder, std::time::Duration::from_secs(60)).unwrap();
    assert_eq!(rest, left);
    assert_eq!(brain::index::unembedded(&db, "m").unwrap(), 0);
}

#[test]
fn concurrent_reindexes_do_not_collide_on_the_index() {
    let env = Env::new(&closed_port());
    for i in 0..40 {
        env.note(
            &format!("{i:08x}"),
            &format!("# Note {i}\n\nWords about topic {i}.\n"),
        );
    }
    env.commit();
    let embedder = brain::embed::Embedder {
        url: env.embed_url.clone(),
        model: "m".to_owned(),
        batch_secs: 1,
    };
    let db = env.dir.join("idx/index.db");
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    let vault = brain::Vault::open(&env.vault()).unwrap();
                    brain::index::reindex_text(&vault, &db, &embedder)
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
    });
    assert!(brain::index::built(&db));
}

fn tokens(s: &str) -> usize {
    brain::tokens::count(s)
}

#[test]
fn the_profile_has_its_five_parts() {
    let env = Env::new(&closed_port());
    let (new, old) = (today(&env.tz, 3), today(&env.tz, 40));
    let (due, fresh) = (today(&env.tz, 8), today(&env.tz, 1));
    env.write("pages/me.md", "---\nkind: person\ntitle: Me\nsummary: me\nstatus: active\nsources: []\nupdated: 2026-01-01\n---\n# Me\n\nShip brain tick.\nRead the JEPA papers.\nSleep.\nA fourth line.\n");
    env.write("pages/brain.md", &format!("---\nkind: project\ntitle: Brain\nsummary: the knowledge layer\nstatus: active\nsources: []\nupdated: {new}\n---\nx\n"));
    env.write("pages/old.md", &format!("---\nkind: project\ntitle: Old\nsummary: dormant\nstatus: active\nsources: []\nupdated: {old}\n---\nx\n"));
    env.write(
        &format!("log/{}.md", today(&env.tz, 0)),
        &format!("## {}T09:00:00+00:00 · UTC · - · plan · Plan\n\n- ship tick\n- [x] done already\n- [ ] review PR\n", today(&env.tz, 0)),
    );
    env.write(
        "labels.jsonl",
        &format!(
            "{}\n{}\n{}\n",
            json!({"id": "arxiv:2410.24164", "verdict": "later", "ts": format!("{due}T10:00:00+00:00"), "surface": "cli"}),
            json!({"id": "arxiv:2501.00001", "verdict": "later", "ts": format!("{fresh}T10:00:00+00:00"), "surface": "cli"}),
            json!({"id": "arxiv:2410.24164", "verdict": "later", "ts": format!("{due}T11:00:00+00:00"), "surface": "cli"}),
        ),
    );
    let out = env.ok(&["profile"]);
    assert!(out.starts_with("profile: profile.md\ntokens: "), "{out}");
    assert!(out.contains("drover needs: none"), "{out}");
    let text = fs::read_to_string(env.vault().join("profile.md")).unwrap();
    assert_eq!(
        text,
        "focus:\n- Ship brain tick.\n- Read the JEPA papers.\n- Sleep.\n\
         active projects:\n- brain: the knowledge layer\n\
         today's plan:\n- ship tick\n- review PR\n\
         needs you:\n- later, overdue: arxiv:2410.24164\n\
         ask brain before guessing: `brain find`\n"
    );
    assert_eq!(env.ok(&[]), text);
}

#[test]
fn the_profile_is_capped_at_400_tokens_on_oversized_input() {
    let env = Env::new(&closed_port());
    let big = "word ".repeat(400);
    env.write("pages/me.md", &format!("---\nkind: person\ntitle: Me\nsummary: me\nstatus: x\nsources: []\nupdated: 2026-01-01\n---\n{big}\n{big}\n{big}\n"));
    let day = today(&env.tz, 0);
    for i in 0..60 {
        env.write(
            &format!("pages/p{i}.md"),
            &format!("---\nkind: project\ntitle: P\nsummary: {big}\nstatus: active\nsources: []\nupdated: {day}\n---\nx\n"),
        );
    }
    let items: String = (0..80)
        .map(|i| format!("- item number {i} of the plan\n"))
        .collect();
    env.write(
        &format!("log/{day}.md"),
        &format!("## {day}T09:00:00+00:00 · UTC · - · plan · Plan\n\n{items}"),
    );
    env.ok(&["profile"]);
    let text = fs::read_to_string(env.vault().join("profile.md")).unwrap();
    assert!(tokens(&text) <= 400, "{} tokens", tokens(&text));
    assert!(text.len() <= 1600, "{} chars", text.len());
    assert!(text.starts_with("focus:\n"));
    assert!(text.ends_with("ask brain before guessing: `brain find`\n"));
    let json = env.ok(&["--json"]);
    assert!(json.len() < 1700);
}

#[test]
fn eval_prints_recall_and_tokens_and_appends_to_its_own_history() {
    let env = Env::new(&closed_port());
    env.note(
        "aaaa0001",
        "# Alpha\n\nThe leader arm latency over the camera bridge was forty milliseconds.\n",
    );
    env.note(
        "bbbb0002",
        "# Beta\n\nGrasp planning with suction cups on the packing line.\n",
    );
    env.commit();
    env.ok(&["reindex"]);
    env.write(
        "eval/questions.jsonl",
        &format!(
            "{}\n{}\n{}\n",
            json!({"q": "leader arm latency", "expect": ["file:aaaa0001"]}),
            json!({"q": "suction cups grasp", "expect": ["file:bbbb0002"]}),
            json!({"q": "latency", "expect": ["file:bbbb0002"]}),
        ),
    );
    env.commit();
    let out = env.ok(&["eval"]);
    assert!(
        out.starts_with("recall@8: 0.667\ntokens_per_hit: "),
        "{out}"
    );
    assert!(
        out.contains("questions: 3\ndegraded: no vectors\nvs_last: no earlier run\n"),
        "{out}"
    );
    let runs = env.lines("eval.jsonl");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["version"], 1);
    assert_eq!(runs[0]["vault"], env.git(&["rev-parse", "HEAD"]).as_str());
    assert!(runs[0]["brain"].as_str().is_some_and(|b| !b.is_empty()));
    // A better question set moves the score, and the diff says by how much.
    env.write(
        "eval/questions.jsonl",
        &format!(
            "{}\n",
            json!({"q": "leader arm latency", "expect": ["file:aaaa0001"]})
        ),
    );
    env.commit();
    let out = env.ok(&["eval"]);
    assert!(out.contains("recall@8: 1.000\n"), "{out}");
    assert!(out.contains("vs_last: recall +0.333, tokens "), "{out}");
    assert_eq!(env.lines("eval.jsonl").len(), 2);
    assert_eq!(env.state("queries.jsonl"), "");
    // Eval is due again only when the questions or the build change.
    // (the embedder is down here, so the tick itself fails on embed.)
    env.run(&["tick"]);
    env.settle();
    assert!(!env.ok(&["tick", "--dry-run"]).contains("eval,"));
}

#[test]
fn eval_without_questions_says_what_to_write() {
    let env = Env::new(&closed_port());
    env.note("aaaa0001", "# Alpha\n");
    env.commit();
    env.ok(&["reindex"]);
    let out = env.run(&["eval"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("eval/questions.jsonl"));
}

#[test]
fn migrate_prints_the_checklist_and_changes_nothing() {
    let env = Env::new(&closed_port());
    fs::remove_dir_all(env.vault()).unwrap();
    let out = env.ok(&["migrate"]);
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows[0], "steps[5/5]{n,command,status,detail}:");
    let cmds = [
        "restic restore",
        "dot",
        "brain reindex --full",
        "systemctl --user enable --now brain-tick.timer",
        "brain eval",
    ];
    for (i, c) in cmds.iter().enumerate() {
        assert!(
            rows[i + 1].starts_with(&format!("  {},{c},todo,", i + 1)),
            "{out}"
        );
    }
    assert!(!env.vault().exists() && !env.dir.join(".cache").exists());
    assert!(!env.dir.join("state/brain").exists());
}

#[test]
fn a_full_catch_up_makes_find_whole_again() {
    let env = Env::new(&fake_embedder());
    env.note(
        "aaaa0001",
        "# Alpha\n\nLeader arm latency over the bridge.\n",
    );
    env.commit();
    env.ok(&["tick"]);
    let out = env.ok(&["find", "latency"]);
    assert!(!out.contains("degraded"), "{out}");
    assert!(Path::new(&env.dir.join(".cache/brain/index.db")).is_file());
}

#[test]
fn the_skill_body_fits_its_600_token_budget() {
    let skill =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("skill/SKILL.md")).unwrap();
    let body = skill.splitn(3, "---").nth(2).unwrap();
    assert!(tokens(body) <= 600, "{} tokens", tokens(body));
}
