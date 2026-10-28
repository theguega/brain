//! brain: a personal paper library.
//!
//! The library is a folder of plain files synced with git (see [`library`]); the
//! search index is a disposable cache rebuilt from it (see [`index`]).

pub mod arxiv;
pub mod date;
pub mod discover;
pub mod error;
pub mod hub;
pub mod id;
pub mod index;
pub mod ingest;
pub mod library;
mod net;
pub mod paper;
pub mod s2;

pub use error::{Error, Result};
pub use id::{ArxivId, PaperId, Slug};
pub use library::Library;
pub use paper::{Card, Paper, Source, Tag};
