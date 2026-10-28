//! `brain wiki`: the commands over [`brain::wiki`].

use std::io::Read as _;
use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;
use serde_json::{Value, json};

use brain::embed::Embedder;
use brain::wiki::{self, Branch, Merged, Proposed, Target};
use brain::{Error, Result, Slug, Vault, index, time};

use crate::{Reply, errors};

#[derive(Subcommand)]
pub(crate) enum WikiCmd {
    /// Marked docs that no merged compile lists and no open proposal covers, oldest first
    Queue,
    /// Open proposals: the vault's `wiki/*` branches
    Ls,
    /// One proposal's page diff against the vault
    Diff {
        #[arg(value_parser = Branch::parse)]
        branch: Branch,
    },
    /// Lint a run's reply from stdin and commit its pages to a `wiki/*` branch
    ///
    /// The reply holds one `=== page: <slug>` line per page, each followed by the page's
    /// full new text. A learn reply may add `=== question: <slug>` blocks: the question the
    /// corrected page answers, added to eval/questions.jsonl when it is merged. In concept
    /// and synthesis pages every claim cites a doc by its directory name, `[[arxiv--2410.24164]]`
    /// or `[[arxiv--2410.24164#Method]]`, and lists its id (`arxiv:2410.24164`) in sources[];
    /// a typed relation is a frontmatter property of quoted wikilinks
    /// (`builds_on: ["[[pi0]]"]`). The pages are linted against the vault as it would be
    /// with them; on errors nothing is committed and the exit code is 1. The vault's
    /// checkout is never touched.
    Propose {
        /// The doc compiled (`wiki/<doc>`), or `lint` / `learn` (`wiki/<that>-<date>`)
        #[arg(value_parser = Target::parse)]
        target: Target,
    },
    /// Merge a proposal into the vault, reindex its pages and log a `compile` block
    ///
    /// On a conflict the merge is aborted, the branch dropped and its doc goes back to
    /// the queue; the exit code is 1.
    Merge {
        #[arg(value_parser = Branch::parse)]
        branch: Branch,
    },
    /// Discard a proposal; its doc goes back to the queue
    Drop {
        #[arg(value_parser = Branch::parse)]
        branch: Branch,
    },
}

fn slugs(pages: &[Slug]) -> Value {
    json!(pages.iter().map(Slug::as_str).collect::<Vec<_>>())
}

pub(crate) fn run(root: &Path, cmd: &WikiCmd) -> Result<(Reply, ExitCode)> {
    let vault = Vault::open(root)?;
    let reply = match cmd {
        WikiCmd::Queue => {
            let rows: Vec<Vec<Value>> = wiki::queue(&vault)?
                .into_iter()
                .map(|q| vec![json!(q.id.to_string()), json!(q.kind), json!(q.added)])
                .collect();
            Reply::Table {
                name: "queue",
                cols: &["id", "kind", "added"],
                total: rows.len().to_string(),
                rows,
            }
        }
        WikiCmd::Ls => {
            let rows: Vec<Vec<Value>> = wiki::ls(&vault)?
                .into_iter()
                .map(|o| {
                    vec![
                        json!(o.branch.as_str()),
                        json!(o.branch.about()),
                        slugs(&o.pages),
                        json!(o.created),
                    ]
                })
                .collect();
            Reply::Table {
                name: "branches",
                cols: &["branch", "doc", "pages", "created"],
                total: rows.len().to_string(),
                rows,
            }
        }
        WikiCmd::Diff { branch } => Reply::Text(wiki::diff(&vault, branch)?),
        WikiCmd::Propose { target } => {
            let mut text = String::new();
            std::io::stdin()
                .take(wiki::MAX_REPLY + 1)
                .read_to_string(&mut text)
                .map_err(|source| Error::Io {
                    path: "<stdin>".into(),
                    source,
                })?;
            if text.len() as u64 > wiki::MAX_REPLY {
                return Err(Error::BadReply(format!("over {} bytes", wiki::MAX_REPLY)));
            }
            let reply = wiki::parse_reply(&text)?;
            match wiki::propose(&vault, target, &reply, time::now()?.day())? {
                Proposed::Branch { branch, pages } => Reply::Fields(vec![
                    ("branch", json!(branch.as_str())),
                    ("pages", slugs(&pages)),
                ]),
                Proposed::Rejected(findings) => {
                    eprintln!(
                        "error: {} lint errors, nothing committed\nnext: fix the pages, then propose again",
                        findings.len()
                    );
                    return Ok((errors(findings), ExitCode::FAILURE));
                }
            }
        }
        WikiCmd::Merge { branch } => {
            let merged = wiki::merge(
                &vault,
                branch,
                time::now()?,
                &index::db_path()?,
                &Embedder::from_env(),
            )?;
            match merged {
                Merged::Clean {
                    pages,
                    questions,
                    warnings,
                } => {
                    for w in &warnings {
                        eprintln!("warning: {w}");
                    }
                    let mut fields =
                        vec![("merged", json!(branch.as_str())), ("pages", slugs(&pages))];
                    if questions > 0 {
                        fields.push(("questions", json!(questions)));
                    }
                    Reply::Fields(fields)
                }
                Merged::Conflict {
                    pages,
                    files,
                    requeued,
                } => {
                    let next = match &requeued {
                        Some(id) => format!("it was dropped, and {id} is back in brain wiki queue"),
                        None => format!(
                            "it was kept: rebase it on the vault's branch, or brain wiki drop {branch}"
                        ),
                    };
                    eprintln!("error: {branch} conflicts with the vault\nnext: {next}");
                    let mut fields = vec![
                        ("merged", json!("none")),
                        ("pages", slugs(&pages)),
                        ("conflict", json!(files)),
                    ];
                    if let Some(id) = requeued {
                        fields.push(("requeued", json!(id.to_string())));
                    }
                    return Ok((Reply::Fields(fields), ExitCode::FAILURE));
                }
                Merged::Rejected(findings) => {
                    eprintln!(
                        "error: {} lint errors in the merged pages, nothing merged\nnext: brain wiki drop {branch}, then propose again",
                        findings.len()
                    );
                    return Ok((errors(findings), ExitCode::FAILURE));
                }
            }
        }
        WikiCmd::Drop { branch } => {
            wiki::drop_branch(&vault, branch)?;
            Reply::Fields(vec![("dropped", json!(branch.as_str()))])
        }
    };
    Ok((reply, ExitCode::SUCCESS))
}
