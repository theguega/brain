//! The wiki's review loop (B17–B19). A run's reply becomes a `wiki/*` branch of the vault,
//! built in a temporary worktree under the cache so the vault's checkout is never touched,
//! and lands on the vault's branch only through [`merge`].
//!
//! A reply is a series of blocks, each opened by a header line:
//!
//! ```text
//! === page: <slug>        the full new text of pages/<slug>.md
//! === question: <slug>    learn only: the question the corrected page answers
//! ```
//!
//! Text before the first header is ignored. A question travels in the proposal's commit as
//! a `Brain-Question:` trailer, and a merge appends it to `eval/questions.jsonl`.

pub mod check;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::embed::Embedder;
use crate::error::{Error, Result, io};
use crate::id::{DocId, Slug};
use crate::index;
use crate::lint::{self, Finding};
use crate::log::{self, Entry};
use crate::page;
use crate::time::Now;
use crate::vault::{Vault, git};

const PREFIX: &str = "wiki/";
const TRAILER: &str = "Brain-Question";
pub const QUESTIONS: &str = "eval/questions.jsonl";

/// What a proposal is for, which names its branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Doc(DocId),
    Lint,
    Learn,
}

impl Target {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "lint" => Ok(Self::Lint),
            "learn" => Ok(Self::Learn),
            _ => DocId::parse(s).map(Self::Doc),
        }
    }

    /// `day` is the local `YYYY-MM-DD`.
    #[must_use]
    pub fn branch(&self, day: &str) -> Branch {
        Branch(match self {
            Self::Doc(id) => format!("{PREFIX}{}", id.dir()),
            Self::Lint => format!("{PREFIX}lint-{day}"),
            Self::Learn => format!("{PREFIX}learn-{day}"),
        })
    }
}

/// A proposal branch: `wiki/<doc dir>`, `wiki/lint-<day>` or `wiki/learn-<day>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Branch(String);

impl Branch {
    pub fn parse(s: &str) -> Result<Self> {
        let ok = s.strip_prefix(PREFIX).is_some_and(|rest| {
            !rest.is_empty()
                && !rest.starts_with('.')
                && !rest.contains("..")
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-.".contains(&b))
        });
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(Error::Usage(format!(
                "not a proposal branch: {s:?} (wiki/<name>, as `brain wiki ls` prints it)"
            )))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn name(&self) -> &str {
        self.0.strip_prefix(PREFIX).unwrap_or(&self.0)
    }

    /// The doc it compiles; `None` for a lint or learn proposal.
    #[must_use]
    pub fn doc(&self) -> Option<DocId> {
        DocId::from_dir(self.name()).ok()
    }

    /// What `ls` prints in the doc column: the doc id, `lint` or `learn`.
    #[must_use]
    pub fn about(&self) -> String {
        match self.doc() {
            Some(id) => id.to_string(),
            None => self.name().split('-').next().unwrap_or_default().to_owned(),
        }
    }

    fn is_learn(&self) -> bool {
        self.name().starts_with("learn-")
    }

    fn exists(&self, root: &Path) -> bool {
        let r = format!("refs/heads/{}", self.0);
        git(root, &["show-ref", "--verify", "--quiet", &r]).is_ok()
    }

    fn require(&self, root: &Path) -> Result<()> {
        if self.exists(root) {
            Ok(())
        } else {
            Err(Error::Usage(format!(
                "no open proposal {self}; `brain wiki ls` lists them"
            )))
        }
    }
}

impl std::fmt::Display for Branch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A learn proposal's question, and the pages that answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub q: String,
    pub expect: Vec<Slug>,
}

/// A parsed reply: each page's full new text, and learn questions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    pub pages: Vec<(Slug, String)>,
    pub questions: Vec<Question>,
}

enum Block {
    Page(Slug),
    Question(Slug),
}

