//! Time follows you (B15): a timestamp is ISO 8601 with its offset, stored with the
//! IANA zone and place read from C15 at capture time.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::vault::read_json;

const ZONEINFO: &str = "/usr/share/zoneinfo";

/// C15: `~/.local/state/context.json`. The last writer wins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub tz: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    pub updated_at: String,
    pub source: String,
}

/// The current time where you are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Now {
    /// `2026-10-27T22:40:12+01:00`
    pub at: String,
    pub tz: String,
    pub place: Option<String>,
}

impl Now {
    /// Local `YYYY-MM-DD`: day boundaries follow the current zone.
    #[must_use]
    pub fn day(&self) -> &str {
        self.at.get(..10).unwrap_or(&self.at)
    }
}

/// `$XDG_STATE_HOME/context.json`, else `~/.local/state/context.json`.
#[must_use]
pub fn context_path() -> Option<PathBuf> {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(d) if !d.is_empty() => Some(PathBuf::from(d).join("context.json")),
        _ => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/context.json")),
    }
}

/// Zone and place from C15; without it, the system zone and no place.
pub fn now() -> Result<Now> {
    let (tz, place) = match context_path() {
        Some(p) if p.is_file() => {
            let c: Context = read_json(&p)?;
            if let Some(place) = &c.place
                && !is_place(place)
            {
                return Err(Error::Invalid {
                    path: p,
                    why: format!("place {place:?} must be one line, without ' · ', and not '-'"),
                });
            }
            (c.tz, c.place)
        }
        _ => (system_tz(), None),
    };
    Ok(Now {
        at: local_time(&tz)?,
        tz,
        place,
    })
}

fn system_tz() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if is_zone(tz) {
            return tz.to_owned();
        }
    }
    let linked = std::fs::read_link("/etc/localtime").ok().and_then(|p| {
        let p = p.to_string_lossy().into_owned();
        p.split_once("zoneinfo/").map(|(_, tz)| tz.to_owned())
    });
    let named = || {
        std::fs::read_to_string("/etc/timezone")
            .ok()
            .map(|s| s.trim().to_owned())
    };
    linked
        .or_else(named)
        .filter(|tz| is_zone(tz))
        .unwrap_or_else(|| "UTC".to_owned())
}

/// A place fits one field of a C3 heading: `-` there means unknown.
fn is_place(p: &str) -> bool {
    !p.is_empty()
        && p != "-"
        && !p.contains('·')
        && !p.chars().any(char::is_control)
        && p.trim() == p
}

/// An IANA zone name with a TZif file: not `zone.tab`, `posixrules`, nor the
/// `posix/` and `right/` trees (the latter counts leap seconds).
fn is_zone(tz: &str) -> bool {
    let named = tz.split('/').all(|seg| {
        seg.starts_with(|c: char| c.is_ascii_alphabetic())
            && seg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
    });
    let special = matches!(
        tz.split('/').next(),
        Some("posix" | "right" | "posixrules" | "localtime" | "Factory")
    );
    let mut magic = [0u8; 4];
    named
        && !special
        && std::fs::File::open(Path::new(ZONEINFO).join(tz))
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
            .is_ok()
        && &magic == b"TZif"
}

/// `date` knows every zone the system does; an unknown `TZ` would silently mean UTC, hence the check.
fn local_time(tz: &str) -> Result<String> {
    if !is_zone(tz) {
        return Err(Error::BadTz(tz.to_owned()));
    }
    let fail = |detail: String| Error::Command {
        cmd: "date".to_owned(),
        detail,
    };
    let out = Command::new("date")
        .env("TZ", tz)
        .arg("+%Y-%m-%dT%H:%M:%S%z")
        .output()
        .map_err(|e| fail(e.to_string()))?;
    let s = String::from_utf8_lossy(&out.stdout);
    // `%z` is `+0100` everywhere; `%:z` is GNU-only.
    match s.trim().split_at_checked(s.trim().len().saturating_sub(2)) {
        Some((head, mm)) if out.status.success() && head.len() == 22 => Ok(format!("{head}:{mm}")),
        _ => Err(fail(format!("unexpected output {s:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_with_offset() {
        let at = local_time("Asia/Tokyo").unwrap();
        assert_eq!(at.len(), 25, "{at}");
        assert!(at.ends_with("+09:00"), "{at}");
        assert!(local_time("Mars/Olympus").is_err());
        for bad in [
            "zone.tab",
            "tzdata.zi",
            "posixrules",
            "right/UTC",
            "posix/Europe/Paris",
            "../etc/passwd",
            "Factory",
        ] {
            assert!(!is_zone(bad), "{bad}");
        }
        for good in [
            "UTC",
            "Europe/Paris",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+5",
        ] {
            assert!(is_zone(good), "{good}");
        }
        assert!(is_place("Paris") && !is_place("-") && !is_place("A · B") && !is_place("A\n## x"));
    }
}
