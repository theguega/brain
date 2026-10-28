//! C3: the log, `log/YYYY-MM-DD.md`, one block per entry:
//! `## <ISO time with offset> · <IANA tz> · <place> · <source> · <title>`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::id::Slug;
use crate::time::Now;
use crate::vault::Vault;

const SEP: &str = " · ";
/// Written for an unknown place, so the heading keeps five fields.
const NO_PLACE: &str = "-";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub at: String,
    pub tz: String,
    pub place: Option<String>,
    pub source: Slug,
    pub title: String,
    pub body: String,
}

impl Entry {
    /// The first line of `text` is the title, the rest the body.
    pub fn new(now: Now, source: Slug, text: &str) -> Result<Self> {
        let text = text.trim();
        let (title, body) = text.split_once('\n').unwrap_or((text, ""));
        if title.is_empty() {
            return Err(Error::Usage("nothing to log".to_owned()));
        }
        Ok(Self {
            at: now.at,
            tz: now.tz,
            place: now.place,
            source,
            title: title.trim().to_owned(),
            body: body.trim().to_owned(),
        })
    }

    #[must_use]
    pub fn heading(&self) -> String {
        let place = self.place.as_deref().unwrap_or(NO_PLACE);
        format!(
            "## {}{SEP}{}{SEP}{place}{SEP}{}{SEP}{}",
            self.at, self.tz, self.source, self.title
        )
    }

    /// Inverse of [`Entry::heading`], with an empty body.
    #[must_use]
    pub fn parse_heading(line: &str) -> Option<Self> {
        let mut f = line.strip_prefix("## ")?.splitn(5, SEP);
        let (at, tz, place, source, title) =
            (f.next()?, f.next()?, f.next()?, f.next()?, f.next()?);
        Some(Self {
            at: at.to_owned(),
            tz: tz.to_owned(),
            place: (place != NO_PLACE).then(|| place.to_owned()),
            source: Slug::parse(source).ok()?,
            title: title.to_owned(),
            body: String::new(),
        })
    }

    /// `log/<local day>.md`.
    #[must_use]
    pub fn file(&self) -> PathBuf {
        Path::new("log").join(format!("{}.md", self.at.get(..10).unwrap_or(&self.at)))
    }
}

/// Appends the block to its day's file and returns that file, relative to the vault.
pub fn append(vault: &Vault, entry: &Entry) -> Result<PathBuf> {
    let file = entry.file();
    let fresh = vault.read(&file)?.is_none_or(|s| s.trim().is_empty());
    let mut block = if fresh {
        String::new()
    } else {
        "\n".to_owned()
    };
    block.push_str(&entry.heading());
    block.push('\n');
    if !entry.body.is_empty() {
        block.push('\n');
        block.push_str(&entry.body);
        block.push('\n');
    }
    vault.append(&file, &block)?;
    Ok(file)
}
