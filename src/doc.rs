//! C1: `docs/<id>/meta.json`. The envelope is common to every kind; the body is the
//! kind's required metadata (see `contracts/kinds/`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::contract::Kind;
use crate::error::{Error, Result};
use crate::id::{DocId, Rel, is_slug, string_newtype_impls};

#[derive(Clone, Debug, Serialize)]
pub struct Doc<B> {
    pub id: DocId,
    pub kind: Kind,
    pub taint: Taint,
    /// When it entered the vault, ISO 8601 with offset.
    pub added: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// File name or URL it came from; never an absolute path (B16).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(flatten)]
    pub body: B,
}

/// Anything from outside is printed fenced, so an injected instruction is never a bare line (B5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Taint {
    Own,
    External,
}

impl Taint {
    #[must_use]
    pub fn of(kind: Kind, id: &DocId) -> Self {
        let outside =
            [Kind::PAPER, Kind::BLOG, Kind::NEWS].contains(&kind) || matches!(id, DocId::Web(_));
        if outside { Self::External } else { Self::Own }
    }
}

/// Lowercase slug, e.g. `vla`, `world-model`, `starred`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Tag(String);

impl Tag {
    pub fn parse(s: &str) -> Result<Self> {
        if is_slug(s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(Error::BadTag(s.to_owned()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

string_newtype_impls!(Tag);

#[derive(Clone, Debug, Default, Serialize)]
pub struct Paper {
    pub title: String,
    pub authors: Vec<String>,
    /// `YYYY-MM-DD`; absent when unknown, so `missing` reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    pub links: PaperLinks,
    #[serde(rename = "abstract", skip_serializing_if = "String::is_empty")]
    pub abstract_: String,
    /// Papers with Code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tldr: Option<String>,
    /// HF Hub `ai_summary`, else alphaXiv's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Papers with Code predecessors and successors, in the library or not. `None` until
    /// PwC answers, so a reindex knows to retry; `Some(vec![])` when it knows of none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineage: Option<Vec<Edge>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<Card>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PaperLinks {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openreview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
}

/// `src` `rel` `dst`, e.g. π0.5 builds_on π0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub src: DocId,
    pub dst: DocId,
    pub rel: Rel,
}

/// blog and news.
#[derive(Clone, Debug, Serialize)]
pub struct Web {
    pub title: String,
    pub url: String,
    pub site: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Artifact {
    pub title: String,
    pub captured_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Meeting {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub started_at: String,
    pub tz: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// Page slugs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// Any other kind: a title plus whatever the file's frontmatter says.
pub type Fields = Map<String, Value>;

/// Structured extraction, written by an agent from the paper text.
/// `None` / empty means "the paper does not say", never a guess.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    pub family: Family,
    #[serde(default)]
    pub backbone: Option<String>,
    #[serde(default)]
    pub action_head: Option<ActionHead>,
    #[serde(default)]
    pub action_space: Option<ActionSpace>,
    #[serde(default)]
    pub chunk_size: Option<u32>,
    #[serde(default)]
    pub control_hz: Option<f64>,
    #[serde(default)]
    pub embodiment: Vec<String>,
    #[serde(default)]
    pub data: Data,
    #[serde(default)]
    pub eval: Eval,
    #[serde(default)]
    pub open: Open,
    #[serde(default)]
    pub compute: Option<String>,
    /// Failure modes the paper itself admits.
    #[serde(default)]
    pub limits: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Family {
    Vla,
    Wam,
    WorldModel,
    DiffusionPolicy,
    Rl,
    Control,
    Benchmark,
    Dataset,
    Library,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ActionHead {
    FlowMatching,
    Diffusion,
    FastTokens,
    ArBins,
    Latent,
    Mlp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ActionSpace {
    Joint,
    EeDelta,
    EeAbs,
    Latent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DataSource {
    #[serde(rename = "in-house")]
    InHouse,
    #[serde(rename = "OXE")]
    Oxe,
    #[serde(rename = "human-video")]
    HumanVideo,
    #[serde(rename = "mixed")]
    Mixed,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    #[serde(default)]
    pub hours: Option<f64>,
    #[serde(default)]
    pub episodes: Option<u64>,
    #[serde(default)]
    pub source: Option<DataSource>,
}

/// Benchmark name -> success rate (or the paper's headline metric).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Eval {
    #[serde(default)]
    pub sim: BTreeMap<String, f64>,
    #[serde(default)]
    pub real: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    #[serde(default)]
    pub weights: Option<bool>,
    /// Code URL, when released.
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub data: Option<bool>,
}
