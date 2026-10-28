//! The contracts (`contracts/*.schema.json`), embedded at build time, and the
//! hand-written serde types for the ones no other module owns.

use std::fmt;

use jsonschema::error::ValidationErrorKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

include!(concat!(env!("OUT_DIR"), "/kinds.rs"));

pub const PAGE: &str = include_str!("../contracts/page.schema.json");
pub const LOG: &str = include_str!("../contracts/log.schema.json");
pub const PROFILE: &str = include_str!("../contracts/profile.schema.json");
pub const LABEL: &str = include_str!("../contracts/label.schema.json");
pub const JOB: &str = include_str!("../contracts/job.schema.json");
pub const REPO: &str = include_str!("../contracts/repo.schema.json");
pub const CONTEXT: &str = include_str!("../contracts/context.schema.json");

/// Every kind with a schema in `contracts/kinds/`, sorted.
pub fn kinds() -> impl Iterator<Item = &'static str> {
    KINDS.iter().map(|(k, _)| *k)
}

/// A C1 kind: one that has a schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Kind(&'static str);

impl Kind {
    pub const PAPER: Self = Self("paper");
    pub const BLOG: Self = Self("blog");
    pub const NEWS: Self = Self("news");
    pub const NOTE: Self = Self("note");
    pub const ARTIFACT: Self = Self("artifact");
    pub const MEETING: Self = Self("meeting");

    pub fn parse(s: &str) -> Result<Self> {
        kinds()
            .find(|k| *k == s)
            .map(Self)
            .ok_or_else(|| Error::BadKind(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.0
    }

    #[must_use]
    pub fn schema(self) -> &'static str {
        KINDS
            .iter()
            .find(|(k, _)| *k == self.0)
            .map_or("", |(_, s)| *s)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A compiled schema.
pub struct Schema(jsonschema::Validator);

impl fmt::Debug for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Schema")
    }
}

impl Schema {
    pub fn compile(name: &str, src: &str) -> Result<Self> {
        let bad = |why: String| Error::Usage(format!("schema {name}: {why}"));
        let v: Value = serde_json::from_str(src).map_err(|e| bad(e.to_string()))?;
        jsonschema::validator_for(&v)
            .map(Self)
            .map_err(|e| bad(e.to_string()))
    }

    #[must_use]
    pub fn problems(&self, instance: &Value) -> Vec<Problem> {
        self.0
            .iter_errors(instance)
            .map(|e| {
                let at = e
                    .instance_path()
                    .as_str()
                    .trim_start_matches('/')
                    .replace('/', ".");
                match e.kind() {
                    ValidationErrorKind::Required { property } => {
                        let name = property.as_str().unwrap_or_default();
                        Problem::Missing(if at.is_empty() {
                            name.to_owned()
                        } else {
                            format!("{at}.{name}")
                        })
                    }
                    _ => Problem::Invalid {
                        at,
                        why: e.to_string(),
                    },
                }
            })
            .collect()
    }
}

/// One way an instance fails its schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A required field, as a dotted path.
    Missing(String),
    Invalid {
        at: String,
        why: String,
    },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(field) => write!(f, "missing required {field}"),
            Self::Invalid { at, why } if at.is_empty() => f.write_str(why),
            Self::Invalid { at, why } => write!(f, "{at}: {why}"),
        }
    }
}

/// C5, as `brain --json` prints it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub profile: String,
}

/// C10: one line of `jobs.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobLine {
    pub job: String,
    pub started: String,
    pub ended: String,
    pub ok: bool,
    pub err: Option<String>,
}

/// C13: a registered repo. `path` is relative to the home directory (B16).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    pub path: String,
    pub remote: Option<String>,
    pub project: Option<String>,
}
