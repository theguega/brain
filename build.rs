//! Embeds every `contracts/kinds/<kind>.schema.json`, so adding a kind is a file and a rebuild.

use std::fmt::Write as _;
use std::path::Path;
use std::{env, fs};

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
