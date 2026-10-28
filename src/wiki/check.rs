//! `brain lint`, wiki part (B18): deterministic checks on `pages/*.md` beyond the schema.
//!
//! Errors: dead `[[wikilinks]]` and `[@<doc-id>#<span>]` citations, uncited paragraphs and
//! list items in concept and synthesis pages outside `## Open questions`, cited ids missing
//! from `sources[]`, pages over [`MAX_TOKENS`]. A missing or long `summary` is the schema's.
//! Warnings: orphans, contradictions open over [`CONTRADICTION_DAYS`], active pages not
//! updated in [`STALE_DAYS`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::chunk;
use crate::error::Result;
use crate::id::DocId;
use crate::lint::{self, Finding};
use crate::page;
use crate::vault::{Vault, git};

pub const MAX_TOKENS: usize = 1500;
pub const CONTRADICTION_DAYS: u64 = 14;
pub const STALE_DAYS: i64 = 90;
const DAY: u64 = 86_400;

#[derive(Debug, Default)]
pub struct Checked {
    pub errors: Vec<Finding>,
    pub warnings: Vec<Finding>,
}

struct Page {
    slug: String,
    file: PathBuf,
    raw: String,
    fm: Map<String, Value>,
    /// File line of the body's first line, 1-based.
    first: usize,
}

pub fn check(vault: &Vault) -> Result<Checked> {
    let mut pages = Vec::new();
    for name in lint::entries(vault, "pages")? {
        let Some(slug) = name.strip_suffix(".md") else {
            continue;
        };
        let file = Path::new("pages").join(&name);
        let raw = vault.read(&file)?.unwrap_or_default();
        // A page whose frontmatter does not parse is the schema check's to report.
        let Some((fm, body_at)) = page::split(&raw)
            .and_then(|(yaml, body)| Some((page::parse_yaml(yaml).ok()?, raw.len() - body.len())))
        else {
            continue;
        };
        let first = raw.get(..body_at).unwrap_or_default().matches('\n').count() + 1;
        pages.push(Page {
            slug: slug.to_owned(),
            file,
            raw,
            fm,
            first,
        });
    }
    let slugs: BTreeSet<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    let mut inbound: BTreeSet<String> = BTreeSet::new();
    let mut out = Checked::default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    for p in &pages {
        let mut error = |line: Option<usize>, why: String| {
            out.errors.push(Finding {
                file: p.file.clone(),
                error: line.map_or(why.clone(), |l| format!("line {l}: {why}")),
            });
        };
        let tokens = chunk::tokens(&p.raw);
        if tokens > MAX_TOKENS {
            error(
                None,
                format!("{tokens} tokens, over {MAX_TOKENS}: split the page"),
            );
        }
        let sources: BTreeSet<DocId> =
            p.fm.get("sources")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|s| DocId::parse(s.as_str()?).ok())
                .collect();
        let cited_required = matches!(
            p.fm.get("kind").and_then(Value::as_str),
            Some("concept" | "synthesis")
        );
        for link in
            p.fm.get("links")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        {
            if let Some(dst) = link.as_object().and_then(|o| o.values().next()?.as_str()) {
                inbound_from(&mut inbound, &p.slug, dst);
            }
        }

        let body = p.raw.lines().skip(p.first - 1);
        let mut unsourced: BTreeSet<DocId> = BTreeSet::new();
        let mut blocks = Blocks::default();
        for (n, line) in (p.first..).zip(body) {
            if !blocks.line(n, line) {
                continue;
            }
            for target in wikilinks(line) {
                inbound_from(&mut inbound, &p.slug, target);
                if !slugs.contains(target) && !is_doc(vault, target) {
                    error(Some(n), format!("dead link [[{target}]]"));
                }
            }
            for cite in citations(line) {
                let id = cite.split_once('#').map_or(cite, |(id, _)| id);
                match DocId::parse(id) {
                    Ok(id) if !vault.has_doc(&id) => {
                        error(Some(n), format!("dead citation [@{cite}]"));
                    }
                    Ok(id) if !sources.contains(&id) => {
                        if unsourced.insert(id.clone()) {
                            error(Some(n), format!("{id} is cited but not in sources[]"));
                        }
                    }
                    Ok(_) => {}
                    Err(_) => error(Some(n), format!("dead citation [@{cite}]: not a doc id")),
                }
            }
            if line.trim_start().starts_with("> [!contradiction]") {
                let age = committed_at(vault, &p.file, n).map_or(0, |t| now.saturating_sub(t));
                if age > CONTRADICTION_DAYS * DAY {
                    out.warnings.push(Finding {
                        file: p.file.clone(),
                        error: format!("line {n}: contradiction open for {} days", age / DAY),
                    });
                }
            }
        }
        if cited_required {
            for n in blocks.finish() {
                error(
                    Some(n),
                    "uncited paragraph: cite a doc as [@<doc-id>]".to_owned(),
                );
            }
        }

        let active = p.fm.get("status").and_then(Value::as_str) == Some("active");
        if active
            && let Some(updated) = p.fm.get("updated").and_then(Value::as_str)
            && let Some(day) = days(updated)
            && i64::try_from(now / DAY).unwrap_or(i64::MAX) - day > STALE_DAYS
        {
            out.warnings.push(Finding {
                file: p.file.clone(),
                error: format!("active, but not updated since {updated}"),
            });
        }
    }
    for p in &pages {
        if !inbound.contains(&p.slug) {
            out.warnings.push(Finding {
                file: p.file.clone(),
                error: "orphan: no page links to it".to_owned(),
            });
        }
    }
    Ok(out)
}

fn inbound_from(inbound: &mut BTreeSet<String>, src: &str, dst: &str) {
    if dst != src {
        inbound.insert(dst.to_owned());
    }
}

