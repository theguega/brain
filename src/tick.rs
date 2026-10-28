//! B20: `brain tick`, run every 15 minutes by one systemd timer, is the only scheduler.
//! Each job has a kind (cron, lazy, event) and a cheap `due` test; only due jobs run.
//!
//! This step runs the jobs of this crate. Model jobs wait for drover v2 and the jobs of
//! other components for their step; both are listed as skipped, never run.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::embed::Embedder;
use crate::error::{Error, Result};
use crate::index::{self, Index};
use crate::time::{self, Now};
use crate::vault::{Vault, git, read_json, write_atomic};
use crate::{eval, gpu, profile, repos, state};

const STATE: &str = "tick.json";
const JOBS: &str = "jobs.jsonl";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// At this local `HH:MM`, once per local day.
    Cron(&'static str),
    Lazy,
    /// Runs inside the command that causes it, never from the tick.
    Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    Here,
    /// A reader run through drover v2.
    NeedsDrover,
    /// Owned by feed, the ranker, shutdown or backup.
    Elsewhere,
}

pub struct Job {
    pub name: &'static str,
    pub kind: Kind,
    pub run: Run,
}

const fn job(name: &'static str, kind: Kind, run: Run) -> Job {
    Job { name, kind, run }
}

/// The DESIGN job table, in the order the tick runs what it owns: reindex and repos before
/// embed, so one tick embeds the chunks they add.
pub const TABLE: &[Job] = &[
    job("feed", Kind::Cron("06:00"), Run::Elsewhere),
    job("plan", Kind::Cron("07:30"), Run::NeedsDrover),
    job("shutdown", Kind::Cron("18:30"), Run::Elsewhere),
    job("profile", Kind::Cron("03:00"), Run::Here),
    job("review", Kind::Cron("18:00"), Run::NeedsDrover),
    job("learn", Kind::Cron("17:00"), Run::NeedsDrover),
    job("wiki-lint", Kind::Cron("17:30"), Run::NeedsDrover),
    job("backup", Kind::Cron("02:00"), Run::Elsewhere),
    job("reindex", Kind::Lazy, Run::Here),
    job("repos", Kind::Lazy, Run::Here),
    job("embed", Kind::Lazy, Run::Here),
    job("compile", Kind::Lazy, Run::NeedsDrover),
    job("ranker", Kind::Lazy, Run::Elsewhere),
    job("news queries", Kind::Lazy, Run::Elsewhere),
    job("eval", Kind::Lazy, Run::Here),
    job("like", Kind::Event, Run::Elsewhere),
    job("merge", Kind::Event, Run::Elsewhere),
    job("run done", Kind::Event, Run::Elsewhere),
];

/// Last runs, kept next to the other machine state, never in the vault.
#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    indexed_head: Option<String>,
    /// Cron job to the local day it last ran.
    #[serde(default)]
    days: BTreeMap<String, String>,
}

impl State {
    fn path() -> Result<PathBuf> {
        Ok(state::dir()?.join(STATE))
    }

    fn load() -> Result<Self> {
        let p = Self::path()?;
        if p.is_file() {
            read_json(&p)
        } else {
            Ok(Self::default())
        }
    }

    fn save(&self) -> Result<()> {
        let p = Self::path()?;
        if let Some(d) = p.parent() {
            crate::error::io(std::fs::create_dir_all(d), d)?;
        }
        let mut s = serde_json::to_string(
            &json!({"version": 1, "indexed_head": self.indexed_head, "days": self.days}),
        )
        .map_err(|source| Error::Json {
            path: p.clone(),
            source,
        })?;
        s.push('\n');
        write_atomic(&p, &s)
    }
}

struct Ctx<'a> {
    vault: &'a Vault,
    db: PathBuf,
    embedder: Embedder,
    now: Now,
    head: String,
}

