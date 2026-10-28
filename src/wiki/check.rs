//! `brain lint`, wiki part (B18): deterministic checks on `pages/*.md` beyond the schema.
//!
//! A `[[<doc dir>]]` or `[[<doc dir>#<heading>]]` wikilink is a citation; any other
//! `[[wikilink]]` names a page.
//!
//! Errors: dead wikilinks and citations, uncited paragraphs and list items in concept and
//! synthesis pages outside `## Open questions`, cited ids missing from `sources[]`, pages
//! over [`MAX_TOKENS`]. A missing or long `summary` is the schema's. Warnings: orphans,
//! contradictions open over [`CONTRADICTION_DAYS`], active pages not updated in
//! [`STALE_DAYS`], frontmatter keys C2 does not name and that are not relations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::chunk;
use crate::contract;
use crate::error::{Error, Result};
use crate::id::DocId;
use crate::index::doc_spans;
use crate::lint::{self, Finding};
use crate::note;
use crate::search::norm;
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
        let Some((fm, body_at)) = note::split(&raw)
            .and_then(|(yaml, body)| Some((note::parse(yaml).ok()?, raw.len() - body.len())))
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
    // The page schema allows any key, so a vault's own keys warn instead of blocking.
    let schema: Value = serde_json::from_str(contract::PAGE).map_err(|source| Error::Decode {
        what: "the page schema",
        source,
    })?;
    let known = schema.get("properties").and_then(Value::as_object);
    let slugs: BTreeSet<&str> = pages.iter().map(|p| p.slug.as_str()).collect();
    let mut inbound: BTreeSet<String> = BTreeSet::new();
    let mut out = Checked::default();
    let mut spans: BTreeMap<DocId, Vec<String>> = BTreeMap::new();
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
        for (key, v) in &p.fm {
            if !known.is_some_and(|k| k.contains_key(key)) && !note::is_relation(v) {
                out.warnings.push(Finding {
                    file: p.file.clone(),
                    error: format!("unknown frontmatter key `{key}`"),
                });
            }
        }
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
        for (dst, _) in note::relations(&p.fm) {
            inbound_from(&mut inbound, &p.slug, &dst);
        }

        let body = p.raw.lines().skip(p.first - 1);
        let mut unsourced: BTreeSet<DocId> = BTreeSet::new();
        let mut blocks = Blocks::default();
        for (n, line) in (p.first..).zip(body) {
            if !blocks.line(n, line) {
                continue;
            }
            for link in note::links(line) {
                let shown = match link.heading {
                    Some(h) => format!("[[{}#{h}]]", link.target),
                    None => format!("[[{}]]", link.target),
                };
                let Some(id) = link.doc() else {
                    inbound_from(&mut inbound, &p.slug, link.target);
                    if !slugs.contains(link.target) {
                        error(Some(n), format!("dead link {shown}"));
                    }
                    continue;
                };
                if !vault.has_doc(&id) {
                    error(Some(n), format!("dead citation {shown}"));
                } else if let Some(span) = link.heading
                    && !has_span(vault, &mut spans, &id, span)
                {
                    error(Some(n), format!("dead citation {shown}: no such span"));
                } else if !sources.contains(&id) && unsourced.insert(id.clone()) {
                    error(Some(n), format!("{id} is cited but not in sources[]"));
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
                    "uncited paragraph: cite a doc as [[<doc-id>]]".to_owned(),
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

/// Whether `brain get <id>#<span>` would find the span: a chunk number or a heading.
/// A doc that cannot be read has none; lint reports the doc itself.
fn has_span(
    vault: &Vault,
    cache: &mut BTreeMap<DocId, Vec<String>>,
    id: &DocId,
    span: &str,
) -> bool {
    let headings = cache
        .entry(id.clone())
        .or_insert_with(|| doc_spans(vault, id).unwrap_or_default());
    let span = span.trim();
    match span.parse::<usize>() {
        Ok(n) => (1..=headings.len()).contains(&n),
        Err(_) => {
            let want = norm(span);
            headings.iter().any(|h| norm(h) == want)
        }
    }
}

/// Paragraphs and list items, each with whether it makes a claim and cites a doc.
/// Code (fenced or indented), `$$` math, HTML comments, headings, and blocks of only
/// links or images are not claims, and `## Open questions` is exempt down to the next
/// heading at its level.
#[derive(Default)]
struct Blocks {
    raw: Option<Raw>,
    comment: bool,
    /// The level of the open-questions heading we are under.
    open: Option<usize>,
    cur: Option<Block>,
    uncited: Vec<usize>,
}

/// Lines that are not prose until their closing line.
enum Raw {
    /// The fence's char and length: it closes with at least as many of the same char.
    Fence(char, usize),
    Math,
}

struct Block {
    line: usize,
    cited: bool,
    claims: bool,
    code: bool,
}

impl Blocks {
    /// Feeds one line; `false` when it is not prose (code, math, a comment).
    fn line(&mut self, n: usize, line: &str) -> bool {
        match &self.raw {
            Some(Raw::Fence(c, len)) => {
                let t = line.trim();
                if t.len() >= *len && t.chars().all(|x| x == *c) {
                    self.raw = None;
                }
                return false;
            }
            Some(Raw::Math) => {
                if line.trim_end().ends_with("$$") {
                    self.raw = None;
                }
                return false;
            }
            None => {}
        }
        let blank = line.trim().is_empty();
        let text = self.uncomment(line);
        let t = text.trim();
        if t.is_empty() {
            if blank {
                self.flush();
            }
            return false;
        }
        if let Some(fence) = fence(t) {
            self.flush();
            self.raw = Some(fence);
            return false;
        }
        if let Some(rest) = t.strip_prefix("$$") {
            self.flush();
            if !rest.trim_end().ends_with("$$") {
                self.raw = Some(Raw::Math);
            }
            return false;
        }
        if is_rule(t) {
            self.flush();
            return true;
        }
        if let Some((level, heading)) = heading(t) {
            self.flush();
            if self.open.is_some_and(|l| level <= l) {
                self.open = None;
            }
            if heading
                .trim_end_matches(':')
                .trim_end()
                .eq_ignore_ascii_case("open questions")
            {
                self.open = Some(level);
            }
            return true;
        }
        let indented = text.starts_with("    ") || text.starts_with('\t');
        if self.cur.is_none() && indented {
            self.cur = Some(Block {
                line: n,
                cited: false,
                claims: false,
                code: true,
            });
        }
        if self.cur.as_ref().is_some_and(|b| b.code) {
            return false;
        }
        if is_item(t) {
            self.flush();
        }
        let cited = note::links(t).iter().any(|l| l.doc().is_some());
        let claims = has_words(t);
        let b = self.cur.get_or_insert(Block {
            line: n,
            cited: false,
            claims: false,
            code: false,
        });
        b.cited |= cited;
        b.claims |= claims;
        true
    }

    /// The line without `<!-- ... -->`, which may span lines.
    fn uncomment(&mut self, line: &str) -> String {
        let mut out = String::new();
        let mut rest = line;
        loop {
            if self.comment {
                match rest.find("-->") {
                    Some(i) => {
                        rest = rest.get(i + 3..).unwrap_or_default();
                        self.comment = false;
                    }
                    None => return out,
                }
            } else {
                match rest.find("<!--") {
                    Some(i) => {
                        out.push_str(rest.get(..i).unwrap_or_default());
                        rest = rest.get(i + 4..).unwrap_or_default();
                        self.comment = true;
                    }
                    None => {
                        out.push_str(rest);
                        return out;
                    }
                }
            }
        }
    }

    fn flush(&mut self) {
        if let Some(b) = self.cur.take()
            && !b.code
            && b.claims
            && !b.cited
            && self.open.is_none()
        {
            self.uncited.push(b.line);
        }
    }

    fn finish(mut self) -> Vec<usize> {
        self.flush();
        self.uncited
    }
}

/// A fence opening: 3 or more backticks or tildes.
fn fence(t: &str) -> Option<Raw> {
    let c = t.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = t.chars().take_while(|x| *x == c).count();
    (len >= 3).then_some(Raw::Fence(c, len))
}

/// Whether a line holds words once list markers, links, images and citations are gone.
fn has_words(t: &str) -> bool {
    let t = item_text(t);
    let t = strip(&strip(t, "![", ")"), "[[", "]]");
    t.chars().any(char::is_alphanumeric)
}

/// `s` without each `open ... close` span.
fn strip(s: &str, open: &str, close: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(open) {
        out.push_str(rest.get(..i).unwrap_or_default());
        let after = rest.get(i + open.len()..).unwrap_or_default();
        match after.find(close) {
            Some(end) => rest = after.get(end + close.len()..).unwrap_or_default(),
            None => {
                rest = after;
                break;
            }
        }
    }
    out.push_str(rest);
    out
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

fn is_item(t: &str) -> bool {
    item_text(t).len() < t.len()
}

/// The text after a list marker: `- `, `* `, `+ `, `1. ` or `1) `; `t` if none.
fn item_text(t: &str) -> &str {
    if let Some(rest) = ["- ", "* ", "+ "].iter().find_map(|m| t.strip_prefix(m)) {
        return rest;
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    t.get(digits..)
        .filter(|_| digits > 0)
        .and_then(|r| r.strip_prefix(". ").or_else(|| r.strip_prefix(") ")))
        .unwrap_or(t)
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
        let body = "# Pi0\n\nA flow policy [[arxiv--2410.24164]].\nIt chunks.\n\nUncited claim.\n\n- cited [[arxiv--2410.24164#Method]]\n- uncited [[pi0]]\n\n```\ncode\n```\n\n## Open questions\n\nWhy?\n\n### Sub\n\nStill open.\n\n## Next\n\nNot exempt.\n";
        assert_eq!(uncited(body), [6, 9, 25]);
    }

    #[test]
    fn code_math_comments_and_link_only_blocks_are_not_claims() {
        let body = "````\n```\nnot closed by three\n````\n\n- [[pi0]]\n- [[jepa|JEPA]], ![fig](x.png)\n\n    indented code\n\n<!-- a note\nstill a note -->\n\n$$\nx = y\n$$\n\n$$ a = b $$\n\n## Open questions:\n\nWhy?\n\n## After\n\n- real claim\n";
        assert_eq!(uncited(body), [26]);
    }

    #[test]
    fn days_count_from_the_epoch() {
        assert_eq!(days("1970-01-01"), Some(0));
        assert_eq!(days("2026-10-27"), Some(20_753));
        assert_eq!(days("2000-03-01T00:00:00Z"), Some(11_017));
    }
}
