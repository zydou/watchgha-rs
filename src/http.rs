//! Fetch JSON data from URLs, with auth and retry. Mirrors `http_help`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::error::WatchGhaError;

/// Status codes the inner fetch retries on (mirrors `RETRY_STATUS_CODES`).
const RETRY_STATUS_CODES: [u16; 1] = [502];

/// Install the `ring` TLS crypto provider once, before any client is built.
/// We use reqwest's `rustls-no-provider` (to avoid the default `aws-lc-rs`,
/// which needs cmake and fails to cross-build for musl), so we must supply a
/// provider ourselves. `install_default` is a no-op if one is already set.
fn install_crypto_provider() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Build a blocking HTTP client. Redirects are followed (up to reqwest's
/// default policy) and the timeout is 30s per request, matching the original.
fn client() -> reqwest::blocking::Client {
    install_crypto_provider();
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client")
}

/// Resolve the `Authorization` header value, mirroring `Http.__init__`:
/// prefer `GITHUB_TOKEN` (bearer); otherwise fall back to `~/.netrc` via the
/// `netrc` credential for the request host (httpx `NetRCAuth`).
fn auth_header(url: &url::Url) -> Option<String> {
    if let Ok(token) = std::env::var("GITHUB_TOKEN")
        && !token.is_empty()
    {
        return Some(format!("Bearer {token}"));
    }
    netrc_auth(url)
}

/// Look up a machine's credentials in `~/.netrc` (or `_NETRC`).
fn netrc_auth(url: &url::Url) -> Option<String> {
    let host = url.host_str()?;
    let netrc_path = std::env::var("_NETRC")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".netrc")
        });
    let contents = std::fs::read_to_string(netrc_path).ok()?;
    let entry = parse_netrc(&contents, host)?;
    // GitHub accepts the login/password pair as basic auth; with a token in
    // the password field this is the common .netrc setup.
    use base64::Engine as _;
    let raw = format!("{}:{}", entry.login, entry.password);
    Some(format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
    ))
}

struct NetrcEntry {
    login: String,
    password: String,
}

/// Minimal `.netrc` parser: build a `machine -> entry` map and return the one
/// matching `host`. Handles the `machine`/`login`/`password` tokens; `default`
/// entries are ignored (GitHub always matches a concrete machine).
fn parse_netrc(contents: &str, host: &str) -> Option<NetrcEntry> {
    use std::collections::HashMap;
    let mut machines: HashMap<String, NetrcEntry> = HashMap::new();
    let mut current: Option<String> = None;
    let mut tokens = contents.split_whitespace();
    while let Some(tok) = tokens.next() {
        match tok {
            "machine" => {
                let name = tokens.next()?.to_string();
                machines.entry(name.clone()).or_insert(NetrcEntry {
                    login: String::new(),
                    password: String::new(),
                });
                current = Some(name);
            }
            "login" => {
                let value = tokens.next()?.to_string();
                if let Some(name) = &current
                    && let Some(entry) = machines.get_mut(name)
                {
                    entry.login = value;
                }
            }
            "password" => {
                let value = tokens.next()?.to_string();
                if let Some(name) = &current
                    && let Some(entry) = machines.get_mut(name)
                {
                    entry.password = value;
                }
            }
            _ => {}
        }
    }
    machines.remove(host)
}

/// GET a URL and return the response body text, with the original's retry
/// behavior: an inner loop retries 502s, and the caller-facing `get_data`
/// wraps that in an outer retry on any error.
pub fn get_data(url: &str) -> Result<String, WatchGhaError> {
    let save = std::env::var("SAVE_DATA")
        .map(|v| v == "1")
        .unwrap_or(false);
    let mut last_err: Option<WatchGhaError> = None;
    for ntry in 0..3 {
        match get_once(url, save) {
            Ok(text) => return Ok(text),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(50 * 2u64.pow(ntry)));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| WatchGhaError("fetch failed".into())))
}

fn get_once(url: &str, save: bool) -> Result<String, WatchGhaError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| WatchGhaError(format!("Couldn't get {url:?}: invalid URL: {e}")))?;
    let client = client();
    let mut req = client.get(url);
    if let Some(auth) = auth_header(&parsed) {
        req = req.header("Authorization", auth);
    }
    let resp = req
        .send()
        .map_err(|e| http_error(url, None, e.to_string()))?;

    let status = resp.status();
    let ok = status.is_success();
    let code = status.as_u16();
    if RETRY_STATUS_CODES.contains(&code) {
        // The inner 502 retry is folded into the outer retry here; surface a
        // retryable error so get_data's backoff loop handles it.
        return Err(WatchGhaError(format!(
            "Server error '{code}' for url {url:?}"
        )));
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = resp
        .text()
        .map_err(|e| http_error(url, Some(code), e.to_string()))?;

    if !ok {
        return Err(http_error(url, Some(code), format!("status {code}")));
    }

    if save {
        save_payload(url, &text, &content_type);
    }
    Ok(text)
}

/// Build a `WatchGhaError` for a failed fetch, mirroring the original's
/// message shape (include the URL when the underlying error omits it).
fn http_error(url: &str, status: Option<u16>, detail: String) -> WatchGhaError {
    let mut msg = if url.len() > 10 && detail.contains(url) {
        detail
    } else {
        format!("Couldn't get {url:?}: {detail}")
    };
    if let Some(code) = status {
        msg.push_str(&format!("\nstatus: {code}"));
    }
    WatchGhaError(msg)
}

static SAVE_COUNT: AtomicU64 = AtomicU64::new(0);

/// Dump a fetched payload to `get_NNN.<ext>` and index it in `get_index.txt`
/// (SAVE_DATA=1 debug aid, mirroring `Http.get_data`).
fn save_payload(url: &str, text: &str, content_type: &str) {
    let n = SAVE_COUNT.fetch_add(1, Ordering::SeqCst);
    let ext = extension_for_content(content_type);
    let filename = format!("get_{n:03}{ext}");
    let index = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("get_index.txt")
        .ok();
    if let Some(mut f) = index {
        let _ = writeln!(f, "{filename}: {url}");
    }
    let _ = std::fs::write(filename, text);
}

/// Derive a filename extension from a `content-type` header, mirroring the
/// original's `extension_for_content` (strip params, then `mimetypes
/// .guess_extension`). GitHub always returns JSON, so `.json` is the practical
/// result; fall back to it for anything unrecognized.
fn extension_for_content(content_type: &str) -> String {
    let mime = content_type.split(';').next().unwrap_or("").trim();
    match mime {
        "application/json" => ".json".to_string(),
        "text/plain" => ".txt".to_string(),
        _ => ".json".to_string(),
    }
}

// base64 is only used for .netrc basic auth; keep it an optional dep path.
#[allow(unused_imports)]
use base64 as _base64_marker;
