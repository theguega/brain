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

/// The page text with `[[dst]]` added under `rel`, `None` if it is there already, or the
/// file line and what is wrong. Edits lines, not the parsed YAML, so the page keeps its
/// comments, key order and line endings.
fn with_link(text: &str, dst: &str, rel: &Rel) -> Result<Option<String>, (usize, String)> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let want = note::wikilink(dst);
    let entry = format!("  - {}", Value::String(want.clone()));
    let Some((yaml, body)) = note::split(text) else {
        if note::opens(text) {
            return Err((1, "the frontmatter has no closing `---`".to_owned()));
        }
        return Ok(Some(format!(
            "---{eol}{rel}:{eol}{entry}{eol}---{eol}{text}"
        )));
    };
    // File lines are YAML lines plus the opening `---`.
    let fm = note::parse(yaml).map_err(|(line, why)| (line + 1, why))?;
    let mut lines: Vec<String> = yaml.lines().map(str::to_owned).collect();
    let key = |l: &str| -> Option<String> {
        l.strip_prefix(rel.as_str())
            .and_then(|r| r.trim_end().strip_prefix(':'))
            .map(|v| v.trim().to_owned())
    };
    match fm.get(rel.as_str()) {
        None => {
            lines.push(format!("{rel}:"));
            lines.push(entry);
        }
        Some(v) => {
            let has = match v {
                Value::Array(a) => a.iter().any(|v| v.as_str() == Some(want.as_str())),
                Value::String(s) => *s == want,
                _ => false,
            };
            if has {
                return Ok(None);
            }
            let i = lines.iter().position(|l| key(l).is_some()).unwrap_or(0);
            let block = lines
                .get(i)
                .and_then(|l| key(l))
                .is_some_and(|v| v.is_empty() || v == "[]");
            if !(block && (v.is_null() || note::is_relation(v) || v == &Value::Array(vec![]))) {
                return Err((
                    i + 2,
                    format!("`{rel}` must be a block list of wikilinks for `brain link`"),
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
                *l = format!("{rel}:");
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

    fn linked(text: &str, dst: &str, rel: &str) -> Result<Option<String>, (usize, String)> {
        with_link(text, dst, &Rel::parse(rel).unwrap())
    }

    #[test]
    fn link_adds_a_wikilink_property() {
        let page = "---\nkind: concept # c\nbuilds_on:\n  - \"[[a]]\"\ntitle: T\n---\nBody\n";
        assert_eq!(
            linked(page, "arxiv--2410.24164", "builds_on")
                .unwrap()
                .unwrap(),
            "---\nkind: concept # c\nbuilds_on:\n  - \"[[a]]\"\n  - \"[[arxiv--2410.24164]]\"\ntitle: T\n---\nBody\n"
        );
        assert_eq!(linked(page, "a", "builds_on").unwrap(), None);
        assert_eq!(
            linked(page, "brain", "project").unwrap().unwrap(),
            "---\nkind: concept # c\nbuilds_on:\n  - \"[[a]]\"\ntitle: T\nproject:\n  - \"[[brain]]\"\n---\nBody\n"
        );
        assert_eq!(linked(page, "x", "title").unwrap_err().0, 5);
        assert_eq!(
            linked("---\nrel: [\"[[a]]\"]\n---\n", "b", "rel")
                .unwrap_err()
                .0,
            2
        );
        assert_eq!(
            linked("Body", "a", "rel").unwrap().unwrap(),
            "---\nrel:\n  - \"[[a]]\"\n---\nBody"
        );
    }
}
