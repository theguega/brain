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
use std::io::{BufRead, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::doc::Taint;
use crate::error::{Error, Result, io};
use crate::index::Source;
use crate::vault::git;

pub const FILE: &str = "repos.toml";
const DEFAULT_ROOT: &str = "~/Developer";
/// Directories below a root searched for checkouts.
const DEPTH: usize = 3;
/// The commits indexed per repo: the last ones reachable from HEAD, so an incremental
/// reindex and a full one agree, and rebased-away commits leave.
const COMMITS: usize = 500;
/// Markdown files bigger than this are skipped: they are generated, not written.
const MAX_DOC_BYTES: u64 = 1 << 20;
/// Chars kept of a commit message.
const MAX_MESSAGE: usize = 16_000;
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
    /// The last [`COMMITS`], or `None` when HEAD is the last indexed head.
    pub commits: Option<Vec<Source>>,
    /// Markdown files over [`MAX_DOC_BYTES`].
    pub skipped: Vec<String>,
    pub languages: Vec<String>,
    pub last_activity: String,
}

/// `None` for a checkout with no commit yet.
pub(crate) fn scan(repo: &Repo, last_head: Option<&str>) -> Result<Option<Scan>> {
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
    let mut skipped = Vec::new();
    for f in files.iter().filter(|f| is_doc(f)) {
        let p = repo.path.join(f);
        if fs::metadata(&p).is_ok_and(|m| m.len() > MAX_DOC_BYTES) {
            skipped.push((*f).to_owned());
            continue;
        }
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
    let commits = if last_head == Some(head.as_str()) {
        None
    } else {
        Some(commits(repo)?)
    };
    Ok(Some(Scan {
        head,
        remote,
        docs,
        commits,
        skipped,
        languages: languages(&files),
        last_activity,
    }))
}

fn is_doc(f: &str) -> bool {
    matches!(f, "README.md" | "AGENTS.md" | "DESIGN.md")
        || (f.starts_with("docs/") && f.ends_with(".md"))
}

fn commits(repo: &Repo) -> Result<Vec<Source>> {
    let args = [
        "log",
        "--no-merges",
        "--format=%H%x1f%cs%x1f%B%x1e",
        &format!("--max-count={COMMITS}"),
        "HEAD",
    ];
    let out = git(&repo.path, &args)?;
    Ok(out
        .split('\x1e')
        .filter_map(|rec| {
            let mut f = rec.trim_start_matches('\n').splitn(3, '\x1f');
            let (sha, date, msg) = (f.next()?, f.next()?, f.next()?.trim());
            let msg: String = msg.chars().take(MAX_MESSAGE).collect();
            let short = sha.get(..12)?;
            Some(Source {
                id: format!("commit:{}@{short}", repo.name),
                kind: "log".to_owned(),
                title: msg.lines().next().unwrap_or_default().to_owned(),
                date: date.to_owned(),
                meta: json!({"repo": repo.name, "sha": sha}),
                taint: Taint::Own,
                text: msg,
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

/// Matches counted before rg is stopped, and the time it gets: a pattern that hits
/// everything must not cost memory or minutes.
const MATCH_CAP: usize = 10_000;
const DEADLINE: Duration = Duration::from_secs(10);
/// Bytes read of one rg JSON line; a longer one still counts as a match.
const MAX_JSON_LINE: usize = 64 * 1024;
/// Bytes of rg's stderr kept for the error.
const MAX_STDERR: usize = 2000;

/// How many lines matched: all of them, or as many as were counted before the cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Total {
    Exact(usize),
    AtLeast(usize),
}

impl std::fmt::Display for Total {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exact(n) => write!(f, "{n}"),
            Self::AtLeast(n) => write!(f, "{n}+"),
        }
    }
}

enum Stop {
    Finished,
    Early,
}

/// `rg --json` across the repos: the first [`MAX_HITS`] (see [`spread`]) and the total.
/// Only the hits that can be printed are kept; rg is killed past [`MATCH_CAP`] or [`DEADLINE`].
pub fn code(repos: &[Repo], pattern: &str) -> Result<(Vec<Hit>, Total)> {
    if repos.is_empty() {
        return Ok((Vec::new(), Total::Exact(0)));
    }
    let fail = |detail: String| Error::Command {
        cmd: "rg".to_owned(),
        detail,
    };
    let mut child = Command::new("rg")
        .args(["--json", "--smart-case", "-e", pattern, "--"])
        .args(repos.iter().map(|r| &r.path))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(e.to_string()))?;
    let (out, err) = (child.stdout.take(), child.stderr.take());
    let stderr = std::thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut err) = err {
            // Drained to the end, so rg never blocks on a full pipe.
            let _ = (&mut err).take(MAX_STDERR as u64).read_to_end(&mut kept);
            let _ = std::io::copy(&mut err, &mut std::io::sink());
        }
        String::from_utf8_lossy(&kept).trim().to_owned()
    });
    let (tx, rx) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        let killed = match rx.recv_timeout(DEADLINE) {
            Ok(Stop::Finished) => false,
            Ok(Stop::Early) | Err(_) => child.kill().is_ok(),
        };
        (child.wait(), killed)
    });

    let mut kept: Vec<Vec<Hit>> = vec![Vec::new(); repos.len()];
    let mut count = 0;
    let read = (|| -> std::io::Result<bool> {
        let Some(out) = out else { return Ok(false) };
        let mut reader = BufReader::new(out);
        let mut line = Vec::new();
        while read_line_capped(&mut reader, &mut line, MAX_JSON_LINE)? {
            if !line.starts_with(br#"{"type":"match""#) {
                continue;
            }
            count += 1;
            if let Some((i, hit)) = parse_hit(&line, repos)
                && let Some(k) = kept.get_mut(i)
            {
                let at = k.partition_point(|h| (&h.path, h.line) <= (&hit.path, hit.line));
                if at < MAX_HITS {
                    k.insert(at, hit);
                    k.truncate(MAX_HITS);
                }
            }
            if count >= MATCH_CAP {
                return Ok(true);
            }
        }
        Ok(false)
    })();
    let capped = matches!(read, Ok(true));
    let _ = tx.send(if capped { Stop::Early } else { Stop::Finished });
    let (status, killed) = watchdog
        .join()
        .map_err(|_| fail("the watchdog thread died".to_owned()))?;
    let stderr = stderr.join().unwrap_or_default();
    read.map_err(|e| fail(e.to_string()))?;
    let status = status.map_err(|e| fail(e.to_string()))?;
    if !killed {
        // 0 and 1 are matches and no match; 2 with matches is a partial error (an unreadable file).
        match status.code() {
            Some(0 | 1) => {}
            Some(2) if count > 0 => {}
            Some(2) => return Err(Error::Usage(format!("rg: {stderr}"))),
            _ => return Err(fail(format!("{status}: {stderr}"))),
        }
    }
    let total = if killed || capped {
        Total::AtLeast(count)
    } else {
        Total::Exact(count)
    };
    Ok((spread(kept), total))
}