pub fn parse_reply(text: &str) -> Result<Reply> {
    let bad = |why: String| Error::Invalid {
        path: PathBuf::from("<stdin>"),
        why,
    };
    let mut blocks: Vec<(Block, Vec<&str>)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let Some(header) = line.strip_prefix("=== ") else {
            if let Some((_, body)) = blocks.last_mut() {
                body.push(line);
            }
            continue;
        };
        let (what, slug) = header.split_once(':').unwrap_or((header, ""));
        let block: fn(Slug) -> Block = match what.trim() {
            "page" => Block::Page,
            "question" => Block::Question,
            _ => {
                return Err(bad(format!(
                    "line {}: expected `=== page: <slug>` or `=== question: <slug>`",
                    i + 1
                )));
            }
        };
        let slug = slug.trim();
        let slug = Slug::parse(slug)
            .map_err(|_| bad(format!("line {}: {slug:?} is not a page slug", i + 1)))?;
        blocks.push((block(slug), Vec::new()));
    }
    let mut reply = Reply::default();
    for (block, body) in blocks {
        let body = body.join("\n");
        match block {
            Block::Page(slug) => {
                if reply.pages.iter().any(|(s, _)| *s == slug) {
                    return Err(bad(format!("page {slug} appears twice")));
                }
                reply
                    .pages
                    .push((slug, format!("{}\n", body.trim_matches('\n'))));
            }
            Block::Question(slug) => {
                let q = body.split_whitespace().collect::<Vec<_>>().join(" ");
                if q.is_empty() {
                    return Err(bad(format!("the question for {slug} is empty")));
                }
                reply.questions.push(Question {
                    q,
                    expect: vec![slug],
                });
            }
        }
    }
    if reply.pages.is_empty() {
        return Err(bad("no `=== page: <slug>` block".to_owned()));
    }
    for q in &reply.questions {
        for slug in &q.expect {
            if !reply.pages.iter().any(|(s, _)| s == slug) {
                return Err(bad(format!(
                    "the question names {slug}, a page not in the reply"
                )));
            }
        }
    }
    Ok(reply)
}

/// What [`propose`] did.
#[derive(Debug)]
pub enum Proposed {
    Branch {
        branch: Branch,
        pages: Vec<Slug>,
    },
    /// Lint errors in the proposed pages; nothing was committed.
    Rejected(Vec<Finding>),
}

/// Lints the reply's pages against the vault as it would be with them, and commits them
/// to the target's branch if they pass.
pub fn propose(vault: &Vault, target: &Target, reply: &Reply, day: &str) -> Result<Proposed> {
    if let Target::Doc(id) = target
        && !vault.has_doc(id)
    {
        return Err(Error::Usage(format!("no doc {id} in the vault")));
    }
    if !reply.questions.is_empty() && *target != Target::Learn {
        return Err(Error::Usage(
            "only a learn proposal carries questions".to_owned(),
        ));
    }
    let branch = target.branch(day);
    let root = vault.root();
    vault.settled(|| {
        if branch.exists(root) {
            return Err(Error::Usage(format!(
                "{branch} is already proposed; merge or drop it first"
            )));
        }
        let wt = Worktree::add(root)?;
        let tree = Vault::open(&wt.dir)?;
        let files: BTreeSet<PathBuf> = reply.pages.iter().map(|(s, _)| page::path(s)).collect();
        for (slug, text) in &reply.pages {
            tree.write(&page::path(slug), text)?;
        }
        let mut errors = lint::lint(&tree)?;
        errors.extend(check::check(&tree)?.errors);
        errors.retain(|f| files.contains(&f.file));
        if !errors.is_empty() {
            return Ok(Proposed::Rejected(errors));
        }
        let dir = wt.dir.as_path();
        git(dir, &["add", "--", "pages"])?;
        if git(dir, &["diff", "--cached", "--name-only"])?
            .trim()
            .is_empty()
        {
            return Err(Error::Usage("the reply changes no page".to_owned()));
        }
        let mut msg = format!("wiki: propose {}\n\n", branch.about());
        for q in &reply.questions {
            let json = serde_json::to_string(q).map_err(|source| Error::Decode {
                what: "a question",
                source,
            })?;
            msg.push_str(&format!("{TRAILER}: {json}\n"));
        }
        git(
            dir,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", &msg],
        )?;
        let sha = git(dir, &["rev-parse", "HEAD"])?;
        git(root, &["branch", branch.as_str(), sha.trim()])?;
        Ok(Proposed::Branch {
            pages: reply.pages.iter().map(|(s, _)| s.clone()).collect(),
            branch: branch.clone(),
        })
    })
}

/// A detached worktree of the vault's HEAD under `$XDG_CACHE_HOME/brain/worktrees/`,
/// removed on drop.
struct Worktree<'a> {
    repo: &'a Path,
    dir: PathBuf,
}

impl<'a> Worktree<'a> {
    fn add(repo: &'a Path) -> Result<Self> {
        let dir = index::xdg("XDG_CACHE_HOME", ".cache")?
            .join("brain/worktrees")
            .join(format!("wiki-{}", std::process::id()));
        // Left over by a killed run with the same pid.
        if dir.exists() {
            io(fs::remove_dir_all(&dir), &dir)?;
        }
        git(repo, &["worktree", "prune"])?;
        if let Some(parent) = dir.parent() {
            io(fs::create_dir_all(parent), parent)?;
        }
        let wt = Self { repo, dir };
        git(
            repo,
            &["worktree", "add", "-q", "--detach", wt.path()?, "HEAD"],
        )?;
        Ok(wt)
    }

