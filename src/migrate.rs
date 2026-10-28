//! B16: moving to a new host. `brain migrate` checks the procedure and changes nothing.

use std::path::Path;
use std::process::Command;

use crate::index::{self, Index};
use crate::{eval, state};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Todo,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok",
            Self::Todo => "todo",
        })
    }
}

#[derive(Clone, Debug)]
pub struct Step {
    pub command: &'static str,
    pub status: Status,
    pub detail: String,
}

/// The five steps in order, each with what this host already has.
#[must_use]
pub fn check(vault: &Path) -> Vec<Step> {
    let step = |command, ok: bool, yes: String, no: String| Step {
        command,
        status: if ok { Status::Ok } else { Status::Todo },
        detail: if ok { yes } else { no },
    };
    let has_vault = vault.join(".git").exists();
    let restic = Command::new("restic").arg("version").output().is_ok();
    let config =
        std::env::var_os("HOME").is_some_and(|h| Path::new(&h).join(".config/brain").is_dir());
    let docs = index::db_path()
        .ok()
        .and_then(|p| Index::open_existing(&p).ok())
        .and_then(|ix| {
            ix.conn
                .query_row("SELECT count(*) FROM docs", [], |r| r.get::<_, i64>(0))
                .ok()
        });
    let timer = Command::new("systemctl")
        .args(["--user", "is-enabled", "brain-tick.timer"])
        .output()
        .map_or_else(
            |_| "systemctl not found".to_owned(),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        );
    let last = eval::last().ok().flatten();
    vec![
        step(
            "restic restore",
            has_vault,
            format!("vault present at {}", vault.display()),
            format!(
                "no vault at {}{}",
                vault.display(),
                if restic {
                    ""
                } else {
                    " (restic is not installed)"
                }
            ),
        ),
        step(
            "dot",
            config,
            "~/.config/brain present".to_owned(),
            "no ~/.config/brain: the dotfiles are not applied".to_owned(),
        ),
        step(
            "brain reindex --full",
            docs.is_some_and(|n| n > 0),
            format!("index holds {} docs", docs.unwrap_or(0)),
            "no index yet".to_owned(),
        ),
        step(
            "systemctl --user enable --now brain-tick.timer",
            timer == "enabled",
            "timer enabled".to_owned(),
            format!(
                "timer {}",
                if timer.is_empty() {
                    "not enabled"
                } else {
                    &timer
                }
            ),
        ),
        step(
            "brain eval",
            last.is_some(),
            last.as_ref().map_or_else(String::new, |l| {
                format!(
                    "compare with recall@8 {:.3}, tokens {:.1} ({})",
                    l.recall, l.tokens, l.at
                )
            }),
            format!(
                "no earlier score in {}/{}: nothing to compare with",
                state::dir().map_or_else(|_| "state".to_owned(), |d| d.display().to_string()),
                eval::FILE
            ),
        ),
    ]
}