/// One line into `buf`, without its newline, keeping at most `max` bytes of it.
/// `false` at the end of input.
fn read_line_capped(r: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> std::io::Result<bool> {
    buf.clear();
    let mut any = false;
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return Ok(any);
        }
        any = true;
        let (line, used, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (chunk.get(..i).unwrap_or_default(), i + 1, true),
            None => (chunk, chunk.len(), false),
        };
        let room = max.saturating_sub(buf.len());
        buf.extend_from_slice(line.get(..room.min(line.len())).unwrap_or_default());
        r.consume(used);
        if done {
            return Ok(true);
        }
    }
}

/// The repo index and hit of one `match` message.
fn parse_hit(line: &[u8], repos: &[Repo]) -> Option<(usize, Hit)> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let data = v.get("data")?;
    let path = text_or_bytes(data.get("path")?)?;
    let text = text_or_bytes(data.get("lines")?)?;
    let i = repos
        .iter()
        .position(|r| Path::new(&path).starts_with(&r.path))?;
    let repo = repos.get(i)?;
    let rel = Path::new(&path)
        .strip_prefix(&repo.path)
        .map_or_else(|_| path.clone(), |p| p.to_string_lossy().into_owned());
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some((
        i,
        Hit {
            repo: repo.name.clone(),
            path: rel,
            line: data.get("line_number").and_then(Value::as_u64).unwrap_or(0),
            text: text.chars().take(MAX_LINE).collect(),
        },
    ))
}

/// rg's `{"text": ..}`, or `{"bytes": base64}` for data that is not UTF-8.
fn text_or_bytes(v: &Value) -> Option<String> {
    if let Some(t) = v.get("text").and_then(Value::as_str) {
        return Some(t.to_owned());
    }
    let raw = base64(v.get("bytes")?.as_str()?)?;
    Some(String::from_utf8_lossy(&raw).into_owned())
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0_u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// At most [`MAX_HITS`], taken round-robin across repos (each already in path, line
/// order) so one busy repo cannot hide the others, then in repo, path, line order.
fn spread(mut by_repo: Vec<Vec<Hit>>) -> Vec<Hit> {
    by_repo.iter_mut().for_each(|k| k.reverse());
    let mut out = Vec::new();
    while out.len() < MAX_HITS && by_repo.iter().any(|k| !k.is_empty()) {
        for k in &mut by_repo {
            if out.len() < MAX_HITS
                && let Some(h) = k.pop()
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
    fn base64_decodes_rg_bytes() {
        assert_eq!(base64("Y2Fm6SB4eXoK").unwrap(), b"caf\xe9 xyz\n");
        assert_eq!(base64("YQ==").unwrap(), b"a");
    }

    #[test]
    fn long_lines_are_cut_not_held() {
        let input = format!("{}\nshort\n", "x".repeat(100));
        let mut r = BufReader::with_capacity(8, input.as_bytes());
        let mut buf = Vec::new();
        assert!(read_line_capped(&mut r, &mut buf, 10).unwrap());
        assert_eq!(buf, b"xxxxxxxxxx");
        assert!(read_line_capped(&mut r, &mut buf, 10).unwrap());
        assert_eq!(buf, b"short");
        assert!(!read_line_capped(&mut r, &mut buf, 10).unwrap());
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
