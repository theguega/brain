//! The commands that run the system: tick, profile, eval, migrate. They own no logic
//! (the library does); this only shapes what they print.

use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;
use serde_json::json;

use brain::embed::Embedder;
use brain::tick::Report;
use brain::{Result, Vault, eval, migrate, profile, tick, time, tokens};

use crate::Reply;

#[derive(Subcommand)]
pub enum Ops {
    /// Run every job that is due, once: the only scheduler (a systemd timer calls it)
    ///
    /// Cron jobs run at their local time (C15), once per local day; lazy jobs run when
    /// there is work. Model jobs and other components' jobs are listed as skipped. Each
    /// job that runs, and the tick itself, appends a line to `~/.local/state/brain/jobs.jsonl`.
    Tick {
        /// Print what is due and why; run nothing and write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Regenerate profile.md: focus, active projects, today's plan, what needs you (max 400 tokens)
    Profile,
    /// Score `eval/questions.jsonl`: recall@8 and output tokens per hit, against the last run
    ///
    /// The result is appended to `~/.local/state/brain/eval.jsonl` with the brain build and
    /// the vault HEAD. Eval queries are never logged as real ones.
    Eval,
    /// Check the move to a new host (restic, dot, reindex, timer, eval); changes nothing
    Migrate,
}

pub fn run(root: &Path, cmd: &Ops) -> Result<(Reply, ExitCode)> {
    let ok = |r| Ok((r, ExitCode::SUCCESS));
    match cmd {
        Ops::Tick { dry_run: true } => ok(due(&tick::dry_run(&Vault::open(root)?)?)),
        Ops::Tick { dry_run: false } => {
            let vault = Vault::open(root)?;
            let report = tick::run(&vault);
            // Whatever the tick wrote (profile.md) still gets committed.
            if let Err(e) = vault.commit_later() {
                eprintln!("warning: the vault will not autocommit: {e}");
            }
            let report = report?;
            let code = if report.failed() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            };
            Ok((ran(&report), code))
        }
        Ops::Profile => {
            let vault = Vault::open(root)?;
            let text = profile::write(&vault, &time::now()?)?;
            if let Err(e) = vault.commit_later() {
                eprintln!("warning: the vault will not autocommit: {e}");
            }
            ok(Reply::Fields(vec![
                ("profile", json!(profile::FILE)),
                ("tokens", json!(tokens::count(&text))),
                ("note", json!(profile::DROVER_NOTE)),
            ]))
        }
        Ops::Eval => {
            let vault = Vault::open(root)?;
            let before = eval::last()?;
            let now = eval::run(&vault, &Embedder::from_env())?;
            let mut fields = vec![
                ("recall@8", json!(format!("{:.3}", now.recall))),
                ("tokens_per_hit", json!(format!("{:.1}", now.tokens))),
                ("questions", json!(now.questions)),
            ];
            if let Some(why) = &now.degraded {
                fields.push(("degraded", json!(why)));
            }
            fields.push((
                "vs_last",
                json!(before.map_or_else(
                    || "no earlier run".to_owned(),
                    |b| format!(
                        "recall {:+.3}, tokens {:+.1} (brain {} -> {}, vault {} -> {})",
                        now.recall - b.recall,
                        now.tokens - b.tokens,
                        b.brain,
                        now.brain,
                        b.vault.get(..7).unwrap_or(&b.vault),
                        now.vault.get(..7).unwrap_or(&now.vault),
                    )
                )),
            ));
            ok(Reply::Fields(fields))
        }
        Ops::Migrate => {
            let rows: Vec<_> = migrate::check(root)
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    vec![
                        json!(i + 1),
                        json!(s.command),
                        json!(s.status.to_string()),
                        json!(s.detail),
                    ]
                })
                .collect();
            ok(Reply::Table {
                name: "steps",
                cols: &["n", "command", "status", "detail"],
                total: rows.len().to_string(),
                rows,
            })
        }
    }
}

fn skipped(report: &Report) -> Vec<Reply> {
    report
        .skipped
        .iter()
        .map(|(why, jobs)| Reply::Text(format!("skipped: {why}: {}", jobs.join(", "))))
        .collect()
}

fn due(report: &Report) -> Reply {
    let rows: Vec<_> = report
        .due
        .iter()
        .map(|(j, why)| vec![json!(j), json!(why)])
        .collect();
    let mut out = vec![Reply::Table {
        name: "due",
        cols: &["job", "why"],
        total: rows.len().to_string(),
        rows,
    }];
    out.extend(skipped(report));
    Reply::Many(out)
}

fn ran(report: &Report) -> Reply {
    let rows: Vec<_> = report
        .ran
        .iter()
        .map(|r| {
            let ok = r.err.is_none();
            vec![
                json!(r.job),
                json!(ok),
                json!(format!("{:.1}s", r.took.as_secs_f64())),
            ]
        })
        .collect();
    let mut out = vec![Reply::Table {
        name: "ran",
        cols: &["job", "ok", "took"],
        total: rows.len().to_string(),
        rows,
    }];
    for r in &report.ran {
        if let Some(e) = &r.err {
            eprintln!("error: {}: {e}", r.job);
        }
    }
    out.extend(skipped(report));
    Reply::Many(out)
}
