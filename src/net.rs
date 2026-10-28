//! HTTP via the system `curl`: present on every macOS/Linux box, and it saves us a
//! TLS stack's worth of dependencies for a handful of GETs.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::error::{Error, Result, io};

const UA: &str = concat!(
    "paperdb/",
    env!("CARGO_PKG_VERSION"),
    " (personal paper library)"
);
const ATTEMPTS: u32 = 4;

pub(crate) struct Response {
    pub status: u16,
    pub body: Vec<u8>,
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

/// One request. 429 and 5xx are retried with backoff; any other status is returned as is.
pub(crate) fn request(url: &str, method: Method<'_>) -> Result<Response> {
    let mut wait = Duration::from_secs(3);
    for _ in 1..ATTEMPTS {
        let r = curl(url, method)?;
        if r.status != 429 && r.status < 500 {
            return Ok(r);
        }
        sleep(wait);
        wait *= 2;
    }
    curl(url, method)
}

pub(crate) fn get(url: &str) -> Result<Response> {
    request(url, Method::Get)
}

/// GET that treats any non-2xx as an error.
pub(crate) fn get_ok(url: &str) -> Result<Vec<u8>> {
    ok(url, get(url)?)
}

pub(crate) fn post_json_ok(url: &str, body: &str) -> Result<Vec<u8>> {
    ok(url, request(url, Method::PostJson(body))?)
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

fn curl(url: &str, method: Method<'_>) -> Result<Response> {
    let fail = |detail: String| Error::Network {
        url: url.to_owned(),
        detail,
    };
    let mut cmd = Command::new("curl");
    // The status code goes to stderr after any error text, so stdout stays the raw body.
    cmd.args([
        "-sS",
        "-L",
        "--max-time",
        "120",
        "-A",
        UA,
        "-w",
        "%{stderr}%{http_code}",
    ]);
    if let Method::PostJson(_) = method {
        cmd.args([
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "--data-binary",
            "@-",
        ]);
    }
    let stdin = match method {
        Method::Get => Stdio::null(),
        Method::PostJson(_) => Stdio::piped(),
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
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
