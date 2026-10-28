//! `brain lint`, schema part (B9): every doc and page against its contract.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::contract::{self, Kind, Schema};
use crate::error::{Result, io};
use crate::id::DocId;
use crate::page;
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
        let file = Path::new("docs").join(&name).join("meta.json");
        let mut finding = |error: String| {
            out.push(Finding {
                file: file.clone(),
                error,
            })
        };
        let Some(raw) = vault.read(&file)? else {
            finding("no meta.json".to_owned());
            continue;
        };
        let meta: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                finding(format!("not JSON: {e}"));
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
        let Some((yaml, _)) = page::split(&text) else {
            finding("no frontmatter".to_owned());
            continue;
        };
        match page::parse_yaml(yaml) {
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
