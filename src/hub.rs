//! Hugging Face Hub: paper markdown (arXiv's HTML rendering, no PDF needed),
//! AI summary/keywords, and the daily papers feed for `discover`.
//!
//! The Hub meters anonymously and by token: 100 markdown fetches per 5 minutes without one,
//! 200 with. `HF_TOKEN` in the environment or the CLI's own saved token lifts both buckets,
//! and is only ever sent to [`HUB`].

use std::path::PathBuf;

use serde::Deserialize;

use crate::doc::{Hub, HubCounts};
use crate::error::{Error, Result};
use crate::id::ArxivId;
use crate::net;
use crate::net::Auth;

const HUB: &str = "https://huggingface.co";

/// Some ids answer 200 with a few hundred bytes (an SVG filename as the "paper").
/// A real paper is never this short, so anything under it counts as a miss.
const MIN_MARKDOWN_BYTES: usize = 5000;

/// The bearer header for the Hub, or `None` when no token is configured.
fn auth() -> Option<(&'static str, String)> {
    let token = std::env::var("HF_TOKEN").ok().or_else(saved_token)?;
    Some(("Authorization", format!("Bearer {token}")))
}

fn saved_token() -> Option<String> {
    let file = std::env::var_os("HF_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/huggingface")))?
        .join("token");
    std::fs::read_to_string(file)
        .ok()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

/// The request's auth as [`net`] wants it.
fn auth_ref<'a>(token: &'a Option<(&'static str, String)>) -> Auth<'a> {
    token.as_ref().map(|(n, v)| (*n, v.as_str()))
}

/// Full-text markdown, or `None` when the Hub cannot serve this paper
/// (no HTML for most pre-2022 papers; not every id is indexed).
pub fn markdown(id: &ArxivId) -> Result<Option<String>> {
    let token = auth();
    let r = net::get_as(&format!("{HUB}/papers/{id}.md"), auth_ref(&token))?;
    let text = String::from_utf8_lossy(&r.body);
    let usable = (200..300).contains(&r.status) && text.len() >= MIN_MARKDOWN_BYTES;
    Ok(usable.then(|| text.into_owned()))
}

#[derive(Debug, Default, Deserialize)]
pub struct Info {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default, rename = "publishedAt")]
    pub published_at: String,
    #[serde(default)]
    pub ai_summary: String,
    #[serde(default)]
    pub ai_keywords: Vec<String>,
    #[serde(default, rename = "githubRepo")]
    pub github_repo: Option<String>,
    #[serde(default, rename = "projectPage")]
    pub project_page: Option<String>,
    #[serde(default)]
    pub upvotes: Option<u64>,
    #[serde(default, rename = "githubStars")]
    pub github_stars: Option<u64>,
    #[serde(default, rename = "linkedModels")]
    pub linked_models: Vec<Artifact>,
    #[serde(default, rename = "linkedDatasets")]
    pub linked_datasets: Vec<Artifact>,
    #[serde(default, rename = "linkedSpaces")]
    pub linked_spaces: Vec<Artifact>,
    #[serde(default, rename = "numTotalModels")]
    pub num_models: Option<usize>,
    #[serde(default, rename = "numTotalDatasets")]
    pub num_datasets: Option<usize>,
    #[serde(default, rename = "numTotalSpaces")]
    pub num_spaces: Option<usize>,
}

/// One linked model, dataset or Space: the id is all brain keeps.
#[derive(Debug, Deserialize)]
pub struct Artifact {
    pub id: String,
}

impl Info {
    /// The paper's `hub:` block, or `None` when the Hub listed no artifact and voted nothing.
    #[must_use]
    pub fn block(&self) -> Option<Hub> {
        let ids = |a: &[Artifact]| a.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        let block = Hub {
            models: ids(&self.linked_models),
            datasets: ids(&self.linked_datasets),
            spaces: ids(&self.linked_spaces),
            counts: HubCounts {
                models: self.num_models.unwrap_or(0),
                datasets: self.num_datasets.unwrap_or(0),
                spaces: self.num_spaces.unwrap_or(0),
            },
            upvotes: self.upvotes.filter(|n| *n > 0),
            stars: self.github_stars.filter(|n| *n > 0),
        };
        let empty = block.models.is_empty()
            && block.datasets.is_empty()
            && block.spaces.is_empty()
            && block.counts == HubCounts::default()
            && block.upvotes.is_none()
            && block.stars.is_none();
        (!empty).then_some(block)
    }
}

#[derive(Debug, Deserialize)]
pub struct Author {
    pub name: String,
}

/// Decode a Hub record; `Ok(None)` when the Hub does not index the paper.
pub fn info(id: &ArxivId) -> Result<Option<Info>> {
    let token = auth();
    let r = net::get_as(&format!("{HUB}/api/papers/{id}"), auth_ref(&token))?;
    if r.status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&r.status) {
        return Err(Error::Http {
            url: format!("{HUB}/api/papers/{id}"),
            status: r.status,
        });
    }
    decode(&r.body, "hub paper info").map(Some)
}

#[derive(Debug, Deserialize)]
pub struct Daily {
    pub paper: DailyPaper,
}

#[derive(Debug, Deserialize)]
pub struct DailyPaper {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default, rename = "publishedAt")]
    pub published_at: String,
    #[serde(default)]
    pub ai_keywords: Vec<String>,
}

/// The Hub's curated daily list for `date` (`YYYY-MM-DD`).
pub fn daily(date: &str) -> Result<Vec<Daily>> {
    let token = auth();
    let auth = auth_ref(&token);
    let r = net::get_as(
        &format!("{HUB}/api/daily_papers?date={date}&limit=100"),
        auth,
    )?;
    if !(200..300).contains(&r.status) {
        return Err(Error::Http {
            url: format!("{HUB}/api/daily_papers?date={date}"),
            status: r.status,
        });
    }
    decode(&r.body, "hub daily papers")
}

fn decode<T: serde::de::DeserializeOwned>(body: &[u8], what: &'static str) -> Result<T> {
    serde_json::from_slice(body).map_err(|source| Error::Decode { what, source })
}
