//! S6: GPU jobs skip their run while training holds the GPU.

use std::collections::BTreeMap;
use std::process::Command;

/// Memory held by other processes above which the GPU is busy, in MiB (2 GB).
const BUSY_MIB: u64 = 2048;

/// Whether processes other than the embedder server hold more than 2 GB of GPU memory.
/// No `nvidia-smi`, no GPU to be busy.
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
    let server = listener(embed_url);
    held(&parse(&out), |pid| {
        server.as_ref().is_some_and(|s| s.owns(pid))
    }) > BUSY_MIB
}

/// The process listening on the embedder's port, as far as `ss` shows it without root.
#[derive(Debug, Default, PartialEq, Eq)]
struct Server {
    pid: Option<u32>,
    uid: Option<u32>,
}

impl Server {
    /// A GPU process is the server's when it descends from the listener (ollama runs the
    /// model in a child). Another user's listener hides its pid, so then the owner decides:
    /// the server's service user runs nothing else.
    fn owns(&self, pid: u32) -> bool {
        match (self.pid, self.uid) {
            (Some(root), _) => descends(pid, root),
            (None, Some(uid)) => uid_of(pid) == Some(uid),
            (None, None) => false,
        }
    }
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

fn listener(url: &str) -> Option<Server> {
    let host_port = url.split_once("://")?.1.split('/').next()?;
    let (host, port) = host_port.rsplit_once(':')?;
    if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return None;
    }
    run("ss", &["-Hltnep", &format!("sport = :{port}")]).map(|o| parse_listener(&o))
}

/// `pid=<n>` is there for the user's own sockets, `uid:<n>` always.
fn parse_listener(out: &str) -> Server {
    let after = |key: &str| {
        let rest = out.split(key).nth(1)?;
        rest.split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    };
    Server {
        pid: after("pid="),
        uid: after("uid:"),
    }
}

fn uid_of(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
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
    fn the_listener_is_read_with_or_without_its_pid() {
        let other = "LISTEN 0 4096 *:11434 *:* uid:997 ino:6705 sk:2024 cgroup:/system.slice/ollama.service v6only:0 <->";
        assert_eq!(
            parse_listener(other),
            Server {
                pid: None,
                uid: Some(997)
            }
        );
        let own = "LISTEN 0 4096 127.0.0.1:11434 0.0.0.0:* users:((\"ollama\",pid=1732,fd=3)) uid:1000 ino:9";
        assert_eq!(
            parse_listener(own),
            Server {
                pid: Some(1732),
                uid: Some(1000)
            }
        );
        assert_eq!(parse_listener(""), Server::default());
    }

    #[test]
    fn a_hidden_listener_owns_the_processes_of_its_user() {
        let me = std::process::id();
        let mine = uid_of(me).unwrap();
        let hidden = |uid| Server {
            pid: None,
            uid: Some(uid),
        };
        assert!(hidden(mine).owns(me));
        assert!(!hidden(mine + 1).owns(me));
        assert!(!Server::default().owns(me));
    }

    #[test]
    fn a_process_descends_from_itself_and_its_parent() {
        let me = std::process::id();
        assert!(descends(me, me));
        assert!(descends(me, std::os::unix::process::parent_id()));
        assert!(!descends(me, u32::MAX));
    }
}
