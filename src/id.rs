//! Identifiers. Parsed once at the edge; past that, an id is proof of a valid shape.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// New-style arXiv id with the version stripped, e.g. `2410.24164`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ArxivId(String);

impl ArxivId {
    pub fn parse(s: &str) -> Result<Self> {
        let s = strip_version(s.trim());
        let ok = s.split_once('.').is_some_and(|(yymm, num)| {
            yymm.len() == 4 && all_digits(yymm) && (4..=5).contains(&num.len()) && all_digits(num)
        });
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(Error::BadRef(s.to_owned()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Lowercase ASCII letters, digits and '-', e.g. `pinocchio` or `2026-10-27-weekly-sync`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Slug(String);

impl Slug {
    pub fn parse(s: &str) -> Result<Self> {
        if is_slug(s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(Error::BadRef(s.to_owned()))
        }
    }

    /// Lenient: lowercases and turns runs of anything else into '-', capped at 64 chars.
    /// `None` if nothing is left.
    #[must_use]
    pub fn slugify(s: &str) -> Option<Self> {
        let out = slug_chars(s);
        Self::parse(out.get(..64).unwrap_or(&out).trim_end_matches('-')).ok()
    }

    /// [`Slug::slugify`], but a slug too long to keep whole is cut and ends in
    /// `-<sha256[:6]>`, so two long names that share a start stay apart.
    #[must_use]
    pub fn slugify_or_hash(s: &str, sha256_hex: &str) -> Option<Self> {
        let out = slug_chars(s);
        if out.len() <= 64 {
            Self::parse(&out).ok()
        } else {
            Self::hashed(&out, sha256_hex)
        }
    }

    /// `base`, cut to fit, then `-<sha256[:6]>`.
    fn hashed(base: &str, sha256_hex: &str) -> Option<Self> {
        let tag = sha256_hex.get(..6)?.to_ascii_lowercase();
        let base = base.get(..64 - 7).unwrap_or(base).trim_end_matches('-');
        Self::parse(&format!("{base}-{tag}")).ok()
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A doc id, derived from its source so it can never disagree with its content.
/// Printed as `<prefix>:<rest>`; its directory under `docs/` is `<prefix>--<rest>`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum DocId {
    /// `arxiv:2410.24164`, version dropped.
    Arxiv(ArxivId),
    /// `web:<slug of host and path>`
    Web(Slug),
    /// `file:<sha256[:8]>`
    File(Slug),
    /// `meeting:<date>-<slug>`
    Meeting(Slug),
    /// `artifact:<slug>`
    Artifact(Slug),
}

impl DocId {
    /// Accepts `<prefix>:<rest>`, and for papers a bare arXiv id, `arXiv:<id>`, or an
    /// arXiv / alphaXiv / Hugging Face paper URL (abs, pdf, html; any version).
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let bad = || Error::BadRef(s.to_owned());
        let slug = |rest: &str| Slug::parse(rest).map_err(|_| bad());
        match s.split_once(':') {
            Some(("arxiv", rest)) => ArxivId::parse(rest).map(Self::Arxiv).map_err(|_| bad()),
            Some(("web", rest)) => slug(rest).map(Self::Web),
            Some(("file", rest))
                if rest.len() == 8 && rest.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                slug(rest).map(Self::File)
            }
            Some(("meeting", rest)) => slug(rest).map(Self::Meeting),
            Some(("artifact", rest)) => slug(rest).map(Self::Artifact),
            _ => ArxivId::parse(arxiv_tail(s))
                .map(Self::Arxiv)
                .map_err(|_| bad()),
        }
    }

    /// Inverse of [`DocId::dir`].
    pub fn from_dir(name: &str) -> Result<Self> {
        let (prefix, rest) = name
            .split_once("--")
            .ok_or_else(|| Error::BadRef(name.to_owned()))?;
        Self::parse(&format!("{prefix}:{rest}"))
    }

    /// `web:` id for a [`canonical_url`]: host (without `www.`), path and query, slugified.
    /// A slug too long to keep whole ends in `-<sha256[:6]>` of the canonical URL.
    #[must_use]
    pub fn web(canonical: &str, sha256_hex: &str) -> Option<Self> {
        Slug::slugify_or_hash(host_path(canonical), sha256_hex).map(Self::Web)
    }

    #[must_use]
    pub fn file(sha256_hex: &str) -> Option<Self> {
        let short = sha256_hex.get(..8)?.to_ascii_lowercase();
        Self::parse(&format!("file:{short}")).ok()
    }

    /// `day` is `YYYY-MM-DD`.
    #[must_use]
    pub fn meeting(day: &str, title: &str) -> Option<Self> {
        Slug::slugify(&format!("{day} {title}")).map(Self::Meeting)
    }

    /// `sha256_hex` of the HTML ends a title too long to keep whole.
    #[must_use]
    pub fn artifact(title: &str, sha256_hex: &str) -> Option<Self> {
        Slug::slugify_or_hash(title, sha256_hex).map(Self::Artifact)
    }

    /// The id with `-<sha256[:6]>` appended, for a source whose id another doc holds.
    /// `None` for ids that are already content-derived.
    #[must_use]
    pub fn hashed(&self, sha256_hex: &str) -> Option<Self> {
        match self {
            Self::Web(s) => Slug::hashed(s.as_str(), sha256_hex).map(Self::Web),
            Self::Artifact(s) => Slug::hashed(s.as_str(), sha256_hex).map(Self::Artifact),
            Self::Arxiv(_) | Self::File(_) | Self::Meeting(_) => None,
        }
    }

    #[must_use]
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::Arxiv(_) => "arxiv",
            Self::Web(_) => "web",
            Self::File(_) => "file",
            Self::Meeting(_) => "meeting",
            Self::Artifact(_) => "artifact",
        }
    }

    #[must_use]
    pub fn rest(&self) -> &str {
        match self {
            Self::Arxiv(id) => id.as_str(),
            Self::Web(s) | Self::File(s) | Self::Meeting(s) | Self::Artifact(s) => s.as_str(),
        }
    }

    /// Directory name under `docs/`: the id with `:` replaced by `--`.
    #[must_use]
    pub fn dir(&self) -> String {
        format!("{}--{}", self.prefix(), self.rest())
    }
}

impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.prefix(), self.rest())
    }
}