impl Ctx<'_> {
    /// Why `job` is due, or `None`.
    fn due(&self, job: &Job, st: &State) -> Result<Option<String>> {
        let name = job.name;
        Ok(match (name, job.kind) {
            (_, Kind::Cron(at)) => {
                let last = st.days.get(name).map(String::as_str);
                cron_due(&self.now, at, last).then(|| {
                    format!(
                        "{at} local has passed; last run {}",
                        last.unwrap_or("never")
                    )
                })
            }
            ("reindex", _) => {
                if !self.db.is_file() {
                    Some("no index yet".to_owned())
                } else {
                    let last = st.indexed_head.as_deref();
                    (last != Some(self.head.as_str())).then(|| {
                        format!(
                            "vault HEAD moved: {} -> {}",
                            short(last.unwrap_or("none")),
                            short(&self.head)
                        )
                    })
                }
            }
            ("repos", _) => {
                let moved = self.moved_repos()?;
                (!moved.is_empty()).then(|| format!("HEAD moved in {}", moved.join(", ")))
            }
            ("embed", _) => {
                let n = index::unembedded(&self.db, &self.embedder.model)?;
                (n > 0).then(|| format!("{n} chunks without vectors"))
            }
            ("eval", _) => eval::due(self.vault)?
                .then(|| "retrieval code or eval/questions.jsonl changed".to_owned()),
            _ => None,
        })
    }

    /// Registered repos whose HEAD is not the one the index holds.
    fn moved_repos(&self) -> Result<Vec<String>> {
        let Ok(ix) = Index::open_existing(&self.db) else {
            return Ok(Vec::new());
        };
        let mut moved = Vec::new();
        for r in repos::discover(&repos::Config::load(self.vault.root())?)? {
            let Ok(head) = git(&r.path, &["rev-parse", "HEAD"]) else {
                continue;
            };
            let known: Option<String> = ix
                .conn
                .query_row("SELECT head FROM repos WHERE name = ?1", [&r.name], |row| {
                    row.get(0)
                })
                .optional()?;
            if known.as_deref() != Some(head.trim()) {
                moved.push(r.name);
            }
        }
        Ok(moved)
    }

    fn exec(&self, name: &str, st: &mut State) -> Result<Done> {
        match name {
            "reindex" | "repos" => {
                let c = index::reindex_text(self.vault, &self.db, &self.embedder)?;
                for w in &c.warnings {
                    eprintln!("warning: {w}");
                }
                st.indexed_head = Some(self.head.clone());
            }
            "embed" => {
                if gpu::busy(&self.embedder.url) {
                    return Ok(Done::Skipped("gpu busy"));
                }
                nice();
                if let (_, Some(e)) = index::embed(&self.db, &self.embedder)? {
                    return Err(e);
                }
            }
            "profile" => {
                profile::write(self.vault, &self.now)?;
                st.days.insert(name.to_owned(), self.now.day().to_owned());
            }
            "eval" => {
                eval::run(self.vault, &self.embedder)?;
            }
            other => return Err(Error::Usage(format!("{other} is not a job of this step"))),
        }
        Ok(Done::Ran)
    }
}

/// Past `at` on the local clock, and not yet run on this local day. The day comes from the
/// zone in C15, so a flight that changes the date makes the job due once more.
fn cron_due(now: &Now, at: &str, last_day: Option<&str>) -> bool {
    now.at.get(11..16).unwrap_or_default() >= at && last_day != Some(now.day())
}

/// S6: the embed job runs at `Nice=19`, set in-process so `brain embed`-like work needs no unit setting.
fn nice() {
    let ok = Command::new("renice")
        .args(["-n", "19", "-p", &std::process::id().to_string()])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("warning: could not renice to 19");
    }
}

fn short(head: &str) -> &str {
    head.get(..7).unwrap_or(head)
}

