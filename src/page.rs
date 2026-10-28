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

/// Splits `---\n<yaml>---\n<body>` into the YAML and the body.
#[must_use]
pub fn split(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    if let Some(body) = rest.strip_prefix("---\n") {
        return Some(("", body));
    }
    let mut at = 0;
    while let Some(i) = rest.get(at..)?.find("\n---") {
        let end = at + i + 1;
        let after = rest.get(end + 3..)?;
        if after.is_empty() || after.starts_with('\n') {
            return Some((rest.get(..end)?, after.strip_prefix('\n').unwrap_or(after)));
        }
        at = end;
    }
    None
}

/// The frontmatter as JSON, or the 1-based line (within the YAML) and what is wrong with it.
pub fn parse_yaml(yaml: &str) -> Result<Map<String, Value>, (usize, String)> {
    let mut map = Map::new();
    let mut list: Option<String> = None;
    for (i, raw) in yaml.lines().enumerate() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let item = trimmed
            .strip_prefix("- ")
            .or((trimmed == "-").then_some(""));
        if let (Some(item), Some(key)) = (item, &list) {
            let v = match pair(item) {
                Some((k, v)) if !v.is_empty() => {
                    Value::Object(Map::from_iter([(k.to_owned(), scalar(v))]))
                }
                _ => scalar(item),
            };
            match map.get_mut(key) {
                Some(Value::Array(a)) => a.push(v),
                Some(slot) => *slot = Value::Array(vec![v]),
                None => {}
            }
            continue;
        }
        if line.starts_with([' ', '\t']) {
            return Err((i + 1, "nested values are not supported".to_owned()));
        }
        let (k, v) = pair(line).ok_or_else(|| (i + 1, "expected `key: value`".to_owned()))?;
        list = v.is_empty().then(|| k.to_owned());
        let v = match v.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            Some(inner) => Value::Array(flow_items(inner).into_iter().map(scalar).collect()),
            None if v.is_empty() => Value::Null,
            None => scalar(v),
        };
        map.insert(k.to_owned(), v);
    }
    Ok(map)
}

/// `key: value` or `key:`, with a bare key.
fn pair(s: &str) -> Option<(&str, &str)> {
    let (k, v) = match s.split_once(": ") {
        Some((k, v)) => (k, v.trim()),
        None => (s.strip_suffix(':')?, ""),
    };
    let bare = !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    bare.then_some((k, v))
}

fn flow_items(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quote) = (0, None);
    for (i, c) in s.char_indices() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            (',', None) => {
                out.extend(s.get(start..i));
                start = i + 1;
            }
            _ => {}
        }
    }
    out.extend(s.get(start..));
    out.into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn scalar(s: &str) -> Value {
    let s = s.trim();
    if s.starts_with('"') {
        return serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_owned()));
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return Value::String(inner.replace("''", "'"));
    }
    let s = s.split_once(" #").map_or(s, |(v, _)| v.trim_end());
    match s {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" | "~" => Value::Null,
        _ => s.parse::<i64>().map_or_else(
            |_| Value::String(s.to_owned()),
            |n| Value::Number(Number::from(n)),
        ),
    }
}

/// Adds `rel: dst` to the page's `links`. `false` when it is already there.
pub fn link(vault: &Vault, page: &Slug, dst: &str, rel: &Rel) -> Result<bool> {
    let rel_path = path(page);
    let text = vault
        .read(&rel_path)?
        .ok_or_else(|| Error::NoPage(page.to_string()))?;
    let entry = format!("  - {rel}: {dst}\n");
    let Some((yaml, body)) = split(&text) else {
        vault.write(&rel_path, &format!("---\nlinks:\n{entry}---\n{text}"))?;
        return Ok(true);
    };
    let map = parse_yaml(yaml).map_err(|(line, why)| Error::Frontmatter {
        path: rel_path.clone(),
        line: line + 1,
        why,
    })?;
    let want = Value::Object(Map::from_iter([(
        rel.to_string(),
        Value::String(dst.to_owned()),
    )]));
    if let Some(Value::Array(links)) = map.get("links")
        && links.contains(&want)
    {
        return Ok(false);
    }
    let mut lines: Vec<String> = yaml.lines().map(str::to_owned).collect();
    match lines
        .iter()
        .position(|l| pair(l).is_some_and(|(k, _)| k == "links"))
    {
        None => lines.push(format!("links:\n{}", entry.trim_end())),
        Some(i) => {
            let value = lines.get(i).and_then(|l| pair(l)).map_or("", |(_, v)| v);
            if !matches!(value, "" | "[]") {
                return Err(Error::Frontmatter {
                    path: rel_path,
                    line: i + 2,
                    why: "`links` must be a block list for `brain link`".to_owned(),
                });
            }
            let mut end = i + 1;
            while lines
                .get(end)
                .is_some_and(|l| l.starts_with([' ', '\t', '-']))
            {
                end += 1;
            }
            lines.insert(end, entry.trim_end().to_owned());
            if let Some(l) = lines.get_mut(i) {
                "links:".clone_into(l);
            }
        }
    }
    vault.write(
        &rel_path,
        &format!("---\n{}\n---\n{body}", lines.join("\n")),
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert!(split("no frontmatter").is_none());
        assert_eq!(split("---\n---\nbody").unwrap(), ("", "body"));
    }
}