    fn path(&self) -> Result<&str> {
        self.dir
            .to_str()
            .ok_or_else(|| Error::Usage(format!("{} is not UTF-8", self.dir.display())))
    }
}

impl Drop for Worktree<'_> {
    fn drop(&mut self) {
        if let Ok(p) = self.path() {
            let _ = git(self.repo, &["worktree", "remove", "--force", p]);
        }
    }
}

/// An open proposal, as `ls` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    pub branch: Branch,
    pub pages: Vec<Slug>,
    /// The proposal commit's date, ISO 8601.
    pub created: String,
}

pub fn ls(vault: &Vault) -> Result<Vec<Open>> {
    let root = vault.root();
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%09%(creatordate:iso-strict)",
            "refs/heads/wiki/",
        ],
    )?;
    let mut out = Vec::new();
    for line in refs.lines() {
        let Some((name, created)) = line.split_once('\t') else {
            continue;
        };
        // A branch someone made by hand under wiki/ with another shape is not a proposal.
        let Ok(branch) = Branch::parse(name) else {
            continue;
        };
        out.push(Open {
            pages: changed_pages(root, &branch)?,
            branch,
            created: created.to_owned(),
        });
    }
    Ok(out)
}

/// The pages the branch changes since it left the vault's branch.
fn changed_pages(root: &Path, branch: &Branch) -> Result<Vec<Slug>> {
    let range = format!("HEAD...{branch}");
    let names = git(root, &["diff", "--name-only", &range, "--", "pages/"])?;
    Ok(names
        .lines()
        .filter_map(|n| n.strip_prefix("pages/")?.strip_suffix(".md"))
        .filter_map(|s| Slug::parse(s).ok())
        .collect())
}

/// The branch's page diff against the vault's branch.
pub fn diff(vault: &Vault, branch: &Branch) -> Result<String> {
    let root = vault.root();
    branch.require(root)?;
    let range = format!("HEAD...{branch}");
    git(
        root,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            &range,
            "--",
            "pages/",
        ],
    )
}

pub fn drop_branch(vault: &Vault, branch: &Branch) -> Result<()> {
    let root = vault.root();
    vault.settled(|| {
        branch.require(root)?;
        git(root, &["branch", "-D", branch.as_str()]).map(drop)
    })
}

/// What [`merge`] did.
#[derive(Debug)]
pub enum Merged {
    Clean {
        pages: Vec<Slug>,
        questions: usize,
        warnings: Vec<String>,
    },
    /// The merge was aborted and the branch dropped; its doc is back in the queue.
    Conflict {
        pages: Vec<Slug>,
        files: Vec<String>,
        requeued: Option<DocId>,
    },
}

/// Merges the branch into the vault's branch, reindexes its pages into `db`, logs a
/// `compile` block, and adds a learn proposal's questions to the eval set.
pub fn merge(
    vault: &Vault,
    branch: &Branch,
    now: Now,
    db: &Path,
    embedder: &Embedder,
) -> Result<Merged> {
    let root = vault.root();
    let landed = vault.settled(|| {
        branch.require(root)?;
        let pages = changed_pages(root, branch)?;
        let questions = if branch.is_learn() {
            questions(root, branch)?
        } else {
            Vec::new()
        };
        let msg = format!("wiki: merge {branch}");
        let merged = git(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "merge",
                "--no-ff",
                "-q",
                "-m",
                &msg,
                branch.as_str(),
            ],
        );
        if let Err(e) = merged {
            let files: Vec<String> = git(root, &["diff", "--name-only", "--diff-filter=U"])?
                .lines()
                .map(str::to_owned)
                .collect();
            if files.is_empty() {
                return Err(e);
            }
            git(root, &["merge", "--abort"])?;
            git(root, &["branch", "-D", branch.as_str()])?;
            return Ok(Err(Merged::Conflict {
                pages,
                files,
                requeued: branch.doc(),
            }));
        }
        git(root, &["branch", "-D", branch.as_str()])?;
        Ok(Ok((pages, questions)))
    })?;
    let (pages, questions) = match landed {
        Ok(l) => l,
        Err(conflict) => return Ok(conflict),
    };

    let warnings = index::reindex_pages(vault, db, &pages, embedder)?;
    let mut text = format!("merged {branch}\n");
    if let Some(id) = branch.doc() {
        text.push_str(&format!("docs: {id}\n"));
    }
    let names: Vec<&str> = pages.iter().map(Slug::as_str).collect();
    text.push_str(&format!("pages: {}\n", names.join(", ")));
    log::append(vault, &Entry::new(now, Slug::parse("compile")?, &text)?)?;
    for q in &questions {
        let line = serde_json::to_string(&EvalQuestion {
            q: &q.q,
            expect: &q.expect,
            from: "correction",
        })
        .map_err(|source| Error::Json {
            path: QUESTIONS.into(),
            source,
        })?;
        vault.append(Path::new(QUESTIONS), &format!("{line}\n"))?;
    }
    Ok(Merged::Clean {
        pages,
        questions: questions.len(),
        warnings,
    })
}

