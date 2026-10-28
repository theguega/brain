//! C2: wiki pages, `pages/<slug>.md` with YAML frontmatter.
//!
//! The frontmatter is read with a small YAML subset (what a person writes by hand and
//! what `brain link` writes): `key: scalar`, `key: [a, b]`, and block lists whose items
//! are scalars or single `rel: target` pairs. Anything else is an error naming the line.

use std::path::{Path, PathBuf};

use serde::ser::SerializeMap as _;
use serde::{Serialize, Serializer};
use serde_json::{Map, Number, Value};

use crate::error::{Error, Result};
use crate::id::{DocId, Rel, Slug};
use crate::vault::Vault;

/// C2 frontmatter.
#[derive(Clone, Debug, Serialize)]
pub struct Frontmatter {
    pub kind: PageKind,
    pub title: String,
    /// One line, at most 120 chars: what `brain pages` prints.
    pub summary: String,
    pub status: String,
    pub sources: Vec<DocId>,
    pub updated: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Slug>,
    /// Other names: an exact match in `find` goes straight to the page.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Obsidian tags.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PageKind {
    Person,
    Project,
    Decision,
    Concept,
    Synthesis,
}

/// Written as a single `rel: target` pair, the form a person types in YAML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub rel: Rel,
    /// A doc id or a page slug.
    pub dst: String,
}

impl Serialize for Link {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry(self.rel.as_str(), &self.dst)?;
        m.end()
    }
}

#[must_use]
pub fn path(slug: &Slug) -> PathBuf {
    Path::new("pages").join(format!("{slug}.md"))
}

/// Splits `---\n<yaml>---\n<body>` (LF or CRLF, BOM ignored) into the YAML and the body.
#[must_use]
pub fn split(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        let next = at + line.len();
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Some((rest.get(..at)?, rest.get(next..)?));
        }
        at = next;
    }
    None
}

/// Whether `text` opens a frontmatter block, closed or not.
fn opens_frontmatter(text: &str) -> bool {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.lines().next().is_some_and(|l| l == "---")
}

/// The frontmatter as JSON, or the 1-based line (within the YAML) and what is wrong with it.
/// Whatever the subset cannot read exactly is an error, never a guess.
pub fn parse_yaml(yaml: &str) -> Result<Map<String, Value>, (usize, String)> {
    let mut map = Map::new();
    let mut list: Option<String> = None;
    for (i, line) in yaml.lines().enumerate() {
        let err = |why: String| (i + 1, why);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.trim_start_matches(' ').starts_with('\t') {
            return Err(err("tabs cannot indent YAML".to_owned()));
        }
        let item = trimmed
            .strip_prefix("- ")
            .or((trimmed == "-").then_some(""));
        if let Some(item) = item {
            let Some(key) = &list else {
                return Err(err("a list item needs a `key:` line above it".to_owned()));
            };
            let v = match pair(item) {
                Some((k, v)) => {
                    let v = if v.is_empty() {
                        Value::Null
                    } else {
                        scalar(v).map_err(err)?
                    };
                    Value::Object(Map::from_iter([(k.to_owned(), v)]))
                }
                None => scalar(item).map_err(err)?,
            };
            match map.get_mut(key) {
                Some(Value::Array(a)) => a.push(v),
                Some(slot) => *slot = Value::Array(vec![v]),
                None => {}
            }
            continue;
        }
        if line.starts_with(' ') {
            return Err(err("nested values are not supported".to_owned()));
        }
        let (k, v) = pair(line).ok_or_else(|| err("expected `key: value`".to_owned()))?;
        if map.contains_key(k) {
            return Err(err(format!("duplicate key `{k}`")));
        }
        list = v.is_empty().then(|| k.to_owned());
        let v = if v.is_empty() {
            Value::Null
        } else {
            value(v).map_err(err)?
        };
        map.insert(k.to_owned(), v);
    }
    Ok(map)
}

/// `key: value` or `key:` (a comment after it counts as nothing), with a bare key.
fn pair(s: &str) -> Option<(&str, &str)> {
    let (k, v) = match s.split_once(": ") {
        Some((k, v)) => (k, v.trim()),
        None => (s.trim_end().strip_suffix(':')?, ""),
    };
    let v = if v.starts_with('#') { "" } else { v };
    let bare = !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    bare.then_some((k, v))
}

/// A value after `key: `: a flow list or a scalar.
fn value(s: &str) -> Result<Value, String> {
    let Some(inner) = s.strip_prefix('[') else {
        return scalar(s);
    };
    let end = close_of(inner, ']').ok_or("unclosed `[`")?;
    let (items, rest) = inner.split_at(end);
    trailing(rest.get(1..).unwrap_or_default())?;
    flow_items(items)?
        .into_iter()
        .map(scalar)
        .collect::<Result<_, _>>()
        .map(Value::Array)
}

