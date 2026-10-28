//! Embeds every `contracts/kinds/<kind>.schema.json`, so adding a kind is a file and a rebuild,
//! and stamps the build with the brain git commit (`BRAIN_GIT`) that `brain eval` records.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::{env, fs};

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=contracts/kinds");
    let mut kinds = Vec::new();
    for entry in fs::read_dir("contracts/kinds")? {
        let name = entry?.file_name();
        if let Some(kind) = name.to_str().and_then(|n| n.strip_suffix(".schema.json")) {
            kinds.push(kind.to_owned());
        }
    }
    kinds.sort();
    // Rebuild when the commit moves or the source changes (`--dirty` tracks the latter).
    println!("cargo::rerun-if-changed=src");
    for r in ["HEAD", "refs"] {
        if let Some(p) = git(&["rev-parse", "--git-path", r]) {
            println!("cargo::rerun-if-changed={p}");
        }
    }
    let commit =
        git(&["describe", "--always", "--dirty", "--abbrev=12"]).unwrap_or("unknown".into());
    println!("cargo::rustc-env=BRAIN_GIT={commit}");
    let mut out = String::from("pub(crate) const KINDS: &[(&str, &str)] = &[\n");
    for k in &kinds {
        writeln!(
            out,
            "    ({k:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/contracts/kinds/{k}.schema.json\"))),"
        )?;
    }
    out.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR")?).join("kinds.rs"), out)?;
    Ok(())
}
