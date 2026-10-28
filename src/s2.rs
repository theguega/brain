//! Semantic Scholar recommendations: "papers like these", seeded from the library.

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::id::ArxivId;
use crate::net;

const API: &str = "https://api.semanticscholar.org/recommendations/v1/papers";

#[derive(Debug)]
pub struct Recommendation {
    pub id: ArxivId,
    pub title: String,
    /// `YYYY-MM-DD`, or empty.
    pub published: String,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default, rename = "recommendedPapers")]
    papers: Vec<Raw>,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    title: String,
    #[serde(default, rename = "publicationDate")]
    date: Option<String>,
    #[serde(default, rename = "externalIds")]
    ids: Option<ExternalIds>,
}

#[derive(Deserialize)]
struct ExternalIds {
    #[serde(rename = "ArXiv")]
    arxiv: Option<String>,
}

/// Up to `limit` recent arXiv papers like `positive` and unlike `negative`.
/// Recommendations without an arXiv id are dropped.
pub fn recommend(
    positive: &[ArxivId],
    negative: &[ArxivId],
    limit: u32,
) -> Result<Vec<Recommendation>> {
    let ids = |v: &[ArxivId]| v.iter().map(|id| format!("arXiv:{id}")).collect::<Vec<_>>();
    let body = serde_json::json!({
        "positivePaperIds": ids(positive),
        "negativePaperIds": ids(negative),
    })
    .to_string();
    let url = format!("{API}?fields=title,publicationDate,externalIds&limit={limit}");
    let raw = net::post_json_ok(&url, &body)?;
    let r: Response = serde_json::from_slice(&raw).map_err(|source| Error::Decode {
        what: "semantic scholar",
        source,
    })?;
    Ok(r.papers
        .into_iter()
        .filter_map(|p| {
            let id = ArxivId::parse(&p.ids?.arxiv?).ok()?;
            Some(Recommendation {
                id,
                title: p.title,
                published: p.date.unwrap_or_default(),
            })
        })
        .collect())
}
