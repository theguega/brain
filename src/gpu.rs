//! S6: GPU jobs skip their run while training holds the GPU.

use std::collections::BTreeMap;
use std::process::Command;

/// Memory held by other processes above which the GPU is busy, in MiB (2 GB).
const BUSY_MIB: u64 = 2048;

/// Whether processes other than the embedder server hold more than 2 GB of GPU memory.
/// No `nvidia-smi`, no GPU to be busy. The server is found by the port of the embedder URL,
/// and its children count as it (ollama runs the model in a child).
#[must_use]
pub fn busy(embed_url: &str) -> bool {
    let Some(out) = run(
        "nvidia-smi",
        &[
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ],
    ) else {
        return false;
    };
    let apps = parse(&out);
    let server = listener(embed_url);
    held(&apps, |pid| server.is_some_and(|s| descends(pid, s))) > BUSY_MIB
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `pid, MiB` lines.
fn parse(out: &str) -> Vec<(u32, u64)> {
    out.lines()
        .filter_map(|l| {
            let (pid, mib) = l.split_once(',')?;
            Some((pid.trim().parse().ok()?, mib.trim().parse().ok()?))
        })
        .collect()
}

/// MiB held by the processes that `ours` does not claim.
fn held(apps: &[(u32, u64)], ours: impl Fn(u32) -> bool) -> u64 {
    apps.iter().filter(|(p, _)| !ours(*p)).map(|(_, m)| m).sum()
}

/// The pid listening on a local embedder URL's port.
fn listener(url: &str) -> Option<u32> {
    let host_port = url.split_once("://")?.1.split('/').next()?;
    let (host, port) = host_port.rsplit_once(':')?;
    if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return None;
    }
    let out = run("ss", &["-Hltnp", &format!("sport = :{port}")])?;
    let rest = out.split("pid=").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Whether `pid` is `root` or one of its descendants.
fn descends(pid: u32, root: u32) -> bool {
    let parents: BTreeMap<u32, u32> = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(e.path().join("stat")).ok()?;
            // `pid (comm) S ppid ...`: comm may hold spaces, so cut at its closing paren.
            let ppid = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()?;
            Some((pid, ppid))
        })
        .collect();
    let mut at = pid;
    for _ in 0..64 {
        if at == root {
            return true;
        }
        match parents.get(&at) {
            Some(&p) if p != at && p != 0 => at = p,
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_other_processes_count_against_the_threshold() {
        let apps = parse("101, 1500\n202, 2100\nnoise\n303, 5\n");
        assert_eq!(apps, [(101, 1500), (202, 2100), (303, 5)]);
        assert_eq!(held(&apps, |p| p == 202), 1505);
        assert_eq!(held(&apps, |_| false), 3605);
    }

    #[test]
    fn a_process_descends_from_itself_and_its_parent() {
        let me = std::process::id();
        assert!(descends(me, me));
        assert!(descends(me, std::os::unix::process::parent_id()));
        assert!(!descends(me, u32::MAX));
    }
}
