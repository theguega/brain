//! Paper identifiers. Parsed once at the edge; past that, an id is proof of a valid shape.

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

/// Key for a paper that is not on arXiv, e.g. `pinocchio`. Never contains '.', so it
/// cannot collide with an [`ArxivId`].
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

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A library key: the directory name under `papers/`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum PaperId {
    Arxiv(ArxivId),
    Slug(Slug),
}

impl PaperId {
    /// Accepts a bare id, `arXiv:<id>`, a slug, or an arXiv / alphaXiv / Hugging Face
    /// paper URL (abs, pdf, html; any version).
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Ok(id) = ArxivId::parse(arxiv_tail(s)) {
            return Ok(Self::Arxiv(id));
        }
        Slug::parse(s)
            .map(Self::Slug)
            .map_err(|_| Error::BadRef(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Arxiv(id) => id.as_str(),
            Self::Slug(s) => s.as_str(),
        }
    }
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
string_newtype_impls!(ArxivId, Slug);

impl fmt::Display for PaperId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_references() {
        for s in [
            "2410.24164",
            "2410.24164v3",
            "arXiv:2410.24164",
            "https://arxiv.org/abs/2410.24164v2",
            "https://arxiv.org/pdf/2410.24164.pdf",
            "https://arxiv.org/html/2410.24164v1/",
            "https://huggingface.co/papers/2410.24164",
            "https://www.alphaxiv.org/abs/2410.24164?x=1",
        ] {
            assert_eq!(PaperId::parse(s).unwrap().as_str(), "2410.24164", "{s}");
        }
        assert!(matches!(
            PaperId::parse("pinocchio").unwrap(),
            PaperId::Slug(_)
        ));
        assert!(PaperId::parse("Pinocchio").is_err());
        assert!(PaperId::parse("https://example.com/x.pdf").is_err());
        assert!(PaperId::parse("241.24164").is_err());
    }
}
