//! A markdown note: YAML frontmatter, then the body. Docs (C1) and pages (C2) are both
//! notes, read and written by one YAML crate (serde-saphyr), so Obsidian and brain agree.
//!
//! A frontmatter value that is a `[[wikilink]]`, or a list of them, is a typed relation:
//! the key is its `rel`. In the body, a `[[wikilink]]` to a doc is a citation.

use std::path::Path;

use serde::Serialize;
use serde::ser::SerializeMap as _;
use serde_json::Value;

use crate::doc::Fields;
use crate::error::{Error, Result};
use crate::id::DocId;

/// Written first, in this order, so Obsidian's properties panel opens on them.
const FIRST: &[&str] = &["id", "kind", "title", "aliases"];

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
#[must_use]
pub fn opens(text: &str) -> bool {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.lines().next().is_some_and(|l| l == "---")
}

/// The frontmatter as JSON, or the 1-based line (within the YAML) and what is wrong.
pub fn parse(yaml: &str) -> Result<Fields, (usize, String)> {
    match serde_saphyr::from_str::<Value>(yaml) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(Value::Null) => Ok(Fields::new()),
        Ok(_) => Err((1, "the frontmatter is not a mapping".to_owned())),
        Err(e) => {
            let line = e
                .location()
                .and_then(|l| usize::try_from(l.line()).ok())
                .unwrap_or(1);
            Err((line, e.without_snippet().to_string()))
        }
    }
}

/// The note's frontmatter and body; `path` names the file in errors.
pub fn read(text: &str, path: &Path) -> Result<(Fields, String)> {
    let (yaml, body) = split(text).ok_or_else(|| Error::Invalid {
        path: path.to_owned(),
        why: "no frontmatter".to_owned(),
    })?;
    let fm = parse(yaml).map_err(|(line, why)| Error::Frontmatter {
        path: path.to_owned(),
        line: line + 1,
        why,
    })?;
    Ok((fm, body.to_owned()))
}

/// `---\n<yaml>---\n<body>`, [`FIRST`] keys first, the rest sorted.
pub fn render(fm: &Fields, body: &str, path: &Path) -> Result<String> {
    let yaml = serde_saphyr::to_string(&Ordered(fm)).map_err(|e| Error::Invalid {
        path: path.to_owned(),
        why: format!("frontmatter: {e}"),
    })?;
    let nl = if yaml.ends_with('\n') { "" } else { "\n" };
    Ok(format!("---\n{yaml}{nl}---\n{body}"))
}

struct Ordered<'a>(&'a Fields);

impl Serialize for Ordered<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for k in FIRST {
            if let Some(v) = self.0.get(*k) {
                m.serialize_entry(k, v)?;
            }
        }
        for (k, v) in self.0.iter().filter(|(k, _)| !FIRST.contains(&k.as_str())) {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// `[[target]]`.
#[must_use]
pub fn wikilink(target: &str) -> String {
    format!("[[{target}]]")
}

/// One `[[target#heading|label]]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link<'a> {
    pub target: &'a str,
    pub heading: Option<&'a str>,
}

impl<'a> Link<'a> {
    /// The inside of `[[...]]`.
    fn parse(inner: &'a str) -> Option<Self> {
        let inner = inner.split('|').next().unwrap_or_default();
        let (target, heading) = match inner.split_once('#') {
            Some((t, h)) => (t, Some(h.trim()).filter(|h| !h.is_empty())),
            None => (inner, None),
        };
        let target = target.trim();
        (!target.is_empty()).then_some(Self { target, heading })
    }
}

/// What a wikilink names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Doc(DocId),
    Page(String),
}

impl Target {
    /// As the index stores it: a doc's id or a page's slug.
    #[must_use]
    pub fn dst(&self) -> String {
        match self {
            Self::Doc(id) => id.to_string(),
            Self::Page(slug) => slug.clone(),
        }
    }
}

/// How a wikilink resolves, in Obsidian's order: a doc's directory name, a page's slug,
/// then an alias (`aliases` in a doc's or page's frontmatter), matched exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved<'a> {
    Found(Target),
    /// An alias more than one note holds.
    Ambiguous(&'a [Target]),
    Unknown,
}

/// The names a vault's wikilinks can use, besides doc directory names.
#[derive(Clone, Debug, Default)]
pub struct Names {
    pages: std::collections::BTreeSet<String>,
    aliases: std::collections::BTreeMap<String, Vec<Target>>,
}

impl Names {
    pub fn add_page(&mut self, slug: &str) {
        self.pages.insert(slug.to_owned());
    }

    /// The note's `aliases`, each naming `target`.
    pub fn add_aliases(&mut self, fm: &Fields, target: &Target) {
        for a in fm
            .get("aliases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let held = self.aliases.entry(a.to_owned()).or_default();
            if !held.contains(target) {
                held.push(target.clone());
            }
        }
    }

    #[must_use]
    pub fn resolve(&self, target: &str) -> Resolved<'_> {
        if let Ok(id) = DocId::from_dir(target) {
            return Resolved::Found(Target::Doc(id));
        }
        if self.pages.contains(target) {
            return Resolved::Found(Target::Page(target.to_owned()));
        }
        match self.aliases.get(target).map(Vec::as_slice) {
            Some([one]) => Resolved::Found(one.clone()),
            Some(many) if !many.is_empty() => Resolved::Ambiguous(many),
            _ => Resolved::Unknown,
        }
    }

    /// The doc a link cites, if it names one.
    #[must_use]
    pub fn doc(&self, link: &Link<'_>) -> Option<DocId> {
        match self.resolve(link.target) {
            Resolved::Found(Target::Doc(id)) => Some(id),
            _ => None,
        }
    }

    /// What the index stores as `dst`: what it resolves to, else the target as written.
    #[must_use]
    pub fn dst(&self, link: &Link<'_>) -> String {
        match self.resolve(link.target) {
            Resolved::Found(t) => t.dst(),
            _ => link.target.to_owned(),
        }
    }
}

