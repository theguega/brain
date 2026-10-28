//! Papers with Code (paperswithcode.co): `tldr`, venue and lineage. Its code links,
//! tasks and methods are not used: on π0 they are empty, too coarse, or wrong.

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
    let url = format!("{API}/{id}/");
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