impl TryFrom<String> for DocId {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        Self::parse(&s)
    }
}

impl From<DocId> for String {
    fn from(id: DocId) -> Self {
        id.to_string()
    }
}

/// A link relation, e.g. `builds_on`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Rel(String);

impl Rel {
    pub const BUILDS_ON: &str = "builds_on";

    pub fn parse(s: &str) -> Result<Self> {
        let ok = (1..=64).contains(&s.len())
            && s.starts_with(|c: char| c.is_ascii_lowercase())
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(Error::BadRel(s.to_owned()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Query params that track a click rather than name a page, besides `utm_*`.
const TRACKING: &[&str] = &["fbclid", "gclid", "mc_cid", "mc_eid", "ref", "ref_src"];

/// The URL as `https://`, without `www.`, its fragment and tracking params; the other
/// params keep their order.
#[must_use]
pub fn canonical_url(url: &str) -> String {
    let url = url.split_once('#').map_or(url, |(u, _)| u);
    let url = format!("https://{}", host_path(url));
    let Some((base, query)) = url.split_once('?') else {
        return url;
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| {
            let key = p.split_once('=').map_or(*p, |(k, _)| k);
            !p.is_empty() && !key.starts_with("utm_") && !TRACKING.contains(&key)
        })
        .collect();
    if kept.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

/// A URL without its scheme and `www.`.
pub(crate) fn host_path(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.strip_prefix("www.").unwrap_or(rest)
}

/// Lowercase ASCII alphanumerics, with runs of anything else as one inner '-'; uncapped.
fn slug_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

/// Last path segment of a URL-ish reference, minus `arXiv:`, `.pdf`, query and fragment.
fn arxiv_tail(s: &str) -> &str {
    let s = s
        .split(['?', '#'])
        .next()
        .unwrap_or(s)
        .trim_end_matches('/');
    let s = s.rsplit('/').next().unwrap_or(s);
    let s = s.strip_suffix(".pdf").unwrap_or(s);
    s.get(..6)
        .filter(|p| p.eq_ignore_ascii_case("arxiv:"))
        .and_then(|_| s.get(6..))
        .unwrap_or(s)
}

fn strip_version(s: &str) -> &str {
    match s.rsplit_once('v') {
        Some((base, v)) if !v.is_empty() && all_digits(v) && base.contains('.') => base,
        _ => s,
    }
}

fn all_digits(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

/// Lowercase ASCII letters, digits and '-', 1..=64 chars, starting with a letter or digit.
pub(crate) fn is_slug(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
}

/// Serde and `Display` for a validated `String` newtype with a `parse` constructor.
macro_rules! string_newtype_impls {
    ($($t:ty),*) => {$(
        impl TryFrom<String> for $t {
            type Error = $crate::error::Error;
            fn try_from(s: String) -> $crate::error::Result<Self> {
                Self::parse(&s)
            }
        }
        impl From<$t> for String {
            fn from(v: $t) -> String {
                v.0
            }
        }
        impl ::std::fmt::Display for $t {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    )*};
}
pub(crate) use string_newtype_impls;
string_newtype_impls!(ArxivId, Slug, Rel);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arxiv_references() {
        for s in [
            "2410.24164",
            "2410.24164v3",
            "arXiv:2410.24164",
            "arxiv:2410.24164",
            "https://arxiv.org/abs/2410.24164v2",
            "https://arxiv.org/pdf/2410.24164.pdf",
            "https://arxiv.org/html/2410.24164v1/",
            "https://huggingface.co/papers/2410.24164",
            "https://www.alphaxiv.org/abs/2410.24164?x=1",
        ] {
            assert_eq!(
                DocId::parse(s).unwrap().to_string(),
                "arxiv:2410.24164",
                "{s}"
            );
        }
        assert!(DocId::parse("pinocchio").is_err());
        assert!(DocId::parse("https://example.com/x.pdf").is_err());
        assert!(DocId::parse("241.24164").is_err());
    }

    #[test]
    fn ids_round_trip_through_dirs() {
        for s in [
            "arxiv:2410.24164",
            "web:example-com-blog-post",
            "file:0a1b2c3d",
            "meeting:2026-10-27-weekly-sync",
            "artifact:contracts-review",
        ] {
            let id = DocId::parse(s).unwrap();
            assert_eq!(id.to_string(), s);
            assert_eq!(DocId::from_dir(&id.dir()).unwrap(), id);
        }
        assert_eq!(
            DocId::parse("arxiv:2410.24164").unwrap().dir(),
            "arxiv--2410.24164"
        );
        assert!(DocId::parse("file:xyz").is_err());
        assert!(DocId::parse("web:Has Space").is_err());
    }

    #[test]
    fn derives_ids_from_sources() {
        let sha = "ABCDEF0123456789";
        let canon =
            canonical_url("https://www.example.com/blog/Post_1?utm_source=x&p=2&ref=hn#top");
        assert_eq!(canon, "https://example.com/blog/Post_1?p=2");
        let web = DocId::web(&canon, sha).unwrap();
        assert_eq!(web.to_string(), "web:example-com-blog-post-1-p-2");
        assert_eq!(
            canonical_url("https://x.com/a?utm_x=1&fbclid=2"),
            "https://x.com/a"
        );
        assert_eq!(canonical_url("http://www.x.com/a"), "https://x.com/a");
        assert_eq!(
            canonical_url("https://x.com/a?b=1&a=2"),
            "https://x.com/a?b=1&a=2"
        );

        let long = format!("https://example.com/{}", "a-".repeat(100));
        let id = DocId::web(&long, sha).unwrap();
        assert_eq!(id.rest().len(), 64);
        assert!(id.rest().ends_with("-abcdef"), "{id}");
        assert_eq!(DocId::parse(&id.to_string()).unwrap(), id);
        assert_eq!(id.hashed(sha).unwrap(), id, "a cut slug hashes once");
        let dash = DocId::artifact("Dashboard", sha).unwrap();
        assert_eq!(
            dash.hashed(sha).unwrap().to_string(),
            "artifact:dashboard-abcdef"
        );
        let m = DocId::meeting("2026-10-27", "Weekly sync!").unwrap();
        assert_eq!(m.to_string(), "meeting:2026-10-27-weekly-sync");
        assert_eq!(
            DocId::file("ABCDEF0123456789").unwrap().to_string(),
            "file:abcdef01"
        );
        assert!(Rel::parse("builds_on").is_ok());
        assert!(Rel::parse("Builds-on").is_err());
    }
}