enum Done {
    Ran,
    Skipped(&'static str),
}

/// A job that ran.
#[derive(Debug)]
pub struct Ran {
    pub job: &'static str,
    pub took: Duration,
    pub err: Option<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub ran: Vec<Ran>,
    /// `(why, jobs)`: model jobs, other components' jobs, the GPU.
    pub skipped: Vec<(&'static str, Vec<&'static str>)>,
    pub due: Vec<(&'static str, String)>,
}

impl Report {
    #[must_use]
    pub fn failed(&self) -> bool {
        self.ran.iter().any(|r| r.err.is_some())
    }
}

fn not_ours() -> Vec<(&'static str, Vec<&'static str>)> {
    let named = |run| {
        TABLE
            .iter()
            .filter(|j| j.run == run && j.kind != Kind::Event)
            .map(|j| j.name)
            .collect::<Vec<_>>()
    };
    vec![
        ("needs drover v2", named(Run::NeedsDrover)),
        ("not in step 1", named(Run::Elsewhere)),
    ]
}

fn context(vault: &Vault) -> Result<Ctx<'_>> {
    Ok(Ctx {
        vault,
        db: index::db_path()?,
        embedder: Embedder::from_env(),
        now: time::now()?,
        head: git(vault.root(), &["rev-parse", "HEAD"])?.trim().to_owned(),
    })
}

/// What is due and why, running nothing and writing nothing.
pub fn dry_run(vault: &Vault) -> Result<Report> {
    let (ctx, st) = (context(vault)?, State::load()?);
    let mut report = Report {
        skipped: not_ours(),
        ..Report::default()
    };
    for j in TABLE.iter().filter(|j| j.run == Run::Here) {
        if let Some(why) = ctx.due(j, &st)? {
            report.due.push((j.name, why));
        }
    }
    Ok(report)
}

/// Runs every due job once, then appends the tick's own C10 line.
pub fn run(vault: &Vault) -> Result<Report> {
    let started = time::now()?.at;
    let result = run_jobs(vault);
    let (ok, err) = match &result {
        Ok(r) => (!r.failed(), None),
        Err(e) => (false, Some(e.to_string())),
    };
    line("tick", &started, ok, err.as_deref())?;
    result
}

fn run_jobs(vault: &Vault) -> Result<Report> {
    let (ctx, mut st) = (context(vault)?, State::load()?);
    let mut report = Report {
        skipped: not_ours(),
        ..Report::default()
    };
    for j in TABLE.iter().filter(|j| j.run == Run::Here) {
        // After the earlier jobs: reindex may have made embed due.
        if ctx.due(j, &st)?.is_none() {
            continue;
        }
        let started = time::now()?.at;
        let clock = Instant::now();
        let outcome = ctx.exec(j.name, &mut st);
        let took = clock.elapsed();
        let (err, skipped) = match outcome {
            Ok(Done::Ran) => (None, None),
            Ok(Done::Skipped(why)) => (Some(format!("skipped: {why}")), Some(why)),
            Err(e) => (Some(e.to_string()), None),
        };
        // A skip is not a failure: the job stays due and runs when the GPU frees up.
        line(
            j.name,
            &started,
            err.is_none() || skipped.is_some(),
            err.as_deref(),
        )?;
        if let Some(why) = skipped {
            report.skipped.push((why, vec![j.name]));
            continue;
        }
        if err.is_none() {
            st.save()?;
        }
        report.ran.push(Ran {
            job: j.name,
            took,
            err,
        });
    }
    Ok(report)
}

/// One C10 line: `{job, started, ended, ok, err}`.
fn line(job: &str, started: &str, ok: bool, err: Option<&str>) -> Result<()> {
    state::append(
        JOBS,
        &json!({"job": job, "started": started, "ended": time::now()?.at, "ok": ok, "err": err}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_at(at: &str) -> Now {
        Now {
            at: at.to_owned(),
            tz: "Europe/Paris".to_owned(),
            place: None,
        }
    }

    #[test]
    fn the_table_is_the_design_table() {
        assert_eq!(TABLE.len(), 18);
        assert_eq!(TABLE.iter().filter(|j| j.kind == Kind::Event).count(), 3);
    }

    #[test]
    fn a_skipped_job_is_never_here() {
        for (_, jobs) in not_ours() {
            assert!(
                jobs.iter()
                    .all(|n| TABLE.iter().any(|j| j.name == *n && j.run != Run::Here))
            );
        }
    }

    #[test]
    fn a_cron_job_is_due_once_per_local_day_and_follows_the_zone() {
        let paris = ctx_at("2026-10-28T03:10:00+01:00");
        assert!(!cron_due(
            &ctx_at("2026-10-28T02:59:00+01:00"),
            "03:00",
            None
        ));
        assert!(cron_due(&paris, "03:00", None));
        assert!(cron_due(&paris, "03:00", Some("2026-10-27")));
        assert!(!cron_due(&paris, "03:00", Some("2026-10-28")));
        // Fly west after the run: the same instant is still the 27th, already run.
        let sf = ctx_at("2026-10-27T19:10:00-07:00");
        assert!(!cron_due(&sf, "03:00", Some("2026-10-27")));
        // Fly east: it is the next local day at 04:00, due again.
        let tokyo = ctx_at("2026-10-29T04:00:00+09:00");
        assert!(cron_due(&tokyo, "03:00", Some("2026-10-28")));
    }
}
