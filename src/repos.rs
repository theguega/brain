//! C13: git checkouts under the roots in `repos.toml`, indexed as markdown, commits and a
//! card; code is read live by `brain code`.
//!
//! ```toml
//! roots = ["~/Developer"]          # under the home directory, never absolute (B16)
//! include = ["*"]                  # globs on the path below a root; `*`, `**`, `?`
//! exclude = ["archive/**"]
//! ```
//!
//! Without the file, the root is `~/Developer` and every checkout is included. A checkout
//! is a directory with a `.git` directory; worktrees (a `.git` file) are left out, since
//! they repeat their main checkout.

use std::fs;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::doc::Taint;
use crate::error::{Error, Result, io};
use crate::index::Source;
use crate::vault::git;

pub const FILE: &str = "repos.toml";
const DEFAULT_ROOT: &str = "~/Developer";
/// Directories below a root searched for checkouts.
const DEPTH: usize = 3;
/// Commits read on a repo's first index.
const FIRST_COMMITS: usize = 500;
const MAX_HITS: usize = 20;
const MAX_LINE: usize = 160;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub roots: Vec<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: vec![DEFAULT_ROOT.to_owned()],
            include: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

impl Config {
    /// `<vault>/repos.toml`, or the default when there is none.
    pub fn load(vault_root: &Path) -> Result<Self> {
        let path = vault_root.join(FILE);
        match fs::read_to_string(&path) {
            Ok(s) => Self::parse(&s).map_err(|(line, why)| Error::Frontmatter {
                path: PathBuf::from(FILE),
                line,
                why,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => io(Err(e), &path),
        }
    }

    /// The TOML subset above: string arrays, possibly over several lines, and comments.
    pub fn parse(s: &str) -> Result<Self, (usize, String)> {
        let mut cfg = Self {
            roots: Vec::new(),
            include: Vec::new(),
            exclude: Vec::new(),
        };
        let mut roots = false;
        let mut lines = s.lines().enumerate();
        while let Some((i, line)) = lines.next() {
            let line = strip_comment(line);
            if line.is_empty() {
                continue;
            }
            let err = |why: &str| (i + 1, why.to_owned());
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| err("expected `key = [...]`"))?;
            let mut value = value.trim().to_owned();
            while !value.ends_with(']') {
                let (_, more) = lines.next().ok_or_else(|| err("unclosed `[`"))?;
                value.push_str(strip_comment(more));
            }
            let items = value
                .strip_prefix('[')
                .and_then(|v| v.strip_suffix(']'))
                .ok_or_else(|| err("expected a list of strings"))?;
            let items = items
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .filter(|s| !s.contains(['"', '\\']))
                        .map(str::to_owned)
                        .ok_or_else(|| err("items are plain \"strings\""))
                })
                .collect::<Result<Vec<_>, _>>()?;
            match key.trim() {
                "roots" => {
                    if let Some(r) = items.iter().find(|r| !r.starts_with("~/")) {
                        return Err(err(&format!(
                            "root {r:?} must start with ~/ (no absolute paths in the vault)"
                        )));
                    }
                    roots = true;
                    cfg.roots = items;
                }
                "include" => cfg.include = items,
                "exclude" => cfg.exclude = items,
                k => return Err(err(&format!("unknown key `{k}`"))),
            }
        }
        if !roots {
            cfg.roots = Self::default().roots;
        }
        Ok(cfg)
    }

    fn wants(&self, rel: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|g| glob(g, rel)))
            && !self.exclude.iter().any(|g| glob(g, rel))
    }
}

fn strip_comment(line: &str) -> &str {
    // Items cannot hold `"` or `#` inside quotes in this subset, so the first `#` starts a comment.
    line.split('#').next().unwrap_or_default().trim()
}

