//! brain: the knowledge layer. A vault of plain files in a local git repo (see [`vault`]),
//! checked against the contracts in `contracts/` (see [`contract`]), and an index rebuilt
//! from those files (see [`index`], [`search`]).

pub mod alphaxiv;
pub mod arxiv;
mod chunk;
pub mod contract;
pub mod doc;
pub mod embed;
pub mod error;
pub mod hub;
pub mod id;
pub mod index;
pub mod ingest;
pub mod label;
pub mod lint;
pub mod log;
mod net;
pub mod page;
pub mod pwc;
pub mod repos;
pub mod s2;
pub mod search;
pub mod time;
pub mod vault;

pub use error::{Error, Result};
pub use id::{ArxivId, DocId, Rel, Slug};
pub use vault::Vault;