/// Every `[[...]]` in `text`, in order.
#[must_use]
pub fn links(text: &str) -> Vec<Link<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("[[") {
        rest = rest.get(i + 2..).unwrap_or_default();
        let Some(end) = rest.find("]]") else { break };
        out.extend(Link::parse(rest.get(..end).unwrap_or_default()));
        rest = rest.get(end + 2..).unwrap_or_default();
    }
    out
}

/// The link a whole value is, as Obsidian reads a property: `"[[target]]"`.
#[must_use]
pub fn value_link(v: &str) -> Option<Link<'_>> {
    Link::parse(v.trim().strip_prefix("[[")?.strip_suffix("]]")?)
}

/// Whether a value is a wikilink or a non-empty list of them: a relation.
#[must_use]
pub fn is_relation(v: &Value) -> bool {
    match v {
        Value::String(s) => value_link(s).is_some(),
        Value::Array(a) => {
            !a.is_empty() && a.iter().all(|v| v.as_str().and_then(value_link).is_some())
        }
        _ => false,
    }
}

/// The frontmatter's relations as `(dst, rel)`: each wikilink-valued key, once per link.
#[must_use]
pub fn relations(fm: &Fields, names: &Names) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (rel, v) in fm.iter().filter(|(_, v)| is_relation(v)) {
        let items = match v {
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            other => other.as_str().into_iter().collect::<Vec<_>>(),
        };
        out.extend(
            items
                .into_iter()
                .filter_map(value_link)
                .map(|l| (names.dst(&l), rel.clone())),
        );
    }
    out
}

/// The body's links as `(dst, rel)`, once each: `cites` for a doc, `mentions` otherwise.
#[must_use]
pub fn body_links(body: &str, names: &Names) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for l in links(body) {
        let rel = if names.doc(&l).is_some() {
            "cites"
        } else {
            "mentions"
        };
        let link = (names.dst(&l), rel.to_owned());
        if !out.contains(&link) {
            out.push(link);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn links_and_relations() {
        let ls = links("a [[x]] b [[y|Y]] [[arxiv--2410.24164#Method|m]] [[ ]]");
        let got: Vec<_> = ls.iter().map(|l| (l.target, l.heading)).collect();
        assert_eq!(
            got,
            [
                ("x", None),
                ("y", None),
                ("arxiv--2410.24164", Some("Method"))
            ]
        );
        let mut names = Names::default();
        let pi0 = Target::Doc(DocId::parse("arxiv:2410.24164").unwrap());
        let Value::Object(aliases) = json!({"aliases": ["π0", "VLA"]}) else {
            unreachable!()
        };
        names.add_aliases(&aliases, &pi0);
        let Value::Object(vla) = json!({"aliases": ["VLA"]}) else {
            unreachable!()
        };
        names.add_aliases(&vla, &Target::Page("vla".to_owned()));
        names.add_page("brain");
        assert_eq!(
            body_links(
                "[[pi0]] [[arxiv--2410.24164]] [[arxiv--2410.24164#2]] [[π0#Method]] [[VLA]] [[arxiv:2410.24164]]",
                &names
            ),
            [
                ("pi0".to_owned(), "mentions".to_owned()),
                ("arxiv:2410.24164".to_owned(), "cites".to_owned()),
                ("VLA".to_owned(), "mentions".to_owned()),
                ("arxiv:2410.24164".to_owned(), "mentions".to_owned()),
            ]
        );
        assert!(matches!(names.resolve("VLA"), Resolved::Ambiguous([_, _])));
        let fm = json!({
            "builds_on": ["[[arxiv--2504.16054]]"], "project": "[[brain]]",
            "title": "[not a link]", "tags": ["vla"], "empty": [],
        });
        let Value::Object(fm) = fm else {
            unreachable!()
        };
        assert_eq!(
            relations(&fm, &names),
            [
                ("arxiv:2504.16054".to_owned(), "builds_on".to_owned()),
                ("brain".to_owned(), "project".to_owned())
            ]
        );
    }

    #[test]
    fn renders_what_it_parses() {
        let Value::Object(fm) = json!({
            "abstract": "two\nlines", "title": "$π_0$: a: b", "kind": "paper", "id": "arxiv:1",
            "builds_on": ["[[arxiv--2504.16054]]"], "n": 1.5, "none": null, "e": [],
        }) else {
            unreachable!()
        };
        let text = render(&fm, "---\n# Body\n", Path::new("x.md")).unwrap();
        assert!(
            text.starts_with("---\nid: arxiv:1\nkind: paper\ntitle: "),
            "{text}"
        );
        assert!(text.contains("- \"[[arxiv--2504.16054]]\"\n"), "{text}");
        let (yaml, body) = split(&text).unwrap();
        assert_eq!(body, "---\n# Body\n");
        assert_eq!(parse(yaml).unwrap(), fm);
    }

    #[test]
    fn bad_yaml_names_its_line() {
        assert_eq!(parse("a: 1\nb: [x\nc: 2\n").unwrap_err().0, 3);
        assert_eq!(parse("k: 1\nk: 2").unwrap_err().0, 2);
        assert_eq!(parse("- a").unwrap_err().0, 1);
        assert_eq!(parse("").unwrap(), Fields::new());
        assert_eq!(
            split("---\r\na: 1\r\n---\r\nbody").unwrap(),
            ("a: 1\r\n", "body")
        );
        assert!(split("no frontmatter").is_none());
        assert_eq!(split("---\n---\nbody").unwrap(), ("", "body"));
    }
}
