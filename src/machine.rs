// `brain machine add`: run on the host, it copies brain's client to a machine over ssh. The client
// is `brain-ssh` and the skill, both from this build, so the machine needs no clone.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use clap::Subcommand;
use serde_json::{Value, json};

use brain::error::{Error, Result};

const BRAIN_SSH: &str = include_str!("../bin/brain-ssh");
const SKILL: &str = include_str!("../skill/SKILL.md");
const SSH_OPTS: [&str; 4] = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"];

#[derive(Subcommand)]
pub enum Op {
    /// Copy brain's client to a machine over ssh: the `brain` command and the agent skill
    ///
    /// Run it on this host. The machine needs ssh to this host without a prompt, and sets
    /// `BRAIN_HOST` to this host's name in its shell. Rerun to update.
    Add {
        /// The machine's ssh name
        ssh: String,
        /// This host's ssh name, as the machine knows it [default: this machine's hostname]
        #[arg(long)]
        host: Option<String>,
    },
}

pub fn run(op: &Op) -> Result<Vec<(&'static str, Value)>> {
    match op {
        Op::Add { ssh, host } => {
            let ssh = name(ssh)?;
            let host = match host {
                Some(h) => name(h)?.to_owned(),
                None => hostname()?,
            };
            ship(ssh, "~/.local/bin", "brain", BRAIN_SSH, "755")?;
            ship(ssh, "~/.agents/skills/brain", "SKILL.md", SKILL, "644")?;
            // Claude Code reads its skills from ~/.claude/skills: link them when nothing is there
            remote(
                ssh,
                "mkdir -p ~/.claude/skills; [ -e ~/.claude/skills/brain ] || ln -s ~/.agents/skills/brain ~/.claude/skills/brain",
            )?;
            // the machine's brain-ssh reaches this host: a failure here is a failure of the install
            remote(
                ssh,
                &format!("BRAIN_HOST={host} ~/.local/bin/brain --version"),
            )?;
            Ok(vec![
                ("machine", json!(ssh)),
                ("host", json!(host)),
                (
                    "note",
                    json!(format!("on {ssh}, set BRAIN_HOST={host} in its shell")),
                ),
            ])
        }
    }
}

// The names go to ssh and into a remote script, so only plain ssh names pass.
fn name(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-@".contains(c));
    if ok {
        Ok(s)
    } else {
        Err(Error::Usage(format!("not an ssh name: {s:?}")))
    }
}

fn hostname() -> Result<String> {
    let out = Command::new("hostname")
        .arg("-s")
        .output()
        .map_err(|e| Error::Usage(format!("hostname: {e}")))?;
    let h = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    name(&h).map(str::to_owned)
}

// Written under a new name and moved over, so a symlink from an older install is replaced, not
// written through.
fn ship(ssh: &str, dir: &str, file: &str, content: &str, mode: &str) -> Result<()> {
    let path = format!("{dir}/{file}");
    let script = format!(
        "set -e; mkdir -p {dir}; cat >{path}.new; chmod {mode} {path}.new; mv -f {path}.new {path}"
    );
    let what = format!("copy {path} to {ssh}");
    let mut child = Command::new("ssh")
        .args(SSH_OPTS)
        .args([ssh, &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| failed(&what, e))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(content.as_bytes())
            .map_err(|e| failed(&what, e))?;
    }
    let out = child.wait_with_output().map_err(|e| failed(&what, e))?;
    if out.status.success() {
        return Ok(());
    }
    Err(failed(&what, String::from_utf8_lossy(&out.stderr).trim()))
}

fn remote(ssh: &str, script: &str) -> Result<()> {
    let out = Command::new("ssh")
        .args(SSH_OPTS)
        .args([ssh, script])
        .output()
        .map_err(|e| failed(script, e))?;
    if out.status.success() {
        return Ok(());
    }
    Err(failed(script, String::from_utf8_lossy(&out.stderr).trim()))
}

fn failed(what: &str, detail: impl std::fmt::Display) -> Error {
    Error::Command {
        cmd: what.to_owned(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::name;

    #[test]
    fn only_plain_ssh_names_pass() {
        assert!(name("gpu").is_ok());
        assert!(name("me@mac.local").is_ok());
        assert!(name("-oProxyCommand=x").is_err());
        assert!(name("a b").is_err());
        assert!(name("").is_err());
    }
}
