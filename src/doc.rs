//! C1: `docs/<dir>/<dir>.md`, one note per doc. Its frontmatter is the envelope, common to
//! every kind, plus the kind's required metadata (see `contracts/kinds/`); its body is the
//! full text. Originals stay beside it: a book's `source.<ext>`, an artifact's `index.html`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::contract::Kind;
use crate::error::{Error, Result};
use crate::id::{DocId, is_slug, string_newtype_impls};
use crate::note;
use crate::vault::Vault;

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
    /// File name or URL it came from; never an absolute path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(flatten)]
    pub body: B,
}

/// Anything from outside is printed fenced, so an injected instruction is never a bare line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Taint {
    Own,
    External,
}

impl Taint {
    #[must_use]
    pub fn of(kind: Kind, id: &DocId) -> Self {
        // Only notes and meetings are own: the rest, an artifact made with a model too, is data.
        let own = [Kind::NOTE, Kind::MEETING].contains(&kind) && !matches!(id, DocId::Web(_));
        if own { Self::Own } else { Self::External }
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
    /// Papers with Code predecessors and successors as `[[<dir>]]` wikilinks, in the library
    /// or not. `None` until PwC answers, so a reindex knows to retry; `Some(vec![])` when it
    /// knows of none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builds_on: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<Card>,
    /// What the Hub attached to it: released models, datasets, demo Spaces. Facts about the
    /// ecosystem around the paper, so not `links` (the paper's own claims) and not `card`
    /// (extracted from its text). Absent when the Hub has nothing to say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub: Option<Hub>,
}

/// A paper's Hugging Face footprint. The Hub lists only the top few artifacts of each kind,
/// so `counts` carries the full totals: 4 shown of 114 models is a different paper from 4 of 4.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hub {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub datasets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spaces: Vec<String>,
    #[serde(default, skip_serializing_if = "HubCounts::is_zero")]
    pub counts: HubCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upvotes: Option<u64>,
    /// Stars on the linked GitHub repo, when the Hub knows one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stars: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubCounts {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub models: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub datasets: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub spaces: usize,
}

impl HubCounts {
    fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
    /// `[[<page slug>]]` wikilinks.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<String>,
    /// A `[[<page slug>]]` wikilink.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// Any other kind: a title plus whatever the file's frontmatter says. Also a note's
/// frontmatter as read.
pub type Fields = Map<String, Value>;

/// `docs/<dir>/<dir>.md`, relative to the vault.
#[must_use]
pub fn path(id: &DocId) -> PathBuf {
    let dir = id.dir();
    Path::new("docs").join(&dir).join(format!("{dir}.md"))
}

/// The doc's frontmatter and body; `None` when it has no note.
pub fn read(vault: &Vault, id: &DocId) -> Result<Option<(Fields, String)>> {
    let rel = path(id);
    vault
        .read(&rel)?
        .map(|text| note::read(&text, &rel))
        .transpose()
}

/// The doc's frontmatter as JSON, envelope and kind metadata together.
pub fn fields<B: Serialize>(doc: &Doc<B>) -> Result<Fields> {
    match serde_json::to_value(doc) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(Error::Invalid {
            path: path(&doc.id),
            why: "a doc is not a JSON object".to_owned(),
        }),
        Err(source) => Err(Error::Decode {
            what: "doc",
            source,
        }),
    }
}

/// The note text. Without `aliases`, they are the title and a single keyword: a lone keyword
/// is a short name (π0), while longer keyword lists are topics, not names.
pub fn text(id: &DocId, mut fm: Fields, body: &str) -> Result<String> {
    if !fm.contains_key("aliases") {
        let title = fm.get("title").and_then(Value::as_str);
        let handle = match fm
            .get("keywords")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
        {
            Some([Value::String(k)]) => Some(k.as_str()),
            _ => None,
        };
        let mut aliases: Vec<Value> = Vec::new();
        for a in title.into_iter().chain(handle) {
            if !a.is_empty() && !aliases.iter().any(|v| v.as_str() == Some(a)) {
                aliases.push(Value::String(a.to_owned()));
            }
        }
        if !aliases.is_empty() {
            fm.insert("aliases".to_owned(), Value::Array(aliases));
        }
    }
    note::render(&fm, body, &path(id))
}

/// Writes the note: the doc exists once it does.
pub fn write(vault: &Vault, id: &DocId, fm: Fields, body: &str) -> Result<()> {
    vault.write(&path(id), text(id, fm, body)?)
}

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
