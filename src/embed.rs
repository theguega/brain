//! Vectors (B12): a local OpenAI-compatible `/v1/embeddings` server, stored as f16.
//!
//! `BRAIN_EMBED_URL` (default [`DEFAULT_URL`]) and `BRAIN_EMBED_MODEL` (default
//! [`DEFAULT_MODEL`]) pick the server and model; `BRAIN_EMBED_TIMEOUT` is the seconds a
//! reindex batch may take (default [`BATCH_SECS`]). A query gets [`QUERY_SECS`], then
//! `find` goes on without vectors.

use serde::Deserialize;
use serde_json::json;

use crate::error::{Error, Result};
use crate::net;

pub const DEFAULT_URL: &str = "http://localhost:11434/v1/embeddings";
pub const DEFAULT_MODEL: &str = "qwen3-embedding:0.6b";
/// Texts per request.
pub(crate) const BATCH: usize = 32;
pub const BATCH_SECS: u64 = 60;
pub const QUERY_SECS: u64 = 5;
/// Chars sent per text: a whole table can be long, and the server truncates anyway.
const MAX_CHARS: usize = 8000;
/// Qwen3-Embedding is trained with an instruction on the query side only.
const QUERY_PREFIX: &str =
    "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Embedder {
    pub url: String,
    pub model: String,
    pub batch_secs: u64,
}

#[derive(Deserialize)]
struct Response {
    data: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    index: usize,
    embedding: Vec<f32>,
}

impl Embedder {
    #[must_use]
    pub fn from_env() -> Self {
        let var = |k: &str, d: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| d.to_owned())
        };
        Self {
            url: var("BRAIN_EMBED_URL", DEFAULT_URL),
            model: var("BRAIN_EMBED_MODEL", DEFAULT_MODEL),
            batch_secs: var("BRAIN_EMBED_TIMEOUT", "").parse().unwrap_or(BATCH_SECS),
        }
    }

    /// One L2-normalized vector per text, in order, within a batch's time.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_within(texts, self.batch_secs)
    }

    fn embed_within(&self, texts: &[String], secs: u64) -> Result<Vec<Vec<f32>>> {
        let input: Vec<String> = texts
            .iter()
            .map(|t| t.chars().take(MAX_CHARS).collect())
            .collect();
        let body = json!({"model": self.model, "input": input}).to_string();
        let raw = net::post_json_within(&self.url, &body, secs)?;
        let mut r: Response = serde_json::from_slice(&raw).map_err(|source| Error::Decode {
            what: "the embedder",
            source,
        })?;
        r.data.sort_by_key(|i| i.index);
        if r.data.len() != texts.len() {
            return Err(Error::Command {
                cmd: format!("POST {}", self.url),
                detail: format!("{} vectors for {} texts", r.data.len(), texts.len()),
            });
        }
        Ok(r.data
            .into_iter()
            .map(|i| normalized(i.embedding))
            .collect())
    }

    pub fn embed_query(&self, q: &str) -> Result<Vec<f32>> {
        let mut v = self.embed_within(&[format!("{QUERY_PREFIX}{q}")], QUERY_SECS)?;
        Ok(v.pop().unwrap_or_default())
    }
}

fn normalized(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// Little-endian f16 bytes.
#[must_use]
pub(crate) fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| f16_bits(*x).to_le_bytes()).collect()
}

/// Dot product of a query with an f16 blob of the same dimension.
#[must_use]
pub(crate) fn dot(q: &[f32], blob: &[u8]) -> f32 {
    let (halves, _) = blob.as_chunks::<2>();
    halves
        .iter()
        .zip(q)
        .map(|(b, x)| f32_of(u16::from_le_bytes(*b)) * x)
        .sum()
}

/// f32 to IEEE 754 binary16, round to nearest even; subnormals kept, overflow to infinity.
#[must_use]
pub(crate) fn f16_bits(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x007f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man == 0 { 0 } else { 0x0200 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x0080_0000;
        let shift = (14 - e) as u32;
        let rounded = m + (1 << (shift - 1)) - 1 + ((m >> shift) & 1);
        return sign | (rounded >> shift) as u16;
    }
    let rounded = man + 0x0fff + ((man >> 13) & 1);
    // A mantissa carry bumps the exponent, which is the right result, up to infinity.
    let v = ((e as u32) << 10) + (rounded >> 13);
    sign | v.min(0x7c00) as u16
}

#[must_use]
pub(crate) fn f32_of(h: u16) -> f32 {
    let sign = u32::from(h & 0x8000) << 16;
    let exp = u32::from((h >> 10) & 0x1f);
    let man = u32::from(h & 0x03ff);
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            let (mut e, mut m) = (127 - 14, man);
            while m & 0x0400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((m & 0x03ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_matches_ieee() {
        for (x, h) in [
            (0.0, 0x0000),
            (-0.0, 0x8000),
            (1.0, 0x3c00),
            (0.5, 0x3800),
            (-2.0, 0xc000),
            (65504.0, 0x7bff),
            (1e6, 0x7c00),
            (6.103_515_6e-5, 0x0400),
            (5.960_464_5e-8, 0x0001),
            (1e-7, 0x0002),
            (1e-8, 0x0000),
            (1.000_488_3, 0x3c00), // halfway between 1 and the next: to even
            (1.001_464_8, 0x3c02), // halfway, odd below: up
        ] {
            assert_eq!(f16_bits(x), h, "{x}");
        }
        assert!(f32_of(0x7e00).is_nan());
        assert_eq!(f32_of(0xfc00), f32::NEG_INFINITY);
    }

    #[test]
    fn f16_round_trips_every_finite_value() {
        for h in 0..=u16::MAX {
            let x = f32_of(h);
            if x.is_finite() {
                assert_eq!(f16_bits(x), h, "{h:#06x}");
            }
        }
    }

    #[test]
    fn dot_of_normalized_blobs_is_cosine() {
        let a = normalized(vec![3.0, 4.0]);
        let d = dot(&a, &to_blob(&a));
        assert!((d - 1.0).abs() < 1e-3, "{d}");
    }
}
