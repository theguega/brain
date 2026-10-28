//! C5: `profile.md`, what a session starts with. Deterministic, no model, at most 400 tokens.

use std::path::Path;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::label::{FILE as LABELS, Label, Verdict};
use crate::time::Now;
use crate::vault::Vault;
use crate::{log, note, tokens};

pub const FILE: &str = "profile.md";
pub const MAX_TOKENS: usize = 400;
/// The C5 schema caps the text at this many chars too.
const MAX_CHARS: usize = 1600;
const LINE: usize = 160;
const FOCUS: usize = 3;
const ACTIVE_DAYS: i64 = 14;
/// A `later` comes back after this many days (the feed's rule).
const LATER_DAYS: i64 = 7;
const FOOTER: &str = "ask brain before guessing: `brain find`";

/// Why drover `needs` events are not in the profile.
pub const DROVER_NOTE: &str = "drover needs: none (drover v2 does not exist yet)";

struct Section {
    head: &'static str,
    lines: Vec<String>,
}

/// Builds the text from the vault as of `now`. Over the cap, lines go from the least
/// important section first: projects, then the plan, then what needs you, then the focus.
pub fn build(vault: &Vault, now: &Now) -> Result<String> {
    let today = days(now.day()).ok_or_else(|| Error::Usage(format!("bad day {:?}", now.day())))?;
    let mut sections = [
        Section {
            head: "focus",
            lines: focus(vault)?,
        },
        Section {
            head: "active projects",
            lines: projects(vault, today)?,
        },
        Section {
            head: "today's plan",
            lines: plan(vault, now.day())?,
        },
        Section {
            head: "needs you",
            lines: needs(vault, today)?,
        },
    ];
    for s in &mut sections {
        s.lines = s.lines.iter().map(|l| clip(l)).collect();
    }
    const DROP_ORDER: [usize; 4] = [1, 2, 3, 0];
    loop {
        let text = render(&sections);
        if fits(&text) {
            return Ok(text);
        }
        let next = DROP_ORDER
            .iter()
            .find(|&&i| sections.get(i).is_some_and(|s| !s.lines.is_empty()));
        match next.and_then(|&i| sections.get_mut(i)) {
            Some(s) => {
                s.lines.pop();
            }
            None => return Ok(text),
        }
    }
}

fn render(sections: &[Section]) -> String {
    let mut out = String::new();
    for s in sections.iter().filter(|s| !s.lines.is_empty()) {
        out.push_str(&format!("{}:\n", s.head));
        for l in &s.lines {
            out.push_str(&format!("- {l}\n"));
        }
    }
    out.push_str(FOOTER);
    out.push('\n');
    out
}

fn fits(text: &str) -> bool {
    tokens::count(text) <= MAX_TOKENS && text.len() <= MAX_CHARS
}

/// One line, at most [`LINE`] chars.
fn clip(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= LINE {
        return one;
    }
    let cut: String = one.chars().take(LINE - 1).collect();
    format!("{cut}…")
}

/// The first lines of `pages/me.md`'s body that are not headings.
fn focus(vault: &Vault) -> Result<Vec<String>> {
    let Some(text) = vault.read(Path::new("pages/me.md"))? else {
        return Ok(Vec::new());
    };
    let body = note::split(&text).map_or(text.as_str(), |(_, b)| b);
    Ok(body
        .lines()
        .map(|l| l.trim().trim_start_matches(['-', '*']).trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .take(FOCUS)
        .map(str::to_owned)
        .collect())
}

/// `slug: summary` of the `status: active` pages updated in the last 14 days, newest first.
fn projects(vault: &Vault, today: i64) -> Result<Vec<String>> {
    let dir = vault.path(Path::new("pages"));
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for e in crate::error::io(std::fs::read_dir(&dir), &dir)? {
        let name = crate::error::io(e, &dir)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        let Some(slug) = name.strip_suffix(".md").filter(|s| !s.starts_with('.')) else {
            continue;
        };
        let text = vault
            .read(&Path::new("pages").join(&name))?
            .unwrap_or_default();
        let Some(fm) = note::split(&text).and_then(|(y, _)| note::parse(y).ok()) else {
            continue;
        };
        let get = |k: &str| fm.get(k).and_then(Value::as_str).unwrap_or_default();
        let updated = get("updated");
        if get("status") == "active"
            && days(updated).is_some_and(|d| (0..=ACTIVE_DAYS).contains(&(today - d)))
        {
            found.push((updated.to_owned(), format!("{slug}: {}", get("summary"))));
        }
    }
    found.sort_by(|a, b| b.cmp(a));
    Ok(found.into_iter().map(|(_, l)| l).collect())
}

/// The list items of today's blocks written with `--source plan`, unchecked ones only.
fn plan(vault: &Vault, day: &str) -> Result<Vec<String>> {
    let Some(text) = vault.read(&Path::new("log").join(format!("{day}.md")))? else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    let mut in_plan = false;
    for line in text.lines() {
        if let Some(e) = log::Entry::parse_heading(line) {
            in_plan = e.source.as_str() == "plan";
        } else if in_plan
            && let Some(item) = line
                .trim()
                .strip_prefix("- ")
                .or_else(|| line.trim().strip_prefix("* "))
            && !item.starts_with("[x]")
        {
            items.push(item.trim_start_matches("[ ]").trim().to_owned());
        }
    }
    Ok(items)
}

/// Labels whose latest verdict is `later`, older than a week: oldest first.
fn needs(vault: &Vault, today: i64) -> Result<Vec<String>> {
    let Some(text) = vault.read(Path::new(LABELS))? else {
        return Ok(Vec::new());
    };
    let mut latest: Vec<Label> = Vec::new();
    for l in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Label>(l).ok())
    {
        latest.retain(|o| o.id != l.id);
        latest.push(l);
    }
    let mut late: Vec<&Label> = latest
        .iter()
        .filter(|l| {
            l.verdict == Verdict::Later
                && days(l.ts.get(..10).unwrap_or_default()).is_some_and(|d| today - d >= LATER_DAYS)
        })
        .collect();
    late.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(late
        .iter()
        .map(|l| format!("later, overdue: {}", l.id))
        .collect())
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` (proleptic Gregorian, Hinnant's algorithm).
fn days(day: &str) -> Option<i64> {
    let mut p = day.split('-');
    let y: i64 = p.next()?.parse().ok()?;
    let m: i64 = p.next()?.parse().ok()?;
    let d: i64 = p.next()?.parse().ok()?;
    if p.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Builds the profile and writes it to the vault atomically; returns the text.
pub fn write(vault: &Vault, now: &Now) -> Result<String> {
    let text = build(vault, now)?;
    vault.write(Path::new(FILE), &text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_count_across_leap_years() {
        assert_eq!(days("1970-01-01"), Some(0));
        assert_eq!(days("2026-10-28"), Some(20_754));
        assert_eq!(days("2024-03-01"), days("2024-02-29").map(|d| d + 1));
        assert_eq!(days("2026-13-01"), None);
    }

    #[test]
    fn clip_makes_one_short_line() {
        assert_eq!(clip("a\n  b"), "a b");
        assert_eq!(clip(&"x".repeat(500)).chars().count(), LINE);
    }
}