/// Byte index of the first `close` outside quotes.
fn close_of(s: &str, close: char) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match (c, quote) {
            ('\\', Some('"')) if !escaped => {
                escaped = true;
                continue;
            }
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(q)) if c == q && !escaped => quote = None,
            (c, None) if c == close => return Some(i),
            _ => {}
        }
        escaped = false;
    }
    None
}

fn flow_items(s: &str) -> Result<Vec<&str>, String> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        let end = close_of(rest, ',');
        let item = rest
            .get(..end.unwrap_or(rest.len()))
            .unwrap_or_default()
            .trim();
        if item.starts_with(['[', '{']) {
            return Err("nested flow collections are not supported".to_owned());
        }
        if !item.is_empty() {
            out.push(item);
        }
        match end {
            Some(e) => rest = rest.get(e + 1..).unwrap_or_default(),
            None => return Ok(out),
        }
    }
}

/// What may follow a closed quote or flow list: nothing, or a comment.
fn trailing(rest: &str) -> Result<(), String> {
    let t = rest.trim_start();
    if t.is_empty() || (t.starts_with('#') && t.len() < rest.len()) {
        Ok(())
    } else {
        Err(format!("unexpected `{t}` after the value"))
    }
}

fn scalar(s: &str) -> Result<Value, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        return double_quoted(rest).map(Value::String);
    }
    if let Some(rest) = s.strip_prefix('\'') {
        return single_quoted(rest).map(Value::String);
    }
    if s.starts_with(['{', '[', '&', '*', '!', '|', '>', '%', '@', '`']) || s.starts_with("- ") {
        return Err(format!(
            "`{s}` is outside the supported YAML subset; quote it"
        ));
    }
    let s = s
        .find(" #")
        .map_or(s, |i| s.get(..i).unwrap_or(s).trim_end());
    if s.contains(": ") || s.ends_with(':') {
        return Err(format!("`{s}` holds `: `; quote it"));
    }
    Ok(match s {
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        "null" | "Null" | "NULL" | "~" => Value::Null,
        _ if let Ok(n) = s.parse::<i64>() => Value::Number(Number::from(n)),
        _ if s.contains(['.', 'e', 'E'])
            && s.bytes()
                .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) =>
        {
            let n = s
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .ok_or_else(|| format!("bad number `{s}`"))?;
            Value::Number(n)
        }
        _ => Value::String(s.to_owned()),
    })
}

/// The body of a `'...'` scalar (after the opening quote): `''` is a quote.
fn single_quoted(rest: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = rest.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '\'' {
            out.push(c);
        } else if chars.next_if(|(_, c)| *c == '\'').is_some() {
            out.push('\'');
        } else {
            trailing(rest.get(i + 1..).unwrap_or_default())?;
            return Ok(out);
        }
    }
    Err("unclosed `'`".to_owned())
}

/// The body of a `"..."` scalar (after the opening quote), YAML escapes decoded.
fn double_quoted(rest: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                trailing(rest.get(i + 1..).unwrap_or_default())?;
                return Ok(out);
            }
            '\\' => {
                let (_, e) = chars.next().ok_or("unclosed `\"`")?;
                let hex =
                    |n: usize, chars: &mut std::str::CharIndices<'_>| -> Result<char, String> {
                        let h: String = chars.by_ref().take(n).map(|(_, c)| c).collect();
                        u32::from_str_radix(&h, 16)
                            .ok()
                            .filter(|_| h.len() == n)
                            .and_then(char::from_u32)
                            .ok_or_else(|| format!("bad escape `\\{e}{h}`"))
                    };
                out.push(match e {
                    '0' => '\0',
                    'a' => '\u{7}',
                    'b' => '\u{8}',
                    't' | '\t' => '\t',
                    'n' => '\n',
                    'v' => '\u{b}',
                    'f' => '\u{c}',
                    'r' => '\r',
                    'e' => '\u{1b}',
                    ' ' => ' ',
                    '"' => '"',
                    '/' => '/',
                    '\\' => '\\',
                    'N' => '\u{85}',
                    '_' => '\u{a0}',
                    'L' => '\u{2028}',
                    'P' => '\u{2029}',
                    'x' => hex(2, &mut chars)?,
                    'u' => hex(4, &mut chars)?,
                    'U' => hex(8, &mut chars)?,
                    other => return Err(format!("unknown escape `\\{other}`")),
                });
            }
            c => out.push(c),
        }
    }
    Err("unclosed `\"`".to_owned())
}

/// Adds `rel: dst` to the page's `links`. `false` when it is already there.
pub fn link(vault: &Vault, page: &Slug, dst: &str, rel: &Rel) -> Result<bool> {
    let rel_path = path(page);
    vault.update(&rel_path, |text| {
        let text = text.ok_or_else(|| Error::NoPage(page.to_string()))?;
        with_link(&text, dst, rel).map_err(|(line, why)| Error::Frontmatter {
            path: rel_path.clone(),
            line,
            why,
        })
    })
}

