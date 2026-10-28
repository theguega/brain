//! brain: the knowledge layer. A vault of plain files in a local git repo (see [`vault`]),
//! checked against the contracts in `contracts/` (see [`contract`]).

pub mod alphaxiv;
pub mod arxiv;
pub mod contract;
pub mod doc;
pub mod error;
pub mod hub;
pub mod id;
pub mod ingest;
pub mod label;
pub mod lint;
pub mod log;
mod net;
pub mod page;
pub mod pwc;
pub mod s2;
pub mod time;
pub mod vault;

pub use error::{Error, Result};
pub use id::{ArxivId, DocId, Rel, Slug};
pub use vault::Vault;
