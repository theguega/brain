//! `brain lint`, schema part (B9): every doc and page against its contract.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::contract::{self, Kind, Schema};
use crate::doc::Taint;
use crate::error::{Result, io};
use crate::id::DocId;
use crate::note;
use crate::vault::Vault;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// Relative to the vault.
    pub file: PathBuf,
    pub error: String,
}

/// Every error, docs first, each group in name order.
pub fn lint(vault: &Vault) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    let mut schemas: BTreeMap<&str, Schema> = BTreeMap::new();
    for name in entries(vault, "docs")? {
        let file = Path::new("docs").join(&name).join(format!("{name}.md"));
        let mut finding = |error: String| {
            out.push(Finding {
                file: file.clone(),
                error,
            })
        };
        let has_note = vault.path(&file).is_file();
        for old in ["meta.json", "text.md"] {
            if vault
                .path(&Path::new("docs").join(&name).join(old))
                .exists()
            {
                finding(if has_note {
                    format!(
                        "{old} beside the note: brain migrate-paperdb finishes a fold it \
                         matches; if it does not, an older brain wrote it: merge what you \
                         want into {name}.md and delete {old}"
                    )
                } else {
                    format!(
                        "{old} is the old layout: run brain migrate-paperdb to fold it into the note"
                    )
                });
            }
        }
        let Some(raw) = vault.read(&file)? else {
            finding(format!("no {name}.md"));
            continue;
        };
        let meta = match note::split(&raw).map(|(yaml, _)| note::parse(yaml)) {
            Some(Ok(fm)) => Value::Object(fm),
            Some(Err((line, why))) => {
                finding(format!("line {}: {why}", line + 1));
                continue;
            }
            None => {
                finding("no frontmatter".to_owned());
                continue;
            }
        };
        let dir_id = DocId::from_dir(&name).ok();
        if dir_id.as_ref().map(DocId::to_string).as_deref()
            != meta.get("id").and_then(Value::as_str)
        {
            finding("id does not match the directory name".to_owned());
        }
        let kind = match meta.get("kind").and_then(Value::as_str).map(Kind::parse) {
            Some(Ok(k)) => k,
            Some(Err(e)) => {
                finding(e.to_string());
                continue;
            }
            None => {
                finding("missing required kind".to_owned());
                continue;
            }
        };
        let schema = match schemas.entry(kind.as_str()) {
            std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::btree_map::Entry::Vacant(v) => {
                v.insert(Schema::compile(kind.as_str(), kind.schema())?)
            }
        };
        for p in schema.problems(&meta) {
            finding(p.to_string());
        }
        // Set by brain from the kind and id; an edit would unfence external text (B5).
        if let Some(id) = &dir_id {
            let want = Taint::of(kind, id);
            if serde_json::to_value(want).ok().as_ref() != meta.get("taint") {
                finding(format!(
                    "taint must be {} for a {kind}",
                    serde_json::to_value(want)
                        .unwrap_or_default()
                        .as_str()
                        .unwrap_or_default()
                ));
            }
        }
    }

    let pages = Schema::compile("page", contract::PAGE)?;
    for name in entries(vault, "pages")? {
        if !name.ends_with(".md") {
            continue;
        }
        let file = Path::new("pages").join(&name);
        let mut finding = |error: String| {
            out.push(Finding {
                file: file.clone(),
                error,
            })
        };
        let text = vault.read(&file)?.unwrap_or_default();
        let Some((yaml, _)) = note::split(&text) else {
            finding("no frontmatter".to_owned());
            continue;
        };
        match note::parse(yaml) {
            Ok(fm) => {
                for p in pages.problems(&Value::Object(fm)) {
                    finding(p.to_string());
                }
            }
            Err((line, why)) => finding(format!("line {}: {why}", line + 1)),
        }
    }
    Ok(out)
}

/// File names in a top-level vault directory, sorted; none if it does not exist yet.
pub(crate) fn entries(vault: &Vault, dir: &str) -> Result<Vec<String>> {
    let path = vault.path(Path::new(dir));
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for e in io(fs::read_dir(&path), &path)? {
        let name = io(e, &path)?.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}
