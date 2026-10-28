use std::path::{Path, PathBuf};

/// Every fallible path in the crate. Each variant is a real boundary:
/// file I/O, JSON on disk or on the wire, SQLite, an external command, HTTP, user input.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("bad JSON from {what}: {source}")]
    Decode {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("index: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("`{cmd}` failed: {detail}")]
    Command { cmd: String, detail: String },
    /// `curl` itself failed: not installed, DNS, TLS, timeout.
    #[error("`curl {url}` failed: {detail}")]
    Network { url: String, detail: String },
    #[error("HTTP {status} from {url}")]
    Http { url: String, status: u16 },
    #[error("not a paper reference: {0:?} (want an arXiv id or URL, or a lowercase slug)")]
    BadRef(String),
    #[error("not a tag: {0:?} (lowercase letters, digits, '-')")]
    BadTag(String),
    #[error("no paper {0} in the library")]
    NotFound(String),
    #[error("{0} is on neither arXiv nor the Hugging Face Hub")]
    Unknown(String),
    #[error("no text for {id}: {why}")]
    NoText { id: String, why: String },
    #[error("no library at {} (run `paperdb init`, or set PAPERDB_LIBRARY)", .0.display())]
    NoLibrary(PathBuf),
    #[error("{failed} of {total} not added (errors above)")]
    Partial { failed: usize, total: usize },
    #[error("{0}")]
    Usage(String),
}

impl Error {
    /// Process exit code, so a caller can tell "fix the command" from "retry later":
    /// 2 bad usage, 3 not in the library, 4 network, 1 anything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) | Self::BadRef(_) | Self::BadTag(_) | Self::NoLibrary(_) => 2,
            Self::NotFound(_) => 3,
            Self::Http { .. } | Self::Network { .. } => 4,
            _ => 1,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Attach the path to an I/O error, at the call site where it is known.
pub(crate) fn io<T>(r: std::io::Result<T>, path: &Path) -> Result<T> {
    r.map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })
}