/// `*` and `?` stay within a path segment; `**` spans segments.
#[must_use]
pub fn glob(pat: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            [b'*', b'*', rest @ ..] => {
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=s.len()).any(|i| s.get(i..).is_some_and(|t| go(rest, t)))
            }
            [b'*', rest @ ..] => (0..=s.len())
                .take_while(|i| s.get(..*i).is_some_and(|h| !h.contains(&b'/')))
                .any(|i| s.get(i..).is_some_and(|t| go(rest, t))),
            [b'?', rest @ ..] => matches!(s, [c, t @ ..] if *c != b'/' && go(rest, t)),
            [c, rest @ ..] => matches!(s, [d, t @ ..] if c == d && go(rest, t)),
        }
    }
    go(pat.as_bytes(), path.as_bytes())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    /// Relative to the home directory (C13).
    pub rel: String,
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::Usage("HOME is not set".to_owned()))
}

/// Every wanted checkout, sorted by name; the first of two with one name wins.
pub fn discover(cfg: &Config) -> Result<Vec<Repo>> {
    let home = home()?;
    let mut out: Vec<Repo> = Vec::new();
    for root in &cfg.roots {
        let dir = home.join(root.trim_start_matches("~/"));
        let mut found = Vec::new();
        walk(&dir, &dir, 0, &mut found);
        found.sort();
        for path in found {
            let below = path.strip_prefix(&dir).unwrap_or(&path).to_string_lossy();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !cfg.wants(&below) || out.iter().any(|r| r.name == name) {
                continue;
            }
            let rel = path
                .strip_prefix(&home)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            out.push(Repo { name, path, rel });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Unreadable directories are skipped: discovery is best effort over a dev tree.
fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if dir != root && dir.join(".git").is_dir() {
        out.push(dir.to_owned());
        return;
    }
    if depth >= DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let hidden = e.file_name().to_string_lossy().starts_with('.');
        if !hidden && e.file_type().is_ok_and(|t| t.is_dir()) {
            walk(root, &e.path(), depth + 1, out);
        }
    }
}

/// What reindex keeps of one repo.
#[derive(Debug)]
pub(crate) struct Scan {
    pub head: String,
    pub remote: Option<String>,
    pub docs: Vec<Source>,
    /// Commits after the last indexed head, or the last [`FIRST_COMMITS`].
    pub commits: Vec<Source>,
    pub languages: Vec<String>,
    pub last_activity: String,
}

/// `None` for a checkout with no commit yet.
pub(crate) fn scan(repo: &Repo, since: Option<&str>) -> Result<Option<Scan>> {
    let Ok(head) = git(&repo.path, &["rev-parse", "HEAD"]) else {
        return Ok(None);
    };
    let head = head.trim().to_owned();
    let remote = git(&repo.path, &["remote", "get-url", "origin"])
        .ok()
        .map(|r| without_userinfo(r.trim()));
    let last_activity = git(&repo.path, &["log", "-1", "--format=%cs"])?
        .trim()
        .to_owned();
    let files = git(&repo.path, &["ls-files", "-z"])?;
    let files: Vec<&str> = files.split('\0').filter(|f| !f.is_empty()).collect();
    let mut docs = Vec::new();
    for f in files.iter().filter(|f| is_doc(f)) {
        let p = repo.path.join(f);
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        let title = text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map_or_else(|| format!("{} {f}", repo.name), |t| t.trim().to_owned());
        docs.push(Source {
            id: format!("repo:{}/{f}", repo.name),
            kind: "note".to_owned(),
            title,
            date: last_activity.clone(),
            meta: json!({"repo": repo.name, "path": f}),
            taint: Taint::Own,
            text,
            links: Vec::new(),
        });
    }
    let range = since
        .filter(|s| git(&repo.path, &["cat-file", "-e", &format!("{s}^{{commit}}")]).is_ok())
        .map(|s| format!("{s}..HEAD"));
    let commits = commits(repo, range.as_deref())?;
    Ok(Some(Scan {
        head,
        remote,
        docs,
        commits,
        languages: languages(&files),
        last_activity,
    }))
}

fn is_doc(f: &str) -> bool {
    matches!(f, "README.md" | "AGENTS.md" | "DESIGN.md")
        || (f.starts_with("docs/") && f.ends_with(".md"))
}

fn commits(repo: &Repo, range: Option<&str>) -> Result<Vec<Source>> {
    let first = format!("--max-count={FIRST_COMMITS}");
    let args = [
        "log",
        "--no-merges",
        "--format=%H%x1f%cs%x1f%B%x1e",
        range.unwrap_or(&first),
    ];
    let out = git(&repo.path, &args)?;
    Ok(out
        .split('\x1e')
        .filter_map(|rec| {
            let mut f = rec.trim_start_matches('\n').splitn(3, '\x1f');
            let (sha, date, msg) = (f.next()?, f.next()?, f.next()?.trim());
            let short = sha.get(..12)?;
            Some(Source {
                id: format!("commit:{}@{short}", repo.name),
                kind: "log".to_owned(),
                title: msg.lines().next().unwrap_or_default().to_owned(),
                date: date.to_owned(),
                meta: json!({"repo": repo.name, "sha": sha}),
                taint: Taint::Own,
                text: msg.to_owned(),
                links: Vec::new(),
            })
        })
        .collect())
}

fn languages(files: &[&str]) -> Vec<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for f in files {
        let ext = f.rsplit_once('.').map_or("", |(_, e)| e);
        let lang = match ext {
            "rs" => "rust",
            "py" => "python",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" | "mjs" => "javascript",
            "go" => "go",
            "c" | "h" => "c",
            "cc" | "cpp" | "hpp" | "cxx" => "c++",
            "sh" => "shell",
            "lua" => "lua",
            "swift" => "swift",
            "kt" => "kotlin",
            "java" => "java",
            _ => continue,
        };
        match counts.iter_mut().find(|(l, _)| *l == lang) {
            Some((_, n)) => *n += 1,
            None => counts.push((lang, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    counts
        .into_iter()
        .take(3)
        .map(|(l, _)| l.to_owned())
        .collect()
}

/// A remote without `user:token@`, which must never reach the index.
fn without_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (auth, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    format!("{scheme}://{host}/{path}")
        .trim_end_matches('/')
        .to_owned()
}

/// `host/owner/name`, lowercase, for matching a remote against a paper's code link.
#[must_use]
pub(crate) fn canonical_url(url: &str) -> String {
    let u = url.trim().to_ascii_lowercase();
    let u = u.split_once("://").map_or(u.as_str(), |(_, r)| r);
    let u = u.split_once('@').map_or(u, |(_, r)| r);
    u.replacen(':', "/", 1)
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_owned()
}

/// The repo card's text.
pub(crate) fn card(repo: &Repo, scan: &Scan, linked_from: &[String]) -> Source {
    let mut text = format!("# {}\n\n", repo.name);
    text.push_str(&format!(
        "remote: {}\n\nlanguages: {}\n\nlast activity: {}",
        scan.remote.as_deref().unwrap_or("none"),
        if scan.languages.is_empty() {
            "none".to_owned()
        } else {
            scan.languages.join(", ")
        },
        scan.last_activity
    ));
    if !linked_from.is_empty() {
        text.push_str(&format!("\n\nlinked from: {}", linked_from.join(", ")));
    }
    Source {
        id: format!("repo:{}", repo.name),
        kind: "repo".to_owned(),
        title: repo.name.clone(),
        date: scan.last_activity.clone(),
        meta: json!({"repo": repo.name, "path": repo.rel, "remote": scan.remote}),
        taint: Taint::Own,
        text,
        links: Vec::new(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub repo: String,
    pub path: String,
    pub line: u64,
    pub text: String,
}

/// `rg --json` across the repos; the first [`MAX_HITS`] in repo, path, line order, and
/// the total number of matching lines.
pub fn code(repos: &[Repo], pattern: &str) -> Result<(Vec<Hit>, usize)> {
    if repos.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let fail = |detail: String| Error::Command {
        cmd: "rg".to_owned(),
        detail,
    };
    let mut child = Command::new("rg")
        .args([
            "--json",
            "--smart-case",
            "--max-columns",
            "500",
            "-e",
            pattern,
            "--",
        ])
        .args(repos.iter().map(|r| &r.path))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| fail(e.to_string()))?;
    let mut hits = Vec::new();
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines() {
            let line = line.map_err(|e| fail(e.to_string()))?;
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("type").and_then(Value::as_str) != Some("match") {
                continue;
            }
            let data = v.get("data").unwrap_or(&Value::Null);
            let text_of = |k: &str| {
                data.get(k)
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
            };
            let (Some(path), Some(text)) = (text_of("path"), text_of("lines")) else {
                continue;
            };
            let Some(repo) = repos.iter().find(|r| Path::new(path).starts_with(&r.path)) else {
                continue;
            };
            let rel = Path::new(path)
                .strip_prefix(&repo.path)
                .map_or_else(|_| path.to_owned(), |p| p.to_string_lossy().into_owned());
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            hits.push(Hit {
                repo: repo.name.clone(),
                path: rel,
                line: data.get("line_number").and_then(Value::as_u64).unwrap_or(0),
                text: text.chars().take(MAX_LINE).collect(),
            });
        }
    }
    // Exit 1 is "no match"; 2 is an error such as a bad regex.
    let status = child.wait().map_err(|e| fail(e.to_string()))?;
    if status.code() == Some(2) && hits.is_empty() {
        return Err(Error::Usage(format!("rg rejected the pattern {pattern:?}")));
    }
    let total = hits.len();
    Ok((spread(hits), total))
}

/// The first [`MAX_HITS`], taken round-robin across repos so one busy repo cannot hide
/// the others, then in repo, path, line order.
fn spread(mut hits: Vec<Hit>) -> Vec<Hit> {
    hits.sort_by(|a, b| (&a.repo, &a.path, a.line).cmp(&(&b.repo, &b.path, b.line)));
    let mut by_repo: Vec<std::collections::VecDeque<Hit>> = Vec::new();
    for h in hits {
        match by_repo.last_mut() {
            Some(q) if q.front().is_some_and(|f| f.repo == h.repo) => q.push_back(h),
            _ => by_repo.push(std::collections::VecDeque::from([h])),
        }
    }
    let mut out = Vec::new();
    while out.len() < MAX_HITS && by_repo.iter().any(|q| !q.is_empty()) {
        for q in &mut by_repo {
            if out.len() < MAX_HITS
                && let Some(h) = q.pop_front()
            {
                out.push(h);
            }
        }
    }
    out.sort_by(|a, b| (&a.repo, &a.path, a.line).cmp(&(&b.repo, &b.path, b.line)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_respect_segments() {
        assert!(glob("*", "drover"));
        assert!(!glob("*", "group/drover"));
        assert!(glob("**", "group/drover"));
        assert!(glob("archive/**", "archive/old/x"));
        assert!(glob("**/x", "a/b/x"));
        assert!(glob("**/x", "x"));
        assert!(glob("op?lin*", "opalin-job"));
        assert!(!glob("opalin", "opalin2"));
    }

    #[test]
    fn reads_the_toml_subset() {
        let c = Config::parse(
            "roots = [\"~/Developer\", # main\n  \"~/src\"]\nexclude = [\"*.old\"]\n",
        )
        .unwrap();
        assert_eq!(c.roots, ["~/Developer", "~/src"]);
        assert_eq!(c.exclude, ["*.old"]);
        assert!(c.include.is_empty());
        assert_eq!(Config::parse("roots = [\"/abs\"]").unwrap_err().0, 1);
        assert_eq!(Config::parse("x = []").unwrap_err().0, 1);
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn remotes_lose_credentials() {
        assert_eq!(
            without_userinfo("https://u:tok@github.com/a/b"),
            "https://github.com/a/b"
        );
        assert_eq!(
            without_userinfo("git@github.com:a/b.git"),
            "git@github.com:a/b.git"
        );
        assert_eq!(canonical_url("git@github.com:A/b.git"), "github.com/a/b");
        assert_eq!(canonical_url("https://github.com/a/b/"), "github.com/a/b");
    }
}
