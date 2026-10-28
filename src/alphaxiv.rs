//! alphaXiv: the code repository when the Hub has none, and a summary.

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::id::ArxivId;
use crate::net;

const API: &str = "https://api.alphaxiv.org/papers/v3/legacy";

#[derive(Debug, Default)]
pub struct Info {
    pub code: Option<String>,
    pub summary: Option<String>,
}

#[derive(Deserialize)]
struct Response {
    paper: Paper,
}

#[derive(Deserialize)]
struct Paper {
    paper_group: Group,
}

#[derive(Deserialize)]
struct Group {
    #[serde(default)]
    resources: Option<Resources>,
    #[serde(default)]
    paper_summary: Option<Summary>,
}

#[derive(Deserialize)]
struct Resources {
    #[serde(default)]
    github: Option<Github>,
}

#[derive(Deserialize)]
struct Github {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
struct Summary {
    #[serde(default)]
    summary: Option<String>,
}

/// alphaXiv's record for `id`, or `None` if it does not know it.
pub fn info(id: &ArxivId) -> Result<Option<Info>> {
    let url = format!("{API}/{id}");
    let r = net::get(&url)?;
    match r.status {
        404 => Ok(None),
        200..300 => {
            let r: Response = serde_json::from_slice(&r.body).map_err(|source| Error::Decode {
                what: "alphaxiv",
                source,
            })?;
            let g = r.paper.paper_group;
            Ok(Some(Info {
                code: g
                    .resources
                    .and_then(|r| r.github?.url)
                    .filter(|u| !u.is_empty()),
                summary: g
                    .paper_summary
                    .and_then(|s| s.summary)
                    .filter(|s| !s.is_empty()),
            }))
        }
        status => Err(Error::Http { url, status }),
    }
}
