//! Hugging Face Hub: paper markdown (arXiv's HTML rendering, no PDF needed),
//! AI summary/keywords, and the daily papers feed for `discover`.
//!
//! The Hub meters anonymously and by token: 100 markdown fetches per 5 minutes without one,
//! 200 with. `HF_TOKEN` in the environment or the CLI's own saved token lifts both buckets,
//! and is only ever sent to [`HUB`].

use std::path::PathBuf;

use serde::Deserialize;

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
}

#[derive(Debug, Deserialize)]
pub struct Author {
    pub name: String,
}

/// Hub record for one paper, or `None` if the Hub does not index it.
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
