//! Fetch JSON data from URLs, with auth and retry. Mirrors `http_help`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::error::WatchGhaError;

/// Status codes the inner fetch retries on (mirrors `RETRY_STATUS_CODES`).
const RETRY_STATUS_CODES: [u16; 1] = [502];
/// Total attempts per URL. The original retries a 502 up to 3× inside an outer
/// any-error retry of 3× (up to 9 attempts); the port folds both into one loop,
/// so this is the total (see D-5).
const RETRY_ATTEMPTS: u32 = 3;

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
/// prefer a `GITHUB_TOKEN` (bearer); otherwise fall back to `~/.netrc` via the
/// `netrc` credential for the request host (httpx `NetRCAuth`). The token is
/// injected by the caller (which reads the env) so the precedence is testable.
fn auth_header(url: &url::Url, token: Option<&str>) -> Option<String> {
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        return Some(format!("Bearer {token}"));
    }
    netrc_auth(url)
}

/// Look up a machine's credentials in `~/.netrc` (or `_NETRC`).
fn netrc_auth(url: &url::Url) -> Option<String> {
    netrc_auth_from(url, &netrc_path())
}

/// Resolve the `.netrc` credentials for `url`'s host from an explicit file
/// path. Split from [`netrc_auth`] so tests can point at a temp file instead
/// of the process `HOME`.
fn netrc_auth_from(url: &url::Url, path: &Path) -> Option<String> {
    let host = url.host_str()?;
    let contents = std::fs::read_to_string(path).ok()?;
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

/// The `.netrc` file to read: the stdlib `netrc.netrc()` honours the `NETRC`
/// environment variable, else falls back to `~/.netrc`.
fn netrc_path() -> PathBuf {
    let netrc = std::env::var("NETRC").ok().filter(|p| !p.is_empty());
    let home = std::env::var("HOME").ok();
    netrc_path_from(netrc.as_deref(), home.as_deref())
}

/// Pure resolution of the netrc path from already-read env values (so tests
/// don't mutate process env, which races across parallel tests). An empty
/// `NETRC` is treated as unset, matching stdlib `netrc.netrc()`.
fn netrc_path_from(netrc: Option<&str>, home: Option<&str>) -> PathBuf {
    if let Some(p) = netrc.filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    PathBuf::from(home.unwrap_or_default()).join(".netrc")
}

struct NetrcEntry {
    login: String,
    password: String,
}

/// Minimal `.netrc` parser mirroring the stdlib `netrc.authenticators(host)`:
/// return the entry for `host`, or the `default` entry when the host has no
/// match, or `None` when neither exists. Handles the `machine`/`default`/
/// `login`/`password` tokens; missing login/password default to empty strings.
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
            // `default` applies to any host without its own `machine` entry.
            "default" => {
                machines.entry("default".to_string()).or_insert(NetrcEntry {
                    login: String::new(),
                    password: String::new(),
                });
                current = Some("default".to_string());
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
    // A concrete host wins over `default` (stdlib authenticators semantics).
    if let Some(entry) = machines.remove(host) {
        return Some(entry);
    }
    machines.remove("default")
}

/// GET a URL and return the response body text, retrying any error up to
/// `RETRY_ATTEMPTS` times with `0.05*2^n` backoff. See D-5: the original wraps
/// an inner 502 retry in an outer any-error retry; the port folds both into
/// this single loop, so a persistently-failing URL is attempted fewer times.
pub fn get_data(url: &str) -> Result<String, WatchGhaError> {
    let save = std::env::var("SAVE_DATA")
        .map(|v| v == "1")
        .unwrap_or(false);
    retry_times(
        || get_once(url, save),
        RETRY_ATTEMPTS,
        |n| Duration::from_millis(50 * 2u64.pow(n)),
        std::thread::sleep,
    )
}

/// Retry `op` up to `attempts` times, sleeping `backoff(n)` after each failed
/// attempt via `sleep`. Returns the last error if every attempt fails. `op` and
/// `sleep` are injected so the attempt count is deterministically testable.
fn retry_times<T>(
    mut op: impl FnMut() -> Result<T, WatchGhaError>,
    attempts: u32,
    backoff: impl Fn(u32) -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Result<T, WatchGhaError> {
    let mut last_err: Option<WatchGhaError> = None;
    for ntry in 0..attempts {
        match op() {
            Ok(text) => return Ok(text),
            Err(e) => {
                last_err = Some(e);
                if ntry + 1 < attempts {
                    sleep(backoff(ntry));
                }
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
    let token = std::env::var("GITHUB_TOKEN").ok();
    if let Some(auth) = auth_header(&parsed, token.as_deref()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netrc_parses_machine_login_password() {
        let contents = "machine github.com\n  login octocat\n  password ghp_tok\n\
                        machine other.com\n  login x\n  password y\n";
        let e = parse_netrc(contents, "github.com").expect("github.com entry");
        assert_eq!(e.login, "octocat");
        assert_eq!(e.password, "ghp_tok");
        // A host with no entry and no `default` yields None.
        assert!(parse_netrc(contents, "gitlab.com").is_none());
    }

    #[test]
    fn netrc_default_entry_is_fallback() {
        // Mirrors stdlib authenticators(): a `default` entry serves hosts with
        // no `machine` of their own, but a concrete machine wins.
        let contents = "machine github.com\n  login octocat\n  password gh\n\
                        default\n  login anon\n  password ftp\n";
        let d = parse_netrc(contents, "gitlab.com").expect("default fallback");
        assert_eq!(d.login, "anon");
        let g = parse_netrc(contents, "github.com").expect("concrete machine");
        assert_eq!(g.login, "octocat");
    }

    #[test]
    fn netrc_auth_from_reads_file_and_encodes() {
        let mut path = std::env::temp_dir();
        path.push(format!("watchgha-netrc-test-{}.txt", std::process::id()));
        std::fs::write(&path, "machine github.com\nlogin oc\npassword tok\n").unwrap();
        let url = url::Url::parse("https://github.com/owner/repo").unwrap();
        let header = netrc_auth_from(&url, &path).expect("basic header");
        assert_eq!(header, "Basic b2M6dG9r");
        // No matching host or default -> no auth.
        let other = url::Url::parse("https://gitlab.com/x/y").unwrap();
        assert!(netrc_auth_from(&other, &path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn netrc_path_prefers_env_then_home() {
        // Mirrors stdlib netrc.netrc(): NETRC env wins, else $HOME/.netrc.
        // Pure inputs avoid mutating process env (races parallel tests).
        assert_eq!(
            netrc_path_from(Some("/tmp/custom-netrc"), Some("/home/u")),
            PathBuf::from("/tmp/custom-netrc")
        );
        assert_eq!(
            netrc_path_from(None, Some("/home/u")),
            PathBuf::from("/home/u/.netrc")
        );
        // An empty NETRC is treated as unset.
        assert_eq!(
            netrc_path_from(Some(""), Some("/home/u")),
            PathBuf::from("/home/u/.netrc")
        );
        // No HOME at all degrades to a bare ".netrc" (home empty).
        assert_eq!(netrc_path_from(None, None), PathBuf::from(".netrc"));
    }

    #[test]
    fn netrc_builds_basic_auth_header() {
        // netrc_auth is host-driven; exercise the Basic encoding directly.
        use base64::Engine as _;
        let raw = "octocat:ghp_tok";
        let header = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
        );
        assert_eq!(header, "Basic b2N0b2NhdDpnaHBfdG9r");
    }

    #[test]
    fn auth_header_prefers_token_over_netrc() {
        let url = url::Url::parse("https://github.com/owner/repo/actions/runs").unwrap();
        // A token (even with a .netrc present) wins as Bearer.
        assert_eq!(
            auth_header(&url, Some("env-token")),
            Some("Bearer env-token".to_string())
        );
        // An empty token is treated as absent, falling through to .netrc (None
        // when no entry matches, as here without a HOME netrc).
        assert_eq!(auth_header(&url, Some("")), netrc_auth(&url));
        assert_eq!(auth_header(&url, None), netrc_auth(&url));
    }

    #[test]
    fn retry_times_attempts_three_then_gives_up() {
        // Injected zero sleep keeps the test fast; count the attempts.
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let res: Result<String, WatchGhaError> = retry_times(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(WatchGhaError("boom".into()))
            },
            RETRY_ATTEMPTS,
            |n| Duration::from_millis(50 * 2u64.pow(n)),
            |_| {},
        );
        assert!(res.is_err(), "persistent failure yields the last error");
        assert_eq!(attempts.load(Ordering::SeqCst), RETRY_ATTEMPTS);
    }

    #[test]
    fn retry_times_stops_on_first_success() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let res = retry_times(
            || {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(WatchGhaError("transient".into()))
                } else {
                    Ok("ok".to_string())
                }
            },
            RETRY_ATTEMPTS,
            |_| Duration::ZERO,
            |_| {},
        );
        assert_eq!(res.unwrap(), "ok");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "retries once then succeeds"
        );
    }
}
