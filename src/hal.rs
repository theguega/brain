//! HAL (hal.science), the French open archive: authors, date and venue of a deposit,
//! by its id (`hal-01866228`), from the public search API.

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::net;

const API: &str = "https://api.archives-ouvertes.fr/search/";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub authors: Vec<String>,
    /// `YYYY-MM-DD`; `None` when HAL gives only a month or a year.
    pub date: Option<String>,
    pub venue: Option<String>,
}

/// A HAL lookup: `Ok(None)` when HAL has no such deposit.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Result<Option<Record>>;

/// `hal-01866228` from a deposit URL on any HAL portal, version dropped.
#[must_use]
pub fn id_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let (host, path) = rest.split_once('/')?;
    if !(host == "hal.science" || host.ends_with(".hal.science") || host.starts_with("hal.")) {
        return None;
    }
    let seg = path.split(['/', '?', '#']).next()?;
    let (prefix, num) = seg.split_once('-')?;
    let digits: String = num.chars().take_while(char::is_ascii_digit).collect();
    let tail = num.get(digits.len()..)?;
    let versioned = tail.is_empty()
        || tail
            .strip_prefix('v')
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()));
    (!digits.is_empty() && prefix.bytes().all(|b| b.is_ascii_lowercase()) && versioned)
        .then(|| format!("{prefix}-{digits}"))
}

#[derive(Deserialize)]
struct Search {
    response: Docs,
}

#[derive(Deserialize)]
struct Docs {
    docs: Vec<Hit>,
}

#[derive(Deserialize)]
struct Hit {
    #[serde(default, rename = "authFullName_s")]
    authors: Vec<String>,
    #[serde(default, rename = "producedDate_s")]
    produced: Option<String>,
    #[serde(default, rename = "conferenceTitle_s")]
    conference: Option<String>,
    #[serde(default, rename = "journalTitle_s")]
    journal: Option<String>,
}

pub fn record(hal_id: &str) -> Result<Option<Record>> {
    let url = format!(
        "{API}?q=halId_s:{hal_id}&fl=authFullName_s,producedDate_s,conferenceTitle_s,journalTitle_s&wt=json"
    );
    let body = net::get_ok(&url)?;
    let s: Search = serde_json::from_slice(&body).map_err(|source| Error::Decode {
        what: "hal",
        source,
    })?;
    Ok(s.response.docs.into_iter().next().map(|h| Record {
        authors: h.authors,
        date: h.produced.filter(|d| is_day(d)),
        venue: h.conference.or(h.journal).filter(|v| !v.is_empty()),
    }))
}

fn is_day(s: &str) -> bool {
    s.len() == 10
        && s.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hal_ids_from_urls() {
        for (url, id) in [
            (
                "https://laas.hal.science/hal-01866228v2",
                Some("hal-01866228"),
            ),
            (
                "https://hal.science/hal-01866228/document",
                Some("hal-01866228"),
            ),
            (
                "https://hal.archives-ouvertes.fr/tel-00123456",
                Some("tel-00123456"),
            ),
            ("https://www.skild.ai/blogs/s1", None),
            ("https://hal.science/about", None),
        ] {
            assert_eq!(id_of(url).as_deref(), id, "{url}");
        }
    }
}
