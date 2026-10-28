//! Markdown into retrieval chunks, HTML into markdown-ish text, chunks into snippets.
//!
//! A chunk is a run of whole blocks (paragraphs, tables, code fences) inside one heading's
//! section, about [`TARGET`] tokens. Chunk texts do not overlap, so joining them with a
//! blank line gives the doc back; the overlap lives in [`Chunk::context`], the previous
//! chunk's last block, which is indexed and embedded with the chunk but never printed.

/// Tokens per chunk, before the overlap.
pub(crate) const TARGET: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chunk {
    /// The section's heading text, without `#`; empty before the first heading.
    pub heading: String,
    pub text: String,
    /// The previous chunk's last block when it is in the same section, else empty.
    pub context: String,
}

/// The rough token count used for every budget: 4 chars a token.
#[must_use]
pub(crate) fn tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    Para,
    Table,
    Fence,
}

/// `# Heading` (1 to 6 `#` then a space), as its text.
fn heading(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = line.get(hashes..)?;
    rest.starts_with(' ').then(|| rest.trim())
}

fn is_table_line(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// Sections as (heading, blocks); a section's heading line is its first block.
fn sections(md: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = vec![(String::new(), Vec::new())];
    let mut cur: Vec<&str> = Vec::new();
    let mut kind = Block::Para;
    let flush = |out: &mut Vec<(String, Vec<String>)>, cur: &mut Vec<&str>| {
        if !cur.is_empty()
            && let Some((_, blocks)) = out.last_mut()
        {
            blocks.push(cur.join("\n"));
        }
        cur.clear();
    };
    for line in md.lines() {
        let line = line.trim_end();
        if kind == Block::Fence {
            cur.push(line);
            if is_fence(line) {
                flush(&mut out, &mut cur);
                kind = Block::Para;
            }
            continue;
        }
        if is_fence(line) {
            flush(&mut out, &mut cur);
            cur.push(line);
            kind = Block::Fence;
            continue;
        }
        if let Some(h) = heading(line) {
            flush(&mut out, &mut cur);
            out.push((h.to_owned(), vec![line.to_owned()]));
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut out, &mut cur);
            continue;
        }
        let this = if is_table_line(line) {
            Block::Table
        } else {
            Block::Para
        };
        if this != kind {
            flush(&mut out, &mut cur);
            kind = this;
        }
        cur.push(line);
    }
    flush(&mut out, &mut cur);
    out.retain(|(_, blocks)| !blocks.is_empty());
    out
}

