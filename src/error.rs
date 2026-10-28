use std::path::{Path, PathBuf};

/// Every fallible path in the crate. Each variant is a real boundary:
/// file I/O, JSON on disk or on the wire, an external command, HTTP, user input.
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
    #[error("{}:{line}: {why}", path.display())]
    Frontmatter {
        path: PathBuf,
        line: usize,
        why: String,
    },
    #[error("{}: {why}", path.display())]
    Invalid { path: PathBuf, why: String },
    #[error("`{cmd}` failed: {detail}")]
    Command { cmd: String, detail: String },
    /// `curl` itself failed: not installed, DNS, TLS, timeout.
    #[error("`curl {url}` failed: {detail}")]
    Network { url: String, detail: String },
    #[error("HTTP {status} from {url}")]
    Http { url: String, status: u16 },
    #[error("not an id or source: {0:?}")]
    BadRef(String),
    #[error("not a tag: {0:?} (lowercase letters, digits, '-')")]
    BadTag(String),
    #[error("not a kind: {0:?}")]
    BadKind(String),
    #[error("not a rel: {0:?} (lowercase letters, digits, '_')")]
    BadRel(String),
    #[error("unknown timezone {0:?}")]
    BadTz(String),
    #[error("no vault at {}", .0.display())]
    NoVault(PathBuf),
    #[error("no page {0:?}")]
    NoPage(String),
    #[error("{0} is on neither arXiv nor the Hugging Face Hub")]
    Unknown(String),
    #[error("no text for {id}: {why}")]
    NoText { id: String, why: String },
    #[error("{0}")]
    Usage(String),
}

impl Error {
    /// 2 when the command itself is wrong, 1 for everything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::BadRef(_)
            | Self::BadTag(_)
            | Self::BadKind(_)
            | Self::BadRel(_)
            | Self::Usage(_) => 2,
            _ => 1,
        }
    }

    /// The command to try next, printed after `error:`.
    #[must_use]
    pub fn next(&self) -> String {
        match self {
            Self::BadRef(_) => {
                "pass an arXiv id or URL, a web URL, a file path, or an id like web:<slug>"
                    .to_owned()
            }
            Self::BadKind(_) => format!(
                "use one of: {}",
                crate::contract::kinds().collect::<Vec<_>>().join(", ")
            ),
            Self::BadTz(_) => {
                "fix tz in ~/.local/state/context.json (an IANA name like Europe/Paris)".to_owned()
            }
            Self::NoVault(p) => format!("run `brain --vault {}` to create it", p.display()),
            Self::NoPage(slug) => format!("create pages/{slug}.md, then run `brain lint`"),
            Self::Frontmatter { .. } | Self::Invalid { .. } | Self::Json { .. } => {
                "fix the file, then run `brain lint`".to_owned()
            }
            Self::Network { .. } | Self::Http { .. } => "retry later".to_owned(),
            Self::Unknown(_) => "check the id on arxiv.org".to_owned(),
            _ => "brain --help".to_owned(),
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
