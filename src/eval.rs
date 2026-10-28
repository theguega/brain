//! B8: `eval/questions.jsonl` holds `{q, expect: [id…]}`. Each question goes through `find`,
//! and the run is scored on recall@8 and on the output tokens per hit.
//!
//! `find` is called directly, so an eval run never lands in `queries.jsonl` as a real query.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::embed::Embedder;
use crate::error::{Error, Result};
use crate::index::{self, Index};
use crate::search::{self, Filter};
use crate::vault::{Vault, git};
use crate::{state, time, tokens};

pub const QUESTIONS: &str = "eval/questions.jsonl";
pub const FILE: &str = "eval.jsonl";
const K: usize = 8;
/// The brain build: eval results are compared across it.
pub const BRAIN: &str = env!("BRAIN_GIT");

#[derive(Deserialize)]
struct Question {
    q: String,
    expect: Vec<String>,
}

/// What one run records in `eval.jsonl` (`version` 1).
#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    pub at: String,
    pub brain: String,
    pub vault: String,
    pub questions_hash: String,
    pub questions: usize,
    pub recall: f64,
    pub tokens: f64,
    pub degraded: Option<String>,
}

impl Score {
    fn json(&self) -> Value {
        json!({
            "version": 1, "at": self.at, "brain": self.brain, "vault": self.vault,
            "questions_hash": self.questions_hash, "questions": self.questions,
            "recall8": self.recall, "tokens_per_hit": self.tokens, "degraded": self.degraded,
        })
    }

    fn of(v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k)?.as_str().map(str::to_owned);
        Some(Self {
            at: s("at")?,
            brain: s("brain")?,
            vault: s("vault")?,
            questions_hash: s("questions_hash")?,
            questions: usize::try_from(v.get("questions")?.as_u64()?).ok()?,
            recall: v.get("recall8")?.as_f64()?,
            tokens: v.get("tokens_per_hit")?.as_f64()?,
            degraded: s("degraded"),
        })
    }
}

/// The last recorded run.
pub fn last() -> Result<Option<Score>> {
    Ok(state::last(FILE)?.and_then(|v| Score::of(&v)))
}

/// What a run depends on: the build and the questions. `None` when there is nothing to
/// run: no questions, or no index with docs in it (`run` needs one).
pub fn inputs(vault: &Vault) -> Result<Option<String>> {
    let Some(text) = vault.read(Path::new(QUESTIONS))? else {
        return Ok(None);
    };
    if text.trim().is_empty() || !index::built(&index::db_path()?) {
        return Ok(None);
    }
    Ok(Some(format!("{BRAIN}:{}", fnv(&text))))
}

/// Why a run is due: the build or the questions changed since the last run, or the last
/// run was degraded and the vectors are back.
pub fn due(vault: &Vault, embedder: &Embedder) -> Result<Option<&'static str>> {
    let Some(key) = inputs(vault)? else {
        return Ok(None);
    };
    let Some(last) = last()? else {
        return Ok(Some("never run"));
    };
    if format!("{}:{}", last.brain, last.questions_hash) != key {
        return Ok(Some("retrieval code or eval/questions.jsonl changed"));
    }
    let whole = index::unembedded(&index::db_path()?, &embedder.model)? == 0
        && embedder.embed_query("ping").is_ok();
    Ok((last.degraded.is_some() && whole)
        .then_some("the last run was degraded and vectors are back"))
}

/// FNV-1a, as the index hashes docs: stable across Rust versions.
fn fnv(s: &str) -> String {
    let h = s.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{h:016x}")
}

fn head(vault: &Vault) -> Result<String> {
    Ok(git(vault.root(), &["rev-parse", "HEAD"])?.trim().to_owned())
}

/// Scores the questions and appends the result to `eval.jsonl`.
pub fn run(vault: &Vault, embedder: &Embedder) -> Result<Score> {
    let text = vault.read(Path::new(QUESTIONS))?.ok_or_else(|| {
        Error::Usage(format!(
            "no {QUESTIONS} in the vault: one {{q, expect}} per line"
        ))
    })?;
    let questions = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str::<Question>(l)
                .ok()
                .filter(|q| !q.expect.is_empty())
                .ok_or_else(|| Error::Invalid {
                    path: QUESTIONS.into(),
                    why: format!(
                        "line {}: expected {{\"q\": text, \"expect\": [id, ...]}}",
                        i + 1
                    ),
                })
        })
        .collect::<Result<Vec<_>>>()?;
    if questions.is_empty() {
        return Err(Error::Usage(format!("{QUESTIONS} has no question")));
    }
    let ix = Index::open_existing(&index::db_path()?)?;
    let (mut recall, mut toks, mut hits) = (0.0, 0, 0_usize);
    let mut degraded = None;
    for q in &questions {
        let found = search::find(&ix, &q.q, &Filter::default(), K, embedder)?;
        degraded = degraded.or(found.degraded);
        let got = q
            .expect
            .iter()
            .filter(|e| found.hits.iter().any(|h| &h.id == *e));
        recall += got.count() as f64 / q.expect.len() as f64;
        hits += found.hits.len();
        for h in &found.hits {
            toks += tokens::count(&format!(
                "{},{},{},{},{}",
                h.id, h.kind, h.date, h.title, h.snippet
            ));
        }
    }
    let score = Score {
        at: time::now()?.at,
        brain: BRAIN.to_owned(),
        vault: head(vault)?,
        questions_hash: fnv(&text),
        questions: questions.len(),
        recall: recall / questions.len() as f64,
        tokens: if hits == 0 {
            0.0
        } else {
            toks as f64 / hits as f64
        },
        degraded: degraded.map(str::to_owned),
    };
    state::append(FILE, &score.json())?;
    Ok(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_score_round_trips_through_its_line() {
        let s = Score {
            at: "2026-10-28T10:00:00+01:00".into(),
            brain: "abc".into(),
            vault: "def".into(),
            questions_hash: "00".into(),
            questions: 3,
            recall: 0.5,
            tokens: 41.5,
            degraded: Some("no vectors".into()),
        };
        assert_eq!(Score::of(&s.json()), Some(s));
    }
}
