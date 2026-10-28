//! Hugging Face Hub: paper markdown (arXiv's HTML rendering, no PDF needed),
//! AI summary/keywords, and the daily papers feed for `discover`.

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::id::ArxivId;
use crate::net;

const HUB: &str = "https://huggingface.co";

/// Some ids answer 200 with a few hundred bytes (an SVG filename as the "paper").
/// A real paper is never this short, so anything under it counts as a miss.
const MIN_MARKDOWN_BYTES: usize = 5000;

/// Full-text markdown, or `None` when the Hub cannot serve this paper
/// (no HTML for most pre-2022 papers; not every id is indexed).
pub fn markdown(id: &ArxivId) -> Result<Option<String>> {
    let r = net::get(&format!("{HUB}/papers/{id}.md"))?;
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
}

#[derive(Debug, Deserialize)]
pub struct Author {
    pub name: String,
}

/// Hub record for one paper, or `None` if the Hub does not index it.
pub fn info(id: &ArxivId) -> Result<Option<Info>> {
    let r = net::get(&format!("{HUB}/api/papers/{id}"))?;
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
    let body = net::get_ok(&format!("{HUB}/api/daily_papers?date={date}&limit=100"))?;
    decode(&body, "hub daily papers")
}

fn decode<T: serde::de::DeserializeOwned>(body: &[u8], what: &'static str) -> Result<T> {
    serde_json::from_slice(body).map_err(|source| Error::Decode {
        what,
        source,
    })
}
