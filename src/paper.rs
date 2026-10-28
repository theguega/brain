//! The on-disk record: `papers/<id>/paper.json`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::date::Day;
use crate::error::{Error, Result};
use crate::id::{ArxivId, PaperId, Slug, is_slug, string_newtype_impls};

/// Where a paper lives. The id is derived from this, so it cannot disagree with it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Arxiv(ArxivId),
    Web {
        slug: Slug,
        url: String,
        pdf: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Paper {
    pub source: Source,
    pub title: String,
    /// Short handle, e.g. "pi0". Empty when none was given.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub authors: Vec<String>,
    /// `YYYY-MM-DD`, or empty when unknown.
    #[serde(default)]
    pub published: String,
    #[serde(default, rename = "abstract")]
    pub abstract_: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Project page, code repo, blog post.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// Hugging Face Hub `ai_summary`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Hugging Face Hub `ai_keywords`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// `YYYY-MM-DD` it entered the library.
    pub added: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<Card>,
}

impl Paper {
    /// A bare record: everything but source, title and date left empty.
    #[must_use]
    pub fn new(source: Source, title: String, added: Day) -> Self {
        Self {
            source,
            title,
            name: String::new(),
            authors: Vec::new(),
            published: String::new(),
            abstract_: String::new(),
            tags: Vec::new(),
            note: String::new(),
            links: Vec::new(),
            summary: String::new(),
            keywords: Vec::new(),
            added: added.iso(),
            card: None,
        }
    }

    #[must_use]
    pub fn id(&self) -> PaperId {
        match &self.source {
            Source::Arxiv(id) => PaperId::Arxiv(id.clone()),
            Source::Web { slug, .. } => PaperId::Slug(slug.clone()),
        }
    }

    #[must_use]
    pub fn url(&self) -> String {
        match &self.source {
            Source::Arxiv(id) => format!("https://arxiv.org/abs/{id}"),
            Source::Web { url, .. } => url.clone(),
        }
    }

    #[must_use]
    pub fn pdf_url(&self) -> Option<String> {
        match &self.source {
            Source::Arxiv(id) => Some(format!("https://arxiv.org/pdf/{id}")),
            Source::Web { pdf, .. } => pdf.clone(),
        }
    }

    /// The name if set, else the title.
    #[must_use]
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.title
        } else {
            &self.name
        }
    }

    /// Adds a tag unless already present; keeps tags sorted.
    pub fn tag(&mut self, t: Tag) {
        if let Err(at) = self.tags.binary_search(&t) {
            self.tags.insert(at, t);
        }
    }

    pub fn untag(&mut self, t: &Tag) {
        self.tags.retain(|x| x != t);
    }
}

/// Lowercase slug, e.g. `vla`, `world-model`, `starred`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Tag(String);

impl Tag {
    /// Strict: the input must already be a slug.
    pub fn parse(s: &str) -> Result<Self> {
        if is_slug(s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(Error::BadTag(s.to_owned()))
        }
    }

    /// Lenient: lowercases and turns runs of anything else into '-'. `None` if nothing is left.
    #[must_use]
    pub fn slugify(s: &str) -> Option<Self> {
        let mut out = String::with_capacity(s.len());
        for c in s.chars().flat_map(char::to_lowercase) {
            if c.is_ascii_alphanumeric() {
                out.push(c);
            } else if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
        }
        let out = out.trim_end_matches('-');
        Self::parse(out.get(..64).unwrap_or(out)).ok()
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

string_newtype_impls!(Tag);

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

/// Shown by `brain card schema`: what an agent fills in.
pub const CARD_TEMPLATE: &str = r#"{
  "family": "vla | wam | world-model | diffusion-policy | rl | control | benchmark | dataset | library | other",
  "backbone": "e.g. PaliGemma-3B, or null",
  "action_head": "flow-matching | diffusion | fast-tokens | ar-bins | latent | mlp | null",
  "action_space": "joint | ee-delta | ee-abs | latent | null",
  "chunk_size": "integer or null",
  "control_hz": "number or null",
  "embodiment": ["robot names"],
  "data": { "hours": null, "episodes": null, "source": "in-house | OXE | human-video | mixed | null" },
  "eval": { "sim": { "LIBERO-10": 0.93 }, "real": { "towel folding": 0.8 } },
  "open": { "weights": null, "code": "https://github.com/... or null", "data": null },
  "compute": "e.g. 64xH100 for 3 days, or null",
  "limits": ["failure modes the paper itself admits"]
}
null / [] / {} = the paper does not say. Never infer."#;

/// Serde name of a unit enum variant, e.g. `Family::WorldModel` -> "world-model".
#[must_use]
pub fn wire_name<T: Serialize>(v: &T) -> Option<String> {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => Some(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags() {
        assert_eq!(
            Tag::slugify("WAM Definition").unwrap().as_str(),
            "wam-definition"
        );
        assert_eq!(Tag::slugify("  Robot / RL! ").unwrap().as_str(), "robot-rl");
        assert!(Tag::slugify("!!").is_none());
        assert!(Tag::parse("Has Space").is_err());
    }

    #[test]
    fn card_rejects_unknown_values() {
        assert!(serde_json::from_str::<Card>(r#"{"family":"vla"}"#).is_ok());
        assert!(serde_json::from_str::<Card>(r#"{"family":"llm"}"#).is_err());
        assert!(serde_json::from_str::<Card>(r#"{"family":"vla","typo":1}"#).is_err());
        assert_eq!(wire_name(&Family::WorldModel).unwrap(), "world-model");
    }
}