/// Splits a prose block over the target into pieces at word boundaries.
fn pieces(block: String) -> Vec<String> {
    if tokens(&block) <= TARGET || is_table_line(&block) || is_fence(&block) {
        return vec![block];
    }
    let max = TARGET * 4;
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in block.split_whitespace() {
        if !cur.is_empty() && cur.len() + word.len() + 1 > max {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Split on headings, pack whole blocks to about [`TARGET`] tokens; tables and code
/// fences are never split.
#[must_use]
pub(crate) fn chunks(md: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    for (head, blocks) in sections(md) {
        let mut cur: Vec<String> = Vec::new();
        let mut context = String::new();
        for block in blocks.into_iter().flat_map(pieces) {
            let size: usize = cur.iter().map(|b| tokens(b)).sum();
            // A heading line is never a chunk on its own.
            let lone_heading = matches!(cur.as_slice(), [h] if heading(h).is_some());
            if !cur.is_empty() && !lone_heading && size + tokens(&block) > TARGET {
                let last = cur.last().cloned().unwrap_or_default();
                out.push(Chunk {
                    heading: head.clone(),
                    text: cur.join("\n\n"),
                    context: std::mem::take(&mut context),
                });
                // One block of overlap, unless it would take half the next chunk.
                if tokens(&last) <= TARGET / 2 {
                    context = last;
                }
                cur.clear();
            }
            cur.push(block);
        }
        if !cur.is_empty() {
            out.push(Chunk {
                heading: head,
                text: cur.join("\n\n"),
                context,
            });
        }
    }
    out
}

/// The readable text of an HTML page as markdown-ish paragraphs: `<h1>`–`<h6>` become
/// headings, block elements break paragraphs, scripts, styles and `<head>` are dropped.
#[must_use]
pub(crate) fn html_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(rest.get(..lt).unwrap_or_default());
        rest = rest.get(lt..).unwrap_or_default();
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after
                .find("-->")
                .map_or("", |e| after.get(e + 3..).unwrap_or_default());
            continue;
        }
        let Some(gt) = rest.find('>') else {
            rest = "";
            break;
        };
        let tag = rest.get(1..gt).unwrap_or_default();
        rest = rest.get(gt + 1..).unwrap_or_default();
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        match name.as_str() {
            "script" | "style" | "head" | "noscript" | "svg" if !closing => {
                let end = format!("</{name}");
                rest = find_ascii_ci(rest, &end)
                    .and_then(|i| rest.get(i..).and_then(|r| r.find('>')).map(|g| i + g + 1))
                    .map_or("", |i| rest.get(i..).unwrap_or_default());
            }
            h if h.len() == 2
                && h.starts_with('h')
                && h.as_bytes()
                    .get(1)
                    .is_some_and(|d| (b'1'..=b'6').contains(d)) =>
            {
                out.push_str("\n\n");
                if !closing {
                    let n = usize::from(h.as_bytes().get(1).copied().unwrap_or(b'1') - b'0');
                    out.push_str(&"#".repeat(n));
                    out.push(' ');
                }
            }
            "p" | "div" | "br" | "li" | "tr" | "section" | "article" | "header" | "footer"
            | "ul" | "ol" | "table" | "pre" | "blockquote" | "main" | "nav" | "figure" | "hr" => {
                out.push_str("\n\n");
            }
            "td" | "th" => out.push(' '),
            _ => {}
        }
    }
    out.push_str(rest);
    let text = decode_entities(&out);
    text.split("\n\n")
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn find_ascii_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or_default());
        rest = rest.get(amp..).unwrap_or_default();
        let end = rest.bytes().take(12).position(|b| b == b';');
        let decoded = end.and_then(|e| {
            let name = rest.get(1..e)?;
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" | "#39" => '\'',
                "nbsp" => ' ',
                _ => {
                    let num = name.strip_prefix('#')?;
                    let n = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => num.parse().ok()?,
                    };
                    char::from_u32(n)?
                }
            };
            Some((c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = rest.get(len..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = rest.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

/// Chunk text as one clean line: no tables, code fences, headings markers, emphasis,
/// link syntax, citations or tags.
#[must_use]
pub(crate) fn plain(text: &str) -> String {
    let mut lines = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if is_fence(line) {
            fenced = !fenced;
            continue;
        }
        if fenced || is_table_line(line) {
            continue;
        }
        let line = line.trim_start();
        let line = heading(line).unwrap_or(line);
        lines.push(line.trim_start_matches('>').trim_start());
    }
    let joined = lines.join(" ");
    let mut out = String::with_capacity(joined.len());
    let mut rest = joined.as_str();
    while let Some(c) = rest.chars().next() {
        let skip = match c {
            '[' if rest.starts_with("[@") => rest.find(']').map(|e| (e + 1, "")),
            '[' if rest.starts_with("[[") => rest
                .find("]]")
                .map(|e| (e + 2, rest.get(2..e).unwrap_or_default())),
            '!' | '[' => link(rest),
            '<' => rest
                .find('>')
                .filter(|e| {
                    rest.get(1..*e)
                        .is_some_and(|t| !t.contains(' ') || t.contains('='))
                })
                .map(|e| (e + 1, " ")),
            '*' | '_' | '`' | '~' | '\\' => Some((1, "")),
            _ => None,
        };
        match skip {
            Some((n, keep)) => {
                out.push_str(keep);
                rest = rest.get(n..).unwrap_or_default();
            }
            None => {
                out.push(c);
                rest = rest.get(c.len_utf8()..).unwrap_or_default();
            }
        }
    }
    // A dropped citation leaves a space before its punctuation.
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" .", ".")
        .replace(" ,", ",")
}

/// `[text](url)` or `![alt](url)` at the start of `s`: its length and its text.
fn link(s: &str) -> Option<(usize, &str)> {
    let open = usize::from(s.starts_with('!'));
    let inner = s.get(open + 1..)?;
    let close = inner.find("](")?;
    let after = inner.get(close + 2..)?;
    let end = after.find(')')?;
    Some((open + 1 + close + 2 + end + 1, inner.get(..close)?))
}

/// At most `max` chars of `plain` (the output of [`plain`]), starting a little before
/// the first query term.
#[must_use]
pub(crate) fn snippet(plain: &str, terms: &[String], max: usize) -> String {
    let chars: Vec<char> = plain.chars().collect();
    let lower: String = chars
        .iter()
        .flat_map(|c| c.to_lowercase().take(1))
        .collect();
    let lower: Vec<char> = lower.chars().collect();
    let hit = terms
        .iter()
        .filter(|t| t.chars().count() >= 3)
        .filter_map(|t| {
            let t: Vec<char> = t.chars().collect();
            // Porter matched a stem: try the term, then its first few letters.
            let stem = t.get(..t.len().saturating_sub(3).max(4).min(t.len()))?;
            [t.as_slice(), stem]
                .iter()
                .find_map(|n| lower.windows(n.len()).position(|w| w == *n))
        })
        .min()
        .unwrap_or(0);
    let mut start = hit.saturating_sub(40);
    while start > 0 && chars.get(start - 1).is_some_and(|c| !c.is_whitespace()) {
        start -= 1;
    }
    let lead = if start > 0 { "…" } else { "" };
    let room = max.saturating_sub(usize::from(start > 0));
    let rest = chars.get(start..).unwrap_or_default();
    if rest.len() <= room {
        return format!("{lead}{}", rest.iter().collect::<String>());
    }
    let mut end = room.saturating_sub(1);
    while end > room / 2 && rest.get(end).is_some_and(|c| !c.is_whitespace()) {
        end -= 1;
    }
    let body: String = rest.get(..end).unwrap_or_default().iter().collect();
    format!("{lead}{}…", body.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_join_back_into_the_doc() {
        // 188 tokens each: two fit in a chunk, and one is small enough to overlap.
        let para = "word ".repeat(150).trim_end().to_owned();
        let md = format!(
            "intro\n\n# A\n\n{para}\n\n{para}\n\n{para}\n\n| a | b |\n|---|---|\n\n```\nx\n\ny\n```\n\n## B\n\nlast"
        );
        let cs = chunks(&md);
        let headings: Vec<_> = cs.iter().map(|c| c.heading.as_str()).collect();
        assert_eq!(headings, ["", "A", "A", "B"]);
        let texts: Vec<_> = cs.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts.join("\n\n"), md);
        assert!(cs.get(1).unwrap().context.is_empty());
        assert_eq!(cs.get(2).unwrap().context, para);
        assert!(
            texts
                .get(2)
                .unwrap()
                .ends_with("| a | b |\n|---|---|\n\n```\nx\n\ny\n```")
        );
    }

    #[test]
    fn a_table_stays_whole_however_long() {
        let table: String = (0..200).map(|i| format!("| row {i} | value |\n")).collect();
        let cs = chunks(&format!("# T\n\n{table}"));
        assert_eq!(cs.len(), 1);
        assert!(cs.first().unwrap().text.ends_with("| row 199 | value |"));
    }

    #[test]
    fn plain_drops_markup_and_tables() {
        let s = plain(
            "## Head\n\nSee **bold** and [the link](http://x.y) or [[page]] [@arxiv:1]\n\n| a | b |\n|---|---|\n\n<b>tag</b> `code`",
        );
        assert_eq!(s, "Head See bold and the link or page tag code");
        assert_eq!(plain("Claim [@arxiv:1]. Next"), "Claim. Next");
    }

    #[test]
    fn snippets_are_capped_on_a_word() {
        let text = format!("{} needle {}", "a ".repeat(100), "b ".repeat(100));
        let s = snippet(&text, &["needle".to_owned()], 60);
        assert!(s.chars().count() <= 60, "{s}");
        assert!(s.starts_with('…') && s.ends_with('…') && s.contains("needle"));
    }

    #[test]
    fn html_becomes_paragraphs() {
        let t = html_text(
            "<html><head><title>x</title><style>p{}</style></head><body><h2>Hi &amp; bye</h2><p>one\n two</p><script>evil()</script><!-- c --><p>&#233;</p></body>",
        );
        assert_eq!(t, "## Hi & bye\n\none two\n\né");
    }
}
