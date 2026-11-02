//! Papers with Code (paperswithcode.co): `tldr`, venue and lineage. Its code links, tasks
//! and methods are not used: on π0 they are empty, too coarse, or wrong.
//!
//! `citation_count` is not stored either. PwC and alphaXiv disagree on it by up to a quarter
//! in both directions (π0: 3224 against 4022; diffusion-policy: 4769 against 4048), so one
//! number in the note would look authoritative while being an arbitrary pick between two.
//!
//! The API answers a trailing slash with a 307 to http and then a 301 back to https, so the
//! path is built without one: three requests per paper become one, which matters because the
//! host sheds load with 503 above roughly eight concurrent clients.

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::id::ArxivId;
use crate::net;

const API: &str = "https://paperswithcode.co/api/v1/papers";

#[derive(Debug, Default, Deserialize)]
pub struct Paper {
    /// Set on every record PwC has; an empty `{}` has none.
    #[serde(default)]
    pub arxiv_id: Option<String>,
    #[serde(default)]
    pub tldr: Option<String>,
    #[serde(default)]
    pub conference_name: Option<String>,
    /// Papers this one builds on; `null` when none.
    #[serde(default)]
    pub predecessor_papers: Option<Vec<Related>>,
    /// Papers that build on this one.
    #[serde(default)]
    pub successor_papers: Option<Vec<Related>>,
}

#[derive(Debug, Deserialize)]
pub struct Related {
    #[serde(default)]
    pub arxiv_id: Option<String>,
}

/// The record for `id`, or `None` if PwC does not know it.
pub fn paper(id: &ArxivId) -> Result<Option<Paper>> {
    let url = format!("{API}/{id}");
    let r = net::get(&url)?;
    match r.status {
        404 => Ok(None),
        200..300 => serde_json::from_slice(&r.body)
            .map(Some)
            .map_err(|source| Error::Decode {
                what: "papers with code",
                source,
            }),
        status => Err(Error::Http { url, status }),
    }
}