/// The page text with `rel: dst` added, `None` if it is there already, or the file line
/// and what is wrong. Keeps the page's line endings.
fn with_link(text: &str, dst: &str, rel: &Rel) -> Result<Option<String>, (usize, String)> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let entry = format!("  - {rel}: {dst}");
    let Some((yaml, body)) = split(text) else {
        if opens_frontmatter(text) {
            return Err((1, "the frontmatter has no closing `---`".to_owned()));
        }
        return Ok(Some(format!(
            "---{eol}links:{eol}{entry}{eol}---{eol}{text}"
        )));
    };
    // File lines are YAML lines plus the opening `---`.
    let map = parse_yaml(yaml).map_err(|(line, why)| (line + 1, why))?;
    let want = Value::Object(Map::from_iter([(
        rel.to_string(),
        Value::String(dst.to_owned()),
    )]));
    if let Some(Value::Array(links)) = map.get("links")
        && links.contains(&want)
    {
        return Ok(None);
    }
    let mut lines: Vec<String> = yaml.lines().map(str::to_owned).collect();
    match lines
        .iter()
        .position(|l| pair(l).is_some_and(|(k, _)| k == "links"))
    {
        None => {
            lines.push("links:".to_owned());
            lines.push(entry);
        }
        Some(i) => {
            let value = lines.get(i).and_then(|l| pair(l)).map_or("", |(_, v)| v);
            if !matches!(value, "" | "[]") {
                return Err((
                    i + 2,
                    "`links` must be a block list for `brain link`".to_owned(),
                ));
            }
            let mut end = i + 1;
            while lines
                .get(end)
                .is_some_and(|l| l.starts_with([' ', '-']) || l.trim().is_empty())
            {
                end += 1;
            }
            lines.insert(end, entry);
            if let Some(l) = lines.get_mut(i) {
                "links:".clone_into(l);
            }
        }
    }
    let bom = if text.starts_with('\u{feff}') {
        "\u{feff}"
    } else {
        ""
    };
    Ok(Some(format!(
        "{bom}---{eol}{}{eol}---{eol}{body}",
        lines.join(eol)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(yaml: &str) -> Result<Value, (usize, String)> {
        parse_yaml(yaml).map(|m| m.get("k").cloned().unwrap_or(Value::Null))
    }

    #[test]
    fn reads_quotes_escapes_and_comments_exactly() {
        assert_eq!(one("k: \"X\" # c").unwrap(), json!("X"));
        assert_eq!(one("k: 'JEPA' # x").unwrap(), json!("JEPA"));
        assert_eq!(one("k: 'it''s'").unwrap(), json!("it's"));
        assert_eq!(one(r#"k: "\x41\u00e9\t\"""#).unwrap(), json!("Aé\t\""));
        assert_eq!(one("k: [a, b] # c").unwrap(), json!(["a", "b"]));
        assert_eq!(one("k: [\"a, b\", 'c']").unwrap(), json!(["a, b", "c"]));
        assert_eq!(one("k: a#b").unwrap(), json!("a#b"));
        assert_eq!(one("k: 1.5").unwrap(), json!(1.5));
        assert_eq!(one("k: # nothing").unwrap(), Value::Null);
        assert_eq!(one("k:\n  - a # c\n  - 'b'").unwrap(), json!(["a", "b"]));
    }

    #[test]
    fn rejects_what_it_cannot_read() {
        for (yaml, line) in [
            ("k: 1\nk: 2", 2),
            ("k: \"\\q\"", 1),
            ("k: \"open", 1),
            ("k: \"x\" y", 1),
            ("k: [a, [b]]", 1),
            ("k: [a, b] c", 1),
            ("k: {a: b}", 1),
            ("k: |", 1),
            ("k: &anchor x", 1),
            ("k: a: b", 1),
            ("k:\n\t- a", 2),
            ("- a", 1),
        ] {
            assert_eq!(parse_yaml(yaml).unwrap_err().0, line, "{yaml:?}");
        }
    }

    const PAGE: &str = "---\nkind: concept\ntitle: \"Action chunking\"\nsummary: Predict k actions at once # why\nsources: [arxiv:2410.24164, 'web:x-com']\nlinks:\n  - builds_on: arxiv:2410.24164\nyear: 2024\n---\n# Body\n---\n";

    #[test]
    fn parses_the_subset() {
        let (yaml, body) = split(PAGE).unwrap();
        assert_eq!(body, "# Body\n---\n");
        let m = parse_yaml(yaml).unwrap();
        assert_eq!(
            Value::Object(m),
            json!({
                "kind": "concept",
                "title": "Action chunking",
                "summary": "Predict k actions at once",
                "sources": ["arxiv:2410.24164", "web:x-com"],
                "links": [{"builds_on": "arxiv:2410.24164"}],
                "year": 2024,
            })
        );
        assert_eq!(parse_yaml("a: 1\n  b: 2\n").unwrap_err().0, 2);
        assert_eq!(
            split("---\r\na: 1\r\n---\r\nbody").unwrap(),
            ("a: 1\r\n", "body")
        );
        assert!(split("no frontmatter").is_none());
        assert_eq!(split("---\n---\nbody").unwrap(), ("", "body"));
    }
}
