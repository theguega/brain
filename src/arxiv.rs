//! arXiv export API: metadata by id, and date-bounded searches for `discover`.
//! The Atom feed is regular enough that a few string scans beat an XML dependency.

use std::thread::sleep;
use std::time::Duration;

use crate::error::Result;
use crate::id::ArxivId;
use crate::net;

const API: &str = "https://export.arxiv.org/api/query";
/// arXiv asks for one request every 3 seconds.
pub(crate) const PAUSE: Duration = Duration::from_secs(3);
const BATCH: usize = 100;

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: ArxivId,
    pub title: String,
    pub abstract_: String,
    pub authors: Vec<String>,
    /// `YYYY-MM-DD`
    pub published: String,
    /// The authors' free-text comment, often with the project page.
    pub comment: String,
}

impl Entry {
    /// URLs in the comment, trailing punctuation dropped.
    pub fn comment_urls(&self) -> impl Iterator<Item = &str> {
        self.comment
            .split_whitespace()
            .filter(|w| w.starts_with("http://") || w.starts_with("https://"))
            .map(|w| w.trim_end_matches(['.', ',', ';', ')']))
    }
}

/// Metadata for `ids`, 100 per request. Ids arXiv does not know are simply absent.
pub fn fetch(ids: &[ArxivId]) -> Result<Vec<Entry>> {
    let mut out = Vec::with_capacity(ids.len());
    for (i, batch) in ids.chunks(BATCH).enumerate() {
        if i > 0 {
            sleep(PAUSE);
        }
        let list: Vec<&str> = batch.iter().map(ArxivId::as_str).collect();
        let url = format!(
            "{API}?id_list={}&max_results={}",
            list.join(","),
            batch.len()
        );
        out.extend(parse_feed(&String::from_utf8_lossy(&net::get_ok(&url)?)));
    }
    Ok(out)
}

/// Newest-first search, e.g. `cat:cs.RO AND abs:"world model"`.
pub fn search(query: &str, max: u32) -> Result<Vec<Entry>> {
    let url = format!(
        "{API}?search_query={}&sortBy=submittedDate&sortOrder=descending&max_results={max}",
        net::encode(query)
    );
    Ok(parse_feed(&String::from_utf8_lossy(&net::get_ok(&url)?)))
}

/// Atom feed -> entries. Entries without a new-style id (errors, old ids) are skipped.
#[must_use]
pub fn parse_feed(xml: &str) -> Vec<Entry> {
    xml.split("<entry>")
        .skip(1)
        .filter_map(|block| {
            let block = block.split("</entry>").next()?;
            let id = ArxivId::parse(inner(block, "id")?.rsplit('/').next()?).ok()?;
            let published = inner(block, "published").unwrap_or_default();
            Some(Entry {
                id,
                title: clean(inner(block, "title")?),
                abstract_: clean(inner(block, "summary").unwrap_or_default()),
                authors: all_inner(block, "name").into_iter().map(clean).collect(),
                published: published.get(..10).unwrap_or(published).to_owned(),
                comment: clean(inner(block, "arxiv:comment").unwrap_or_default()),
            })
        })
        .collect()
}

/// Text of the first `<tag ...>...</tag>` in `s`.
fn inner<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    all_inner(s, tag).into_iter().next()
}

fn all_inner<'a>(mut s: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut out = Vec::new();
    while let Some(start) = s.find(&open) {
        let Some(rest) = s.get(start + open.len()..) else {
            break;
        };
        s = rest;
        // `<id>` must not match `<idx>`: the next byte ends the tag name.
        if !rest.starts_with(['>', ' ', '\n', '\t']) {
            continue;
        }
        let Some(body) = rest.find('>').and_then(|gt| rest.get(gt + 1..)) else {
            break;
        };
        let Some(end) = body.find(&close) else { break };
        out.extend(body.get(..end));
        s = body.get(end..).unwrap_or_default();
    }
    out
}

/// Unescape the five XML entities and collapse whitespace.
fn clean(s: &str) -> String {
    let s = s
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<feed><title>arXiv Query</title><id>http://arxiv.org/api/x</id>
<entry>
  <id>http://arxiv.org/abs/2410.24164v3</id>
  <published>2024-10-31T17:22:30Z</published>
  <title>$\pi_0$: A Vision-Language-Action
    Flow Model</title>
  <summary>Robots &amp; things.</summary>
  <author><name>Kevin Black</name></author>
  <author><name>Noah Brown</name></author>
  <arxiv:comment>See project website for videos: https://physicalintelligence.company/blog/pi0. Published in RSS 2025</arxiv:comment>
  <arxiv:primary_category term="cs.LG"/>
</entry>
<entry><id>http://arxiv.org/api/errors#bad</id><title>Error</title></entry>
</feed>"#;

    #[test]
    fn parses_atom() {
        let e = parse_feed(FEED);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].id.as_str(), "2410.24164");
        assert_eq!(e[0].title, r"$\pi_0$: A Vision-Language-Action Flow Model");
        assert_eq!(e[0].abstract_, "Robots & things.");
        assert_eq!(e[0].authors, ["Kevin Black", "Noah Brown"]);
        assert_eq!(e[0].published, "2024-10-31");
        assert_eq!(
            e[0].comment_urls().collect::<Vec<_>>(),
            ["https://physicalintelligence.company/blog/pi0"]
        );
    }
}