fn is_doc(vault: &Vault, target: &str) -> bool {
    DocId::parse(target).is_ok_and(|id| vault.has_doc(&id))
}

/// Paragraphs and list items, each with whether it cites a doc. Fenced code and headings
/// are not claims, and `## Open questions` is exempt down to the next heading at its level.
#[derive(Default)]
struct Blocks {
    fence: bool,
    /// The level of the open-questions heading we are under.
    open: Option<usize>,
    /// The current block's first line, and whether it cites anything yet.
    cur: Option<(usize, bool)>,
    uncited: Vec<usize>,
}

impl Blocks {
    /// Feeds one line; `false` when it is inside or opens a code fence.
    fn line(&mut self, n: usize, line: &str) -> bool {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            self.flush();
            self.fence = !self.fence;
            return false;
        }
        if self.fence {
            return false;
        }
        if t.is_empty() || is_rule(t) {
            self.flush();
            return true;
        }
        if let Some((level, text)) = heading(t) {
            self.flush();
            if self.open.is_some_and(|l| level <= l) {
                self.open = None;
            }
            if text.eq_ignore_ascii_case("open questions") {
                self.open = Some(level);
            }
            return true;
        }
        if is_item(t) {
            self.flush();
        }
        let cites = !citations(line).is_empty();
        match &mut self.cur {
            Some((_, cited)) => *cited |= cites,
            None => self.cur = Some((n, cites)),
        }
        true
    }

    fn flush(&mut self) {
        if let Some((n, false)) = self.cur.take()
            && self.open.is_none()
        {
            self.uncited.push(n);
        }
    }

    fn finish(mut self) -> Vec<usize> {
        self.flush();
        self.uncited
    }
}

/// `# Heading` (1 to 6 `#` then a space), as its level and text.
fn heading(t: &str) -> Option<(usize, &str)> {
    let level = t.bytes().take_while(|b| *b == b'#').count();
    let rest = t.get(level..)?;
    ((1..=6).contains(&level) && rest.starts_with(' ')).then(|| (level, rest.trim()))
}

fn is_rule(t: &str) -> bool {
    t.len() >= 3 && b"-*_".iter().any(|c| t.bytes().all(|b| b == *c))
}

/// `- `, `* `, `+ `, or `1. ` / `1) `.
fn is_item(t: &str) -> bool {
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && t.get(digits..)
            .is_some_and(|r| r.starts_with(". ") || r.starts_with(") "))
}

/// `[[target]]`, `[[target|label]]`, `[[target#span]]`, as targets.
fn wikilinks(line: &str) -> Vec<&str> {
    between(line, "[[", "]]")
        .filter_map(|inner| {
            let t = inner.split(['|', '#']).next()?.trim();
            (!t.is_empty()).then_some(t)
        })
        .collect()
}

/// `[@<doc-id>]` or `[@<doc-id>#<span>]`, as `<doc-id>[#<span>]`.
fn citations(line: &str) -> Vec<&str> {
    between(line, "[@", "]").map(str::trim).collect()
}

fn between<'a>(line: &'a str, open: &'a str, close: &'a str) -> impl Iterator<Item = &'a str> {
    let mut rest = line;
    std::iter::from_fn(move || {
        let i = rest.find(open)?;
        rest = rest.get(i + open.len()..)?;
        let end = rest.find(close)?;
        let inner = rest.get(..end)?;
        rest = rest.get(end + close.len()..)?;
        Some(inner)
    })
}

/// When the line was last committed, in Unix seconds; `None` before its first commit.
fn committed_at(vault: &Vault, file: &Path, line: usize) -> Option<u64> {
    let range = format!("{line},{line}");
    let out = git(
        vault.root(),
        &["blame", "--porcelain", "-L", &range, "--", file.to_str()?],
    )
    .ok()?;
    let mut time = None;
    for l in out.lines() {
        if l.starts_with("0000000000000000000000000000000000000000 ") {
            return None;
        }
        if let Some(t) = l.strip_prefix("committer-time ") {
            time = t.parse().ok();
        }
    }
    time
}

/// Days since 1970-01-01 of a `YYYY-MM-DD[T...]` date (Hinnant's `days_from_civil`).
fn days(date: &str) -> Option<i64> {
    let y: i64 = date.get(..4)?.parse().ok()?;
    let m: i64 = date.get(5..7)?.parse().ok()?;
    let d: i64 = date.get(8..10)?.parse().ok()?;
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uncited(body: &str) -> Vec<usize> {
        let mut b = Blocks::default();
        for (n, l) in (1..).zip(body.lines()) {
            b.line(n, l);
        }
        b.finish()
    }

    #[test]
    fn every_paragraph_and_item_outside_open_questions_cites() {
        let body = "# Pi0\n\nA flow policy [@arxiv:2410.24164].\nIt chunks.\n\nUncited claim.\n\n- cited [@arxiv:2410.24164]\n- uncited\n\n```\ncode\n```\n\n## Open questions\n\nWhy?\n\n### Sub\n\nStill open.\n\n## Next\n\nNot exempt.\n";
        assert_eq!(uncited(body), [6, 9, 25]);
    }

    #[test]
    fn links_and_citations_parse() {
        assert_eq!(
            wikilinks("see [[pi0|π0]] and [[jepa#history]]"),
            ["pi0", "jepa"]
        );
        assert_eq!(
            citations("x [@arxiv:2410.24164#2] y [@web:a-b]"),
            ["arxiv:2410.24164#2", "web:a-b"]
        );
    }

    #[test]
    fn days_count_from_the_epoch() {
        assert_eq!(days("1970-01-01"), Some(0));
        assert_eq!(days("2026-10-27"), Some(20_753));
        assert_eq!(days("2000-03-01T00:00:00Z"), Some(11_017));
    }
}