/// A line of `eval/questions.jsonl`, in its field order.
#[derive(Serialize)]
struct EvalQuestion<'a> {
    q: &'a str,
    expect: &'a [Slug],
    from: &'static str,
}

fn questions(root: &Path, branch: &Branch) -> Result<Vec<Question>> {
    let range = format!("HEAD..{branch}");
    let fmt = format!("--format=%(trailers:key={TRAILER},valueonly,unfold)");
    git(root, &["log", &fmt, &range])?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l).map_err(|source| Error::Decode {
                what: "a Brain-Question trailer",
                source,
            })
        })
        .collect()
}

/// A doc in the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    pub id: DocId,
    pub kind: String,
    pub added: String,
}

/// Marked docs that no merged compile lists and no open proposal covers, oldest first.
pub fn queue(vault: &Vault) -> Result<Vec<Queued>> {
    let mut done = compiled(vault)?;
    done.extend(ls(vault)?.iter().filter_map(|o| o.branch.doc()));
    let mut out = Vec::new();
    for name in lint::entries(vault, "docs")? {
        let Ok(id) = DocId::from_dir(&name) else {
            continue;
        };
        if done.contains(&id) {
            continue;
        }
        let Some(raw) = vault.read(&Vault::doc_dir(&id).join("meta.json"))? else {
            continue;
        };
        // An unreadable meta.json is lint's to report; it is still marked.
        let meta: Value = serde_json::from_str(&raw).unwrap_or_default();
        let get = |k: &str| {
            meta.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        out.push(Queued {
            kind: get("kind"),
            added: get("added"),
            id,
        });
    }
    out.sort_by(|a, b| a.added.cmp(&b.added).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

/// Doc ids on a `docs:` line of any `compile` log block.
fn compiled(vault: &Vault) -> Result<BTreeSet<DocId>> {
    let mut out = BTreeSet::new();
    for name in lint::entries(vault, "log")? {
        let Some(text) = vault.read(&Path::new("log").join(&name))? else {
            continue;
        };
        let mut in_compile = false;
        for line in text.lines() {
            if let Some(e) = Entry::parse_heading(line) {
                in_compile = e.source.as_str() == "compile";
            } else if in_compile && let Some(ids) = line.strip_prefix("docs: ") {
                out.extend(ids.split(',').filter_map(|s| DocId::parse(s).ok()));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_splits_into_pages_and_questions() {
        let r = parse_reply(
            "Here are the pages.\n=== page: pi0\n---\nkind: concept\n---\nBody.\n\n=== page: flow-matching\nx\n=== question: pi0\nWhat is\n  pi0's action expert?\n",
        )
        .unwrap();
        assert_eq!(r.pages.len(), 2);
        assert_eq!(r.pages[0].1, "---\nkind: concept\n---\nBody.\n");
        assert_eq!(r.pages[1].1, "x\n");
        assert_eq!(r.questions[0].q, "What is pi0's action expert?");
        assert_eq!(r.questions[0].expect[0].as_str(), "pi0");
    }

    #[test]
    fn a_bad_reply_is_an_error() {
        for bad in [
            "",
            "no blocks",
            "=== page: Pi0\nx",
            "=== page: a\nx\n=== page: a\ny",
            "=== note: a\nx",
            "=== page: a\nx\n=== question: b\nwhy?",
        ] {
            assert!(parse_reply(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn branches_name_their_target() {
        let id = DocId::parse("arxiv:2410.24164").unwrap();
        let b = Target::Doc(id.clone()).branch("2026-10-27");
        assert_eq!(b.as_str(), "wiki/arxiv--2410.24164");
        assert_eq!(b.doc(), Some(id));
        let l = Target::Learn.branch("2026-10-27");
        assert_eq!(
            (l.as_str(), l.about().as_str()),
            ("wiki/learn-2026-10-27", "learn")
        );
        assert!(Branch::parse("wiki/../x").is_err());
        assert!(Branch::parse("main").is_err());
    }
}
