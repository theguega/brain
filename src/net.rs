//! HTTP via the system `curl`: present on every macOS/Linux box, and it saves us a
//! TLS stack's worth of dependencies for a handful of GETs.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::error::{Error, Result, io};

const UA: &str = concat!(
    "brain/",
    env!("CARGO_PKG_VERSION"),
    " (personal paper library)"
);
const ATTEMPTS: u32 = 4;
/// Seconds for a whole request, unless the caller sets its own.
const MAX_TIME: u64 = 120;
/// Seconds to connect: a dead host fails fast whatever the request's budget.
const CONNECT_TIME: &str = "10";
/// Ceiling on a `Retry-After` wait: one add must not block for the rest of a 5-minute window.
const MAX_WAIT: u64 = 60;

pub(crate) struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// Seconds the server asked us to wait, from the response that answered.
    pub retry_after: Option<u64>,
}

impl Response {
    fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Method<'a> {
    Get,
    PostJson(&'a str),
}

/// The header pair for an authorised request: `Some(("Authorization", "Bearer …"))`.
pub(crate) type Auth<'a> = Option<(&'a str, &'a str)>;

/// One request. 429 and 5xx are retried with backoff, waiting as long as the server asks
/// (`Retry-After`) up to [`MAX_WAIT`]; any other status is returned as is.
pub(crate) fn request(url: &str, method: Method<'_>, auth: Auth<'_>) -> Result<Response> {
    let mut wait = Duration::from_secs(3);
    for _ in 1..ATTEMPTS {
        let r = curl(url, method, auth, MAX_TIME)?;
        if r.status != 429 && r.status < 500 {
            return Ok(r);
        }
        // A 429 without the header still gets the default backoff.
        wait = r
            .retry_after
            .map_or(wait, |s| Duration::from_secs(s.min(MAX_WAIT)));
        sleep(wait);
        wait *= 2;
    }
    curl(url, method, auth, MAX_TIME)
}

pub(crate) fn get(url: &str) -> Result<Response> {
    request(url, Method::Get, None)
}

/// A GET carrying a bearer header: a higher rate-limit bucket, never a blanket credential.
pub(crate) fn get_as(url: &str, auth: Auth<'_>) -> Result<Response> {
    request(url, Method::Get, auth)
}

/// GET that treats any non-2xx as an error.
pub(crate) fn get_ok(url: &str) -> Result<Vec<u8>> {
    ok(url, get(url)?)
}

pub(crate) fn post_json_ok(url: &str, body: &str) -> Result<Vec<u8>> {
    ok(url, request(url, Method::PostJson(body), None)?)
}

/// One POST, no retry, given up after `secs`: for a caller that has a fallback.
pub(crate) fn post_json_within(url: &str, body: &str, secs: u64) -> Result<Vec<u8>> {
    ok(url, curl(url, Method::PostJson(body), None, secs)?)
}

pub(crate) fn download(url: &str, to: &Path) -> Result<()> {
    let body = get_ok(url)?;
    if let Some(dir) = to.parent() {
        io(std::fs::create_dir_all(dir), dir)?;
    }
    io(std::fs::write(to, body), to)
}

fn ok(url: &str, r: Response) -> Result<Vec<u8>> {
    if r.is_ok() {
        Ok(r.body)
    } else {
        Err(Error::Http {
            url: url.to_owned(),
            status: r.status,
        })
    }
}

fn curl(url: &str, method: Method<'_>, auth: Auth<'_>, max_time: u64) -> Result<Response> {
    let fail = |detail: String| Error::Network {
        url: url.to_owned(),
        detail,
    };
    // `%header{...}` needs curl 7.83; this box has 7.81, so headers go to a file.
    let headers = std::env::temp_dir().join(format!("brain-hdr-{}.txt", std::process::id()));
    let mut cmd = Command::new("curl");
    // The status code goes to stderr after any error text, so stdout stays the raw body.
    cmd.args([
        "-sS",
        "-L",
        "--connect-timeout",
        CONNECT_TIME,
        "--max-time",
        &max_time.to_string(),
        "-D",
        &headers.to_string_lossy(),
        "-A",
        UA,
        "-w",
        "%{stderr}%{http_code}",
    ]);
    if let Some((name, value)) = auth {
        cmd.args(["-H", &format!("{name}: {value}")]);
    }
    let stdin = match method {
        Method::Get => Stdio::null(),
        Method::PostJson(_) => {
            cmd.args([
                "-X",
                "POST",
                "-H",
                "content-type: application/json",
                "--data-binary",
                "@-",
            ]);
            Stdio::piped()
        }
    };
    let mut child = cmd
        .arg(url)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(e.to_string()))?;
    if let (Method::PostJson(body), Some(mut pipe)) = (method, child.stdin.take()) {
        pipe.write_all(body.as_bytes())
            .map_err(|e| fail(e.to_string()))?;
    }
    let out = child.wait_with_output().map_err(|e| fail(e.to_string()))?;
    let retry_after = std::fs::read_to_string(&headers)
        .map(|h| retry_after(&h))
        .unwrap_or_default();
    let _ = std::fs::remove_file(&headers);
    let err = String::from_utf8_lossy(&out.stderr);
    let err = err.trim_end();
    let (detail, code) = err.split_at(err.len().saturating_sub(3));
    if !out.status.success() {
        return Err(fail(detail.trim().to_owned()));
    }
    let status = code
        .parse()
        .map_err(|_| fail(format!("unreadable status {code:?}")))?;
    Ok(Response {
        status,
        body: out.stdout,
        retry_after,
    })
}

/// Seconds from the last `Retry-After` in a dumped header block. A redirect chain writes one
/// block per hop; the final response is the one that answered. An HTTP-date form is ignored:
/// every host brain talks to sends delta-seconds.
fn retry_after(headers: &str) -> Option<u64> {
    headers.lines().rev().find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.eq_ignore_ascii_case("retry-after")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

/// Percent-encode a query-string component.
#[must_use]
pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            // Writing to a String cannot fail.
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::retry_after;

    #[test]
    fn retry_after_takes_the_final_response() {
        // A redirect chain: the 307's header is not the one that answered.
        let dumped = "HTTP/2 307\r\nRetry-After: 300\r\n\r\n\
                      HTTP/2 429\r\nretry-after: 47\r\n\r\n";
        assert_eq!(retry_after(dumped), Some(47));
    }

    #[test]
    fn retry_after_ignores_an_http_date() {
        let dumped = "HTTP/2 429\r\nRetry-After: Wed, 21 Oct 2015 07:28:00 GMT\r\n\r\n";
        assert_eq!(retry_after(dumped), None);
    }

    #[test]
    fn retry_after_absent_is_none() {
        assert_eq!(retry_after("HTTP/2 200\r\netag: \"x\"\r\n\r\n"), None);
    }
}
