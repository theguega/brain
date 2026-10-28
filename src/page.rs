//! C2: wiki pages, `pages/<slug>.md`: a note (see [`crate::note`]) whose typed relations
//! are wikilink-valued frontmatter properties, `brain link` adding them.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::id::{DocId, Rel, Slug};
use crate::note;
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
    /// `[[<slug>]]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Other names: an exact match in `find` goes straight to the page.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Obsidian tags.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
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

#[must_use]
pub fn path(slug: &Slug) -> PathBuf {
    Path::new("pages").join(format!("{slug}.md"))
}

/// Adds `[[dst]]` to the page's `rel` property, a block list. `false` when it is there.
/// `dst` is a doc's directory name or a page slug: what the wikilink names.
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

/// Whether the page schema makes `rel` one link (`superseded_by: "[[slug]]"`), not a list.
fn single_valued(rel: &Rel) -> bool {
    serde_json::from_str::<Value>(crate::contract::PAGE).is_ok_and(|schema| {
        schema
            .pointer(&format!("/properties/{rel}/type"))
            .is_some_and(|t| t == "string")
    })
}

/// The page text with `[[dst]]` added under `rel`, `None` if it is there already, or the
/// file line and what is wrong. Only the `rel` key's lines are rewritten, as a block list
/// (or the one link the schema wants), whatever form they had; the rest of the page keeps
/// its comments, key order and line endings. The result is parsed back before it is
/// returned, so a page is never left with YAML that does not read.
fn with_link(text: &str, dst: &str, rel: &Rel) -> Result<Option<String>, (usize, String)> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let want = note::wikilink(dst);
    let single = single_valued(rel);
    let quoted = |s: &str| Value::String(s.to_owned()).to_string();
    let block = |items: &[String]| match items {
        [one] if single => format!("{rel}: {}", quoted(one)),
        _ => std::iter::once(format!("{rel}:"))
            .chain(items.iter().map(|i| format!("  - {}", quoted(i))))
            .collect::<Vec<_>>()
            .join(eol),
    };
    let Some((yaml, body)) = note::split(text) else {
        if note::opens(text) {
            return Err((1, "the frontmatter has no closing `---`".to_owned()));
        }
        let value = block(std::slice::from_ref(&want));
        return Ok(Some(format!("---{eol}{value}{eol}---{eol}{text}")));
    };
    // File lines are YAML lines plus the opening `---`.
    let fm = note::parse(yaml).map_err(|(line, why)| (line + 1, why))?;
    let mut lines: Vec<&str> = yaml.lines().collect();
    let at = lines.iter().position(|l| {
        l.strip_prefix(rel.as_str())
            .and_then(|r| r.strip_prefix(':'))
            .is_some_and(|r| r.is_empty() || r.starts_with([' ', '\t']))
    });
    let line = at.map_or(1, |i| i + 2);
    let links = |v: &Value| -> Option<Vec<String>> {
        match v {
            Value::Null => Some(Vec::new()),
            Value::String(s) => note::value_link(s).map(|_| vec![s.clone()]),
            Value::Array(a) => a
                .iter()
                .map(|v| {
                    v.as_str()
                        .filter(|s| note::value_link(s).is_some())
                        .map(str::to_owned)
                })
                .collect(),
            _ => None,
        }
    };
    let mut items = match fm.get(rel.as_str()) {
        None => Vec::new(),
        Some(v) => links(v)
            .ok_or_else(|| (line, format!("`{rel}` is not a wikilink or a list of them")))?,
    };
    if items.contains(&want) {
        return Ok(None);
    }
    if single && let Some(held) = items.first() {
        return Err((
            line,
            format!("`{rel}` holds one link and is already {held}"),
        ));
    }
    items.push(want.clone());
    let value = block(&items);
    match at {
        None => lines.push(&value),
        Some(i) => {
            let mut end = i + 1;
            while lines
                .get(end)
                .is_some_and(|l| l.trim().is_empty() || l.starts_with([' ', '\t', '-']))
            {
                end += 1;
            }
            while end > i + 1 && lines.get(end - 1).is_some_and(|l| l.trim().is_empty()) {
                end -= 1;
            }
            lines.splice(i..end, [value.as_str()]);
        }
    }
    let bom = if text.starts_with('\u{feff}') {
        "\u{feff}"
    } else {
        ""
    };
    let out = format!("{bom}---{eol}{}{eol}---{eol}{body}", lines.join(eol));
    let mut after = note::split(&out)
        .and_then(|(y, _)| note::parse(y).ok())
        .ok_or((
            line,
            format!("`{rel}` cannot be edited in place; edit it by hand"),
        ))?;
    let mut before = fm;
    let got = after.remove(rel.as_str());
    before.remove(rel.as_str());
    if before != after || got.as_ref().and_then(links) != Some(items) {
        return Err((
            line,
            format!("`{rel}` cannot be edited in place; edit it by hand"),
        ));
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked(text: &str, dst: &str, rel: &str) -> Result<Option<String>, (usize, String)> {
        with_link(text, dst, &Rel::parse(rel).unwrap())
    }

    #[test]
    fn link_adds_a_wikilink_property_in_any_form() {
        let page = "---\nkind: concept # c\nbuilds_on:\n  - \"[[a]]\"\n\ntitle: T\n---\nBody\n";
        assert_eq!(
            linked(page, "arxiv--2410.24164", "builds_on")
                .unwrap()
                .unwrap(),
            "---\nkind: concept # c\nbuilds_on:\n  - \"[[a]]\"\n  - \"[[arxiv--2410.24164]]\"\n\ntitle: T\n---\nBody\n"
        );
        assert_eq!(linked(page, "a", "builds_on").unwrap(), None);
        assert_eq!(
            linked(page, "brain", "project").unwrap().unwrap(),
            "---\nkind: concept # c\nbuilds_on:\n  - \"[[a]]\"\n\ntitle: T\nproject:\n  - \"[[brain]]\"\n---\nBody\n"
        );
        assert_eq!(linked(page, "x", "title").unwrap_err().0, 6);
        // What serde-saphyr writes (items at column 0), the flow form `wiki propose --help`
        // shows, a single link, and an empty value all become one block list.
        for form in [
            "rel:\n- \"[[a]]\"",
            "rel: [\"[[a]]\"]",
            "rel: \"[[a]]\"",
            "rel: [\"[[a]]\"] # c",
        ] {
            let text = format!("---\ntitle: x\n{form}\nz: 1\n---\nb\n");
            assert_eq!(
                linked(&text, "b", "rel").unwrap().unwrap(),
                "---\ntitle: x\nrel:\n  - \"[[a]]\"\n  - \"[[b]]\"\nz: 1\n---\nb\n",
                "{form}"
            );
        }
        assert_eq!(
            linked("---\nrel:\nz: 1\n---\n", "b", "rel")
                .unwrap()
                .unwrap(),
            "---\nrel:\n  - \"[[b]]\"\nz: 1\n---\n"
        );
        assert_eq!(
            linked("Body", "a", "rel").unwrap().unwrap(),
            "---\nrel:\n  - \"[[a]]\"\n---\nBody"
        );
    }

    #[test]
    fn superseded_by_is_one_link_as_the_schema_wants() {
        let page = "---\ntitle: x\n---\n";
        let one = linked(page, "beta", "superseded_by").unwrap().unwrap();
        assert_eq!(one, "---\ntitle: x\nsuperseded_by: \"[[beta]]\"\n---\n");
        assert_eq!(linked(&one, "beta", "superseded_by").unwrap(), None);
        assert_eq!(linked(&one, "gamma", "superseded_by").unwrap_err().0, 3);
    }
}
