//! C7: feed verdicts, one JSON line each in `labels.jsonl`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::id::DocId;
use crate::vault::Vault;

pub const FILE: &str = "labels.jsonl";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Like,
    Skip,
    Later,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub id: DocId,
    pub verdict: Verdict,
    pub ts: String,
    /// Where the verdict was given: `cli`, `dashboard`.
    pub surface: String,
}

pub fn append(vault: &Vault, label: &Label) -> Result<()> {
    let mut line = serde_json::to_string(label).map_err(|source| Error::Json {
        path: FILE.into(),
        source,
    })?;
    line.push('\n');
    vault.append(Path::new(FILE), &line)
}
