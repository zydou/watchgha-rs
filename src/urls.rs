//! Build GitHub Actions API URLs from a repo (local dir or GitHub URL),
//! branch, and optional SHA. Mirrors `watch_runs.gha_urls`.

use regex::Regex;

use crate::error::fatal;
use crate::git_help::{git_branch, git_repo_urls};

/// Build the list of GitHub Actions run URLs for `repo`, `branch`, `sha`.
///
/// `repo` is either a local directory (whose git remotes are inspected) or a
/// GitHub URL. `branch` defaults from the local git repo when it is a
/// directory; it is required for a URL repo. `sha`, when present, filters by
/// `head_sha` instead of `branch`.
pub fn gha_urls(repo: &str, branch: Option<&str>, sha: Option<&str>) -> Vec<String> {
    let repo_urls: Vec<String>;
    let mut branch_owned = branch.map(|s| s.to_string());

    let is_dir = std::path::Path::new(repo).is_dir();
    if is_dir {
        let path = std::path::Path::new(repo);
        repo_urls = git_repo_urls(path).unwrap_or_else(|e| fatal(e, 1));
        if branch_owned.is_none() {
            branch_owned = Some(git_branch(path).unwrap_or_else(|e| fatal(e, 1)));
        }
    } else if repo.contains(':') {
        repo_urls = vec![repo.to_string()];
        if branch_owned.is_none() {
            fatal("Branch is required for URL repo", 1);
        }
    } else {
        fatal(format!("Don't understand repo '{}'", repo), 1);
    }

    // Build the query string: per_page=100 plus either head_sha or branch.
    let mut params: Vec<(&str, String)> = vec![("per_page", "100".to_string())];
    match sha {
        Some(s) if !s.is_empty() => params.push(("head_sha", s.to_string())),
        _ => {
            let b = branch_owned.expect("branch is not None here");
            params.push(("branch", b));
        }
    }
    let url_args = urlencode(&params);

    let server_url =
        std::env::var("GITHUB_SERVER_URL").unwrap_or_else(|_| "https://github.com".into());
    let api_url =
        std::env::var("GITHUB_API_URL").unwrap_or_else(|_| "https://api.github.com".into());

    // `(?:{server_url}|git@github.com)[/:]?(owner/repo)(?:\.git|/)?`
    // The server URL is regex-escaped; `git@github.com` is an SSH remote form.
    // The pattern is anchored (\A...\z) to reproduce Python's `re.fullmatch`:
    // without it, the lazy `[^/]+?` would stop at the shortest match.
    let pattern = format!(
        r"\A(?:{}|git@github\.com)[/:]?([^/]+/[^/]+?)(?:\.git|/)?\z",
        regex_escape(&server_url)
    );
    let re = Regex::new(&pattern).expect("valid repo-match regex");

    let mut github_urls = Vec::new();
    for repo_url in &repo_urls {
        if let Some(cap) = re.captures(repo_url) {
            let slug = &cap[1];
            github_urls.push(format!("{api_url}/repos/{slug}/actions/runs?{url_args}"));
        }
    }

    if github_urls.is_empty() {
        fatal(
            format!(
                "Couldn't find GitHub repo from remote URLs: {}",
                py_repr_list(&repo_urls)
            ),
            1,
        );
    }

    github_urls
}

/// Percent-encode like Python's `urllib.parse.urlencode` (quote_plus): spaces
/// become `+`, unreserved chars (`A-Za-z0-9_.-~`) are kept, everything else is
/// `%XX`-encoded from its UTF-8 bytes.
fn urlencode(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn quote_plus(s: &str) -> String {
    let mut out = String::new();
    for &byte in s.as_bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '~') {
            out.push(c);
        } else if c == ' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Regex-escape a literal string (mirrors `re.escape`): escape all
/// non-alphanumeric characters.
fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

/// Render a list of strings like Python's `repr(list_of_str)`, e.g.
/// `['https://gitlab.com/maintainer/project.git']`.
fn py_repr_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("'{s}'")).collect();
    format!("[{}]", inner.join(", "))
}

// Re-exported for callers that need the error type in scope.
#[allow(unused_imports)]
use crate::error::WatchGhaError as _WatchGhaError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencodes_branch_slashes() {
        let params = vec![
            ("per_page", "100".to_string()),
            ("branch", "joe/feature1".to_string()),
        ];
        assert_eq!(urlencode(&params), "per_page=100&branch=joe%2Ffeature1");
    }

    #[test]
    fn urlencodes_head_sha() {
        let params = vec![
            ("per_page", "100".to_string()),
            ("head_sha", "sha123".to_string()),
        ];
        assert_eq!(urlencode(&params), "per_page=100&head_sha=sha123");
    }

    /// The production repo-match pattern (anchored, as `gha_urls` builds it).
    fn repo_re(server_url: &str) -> Regex {
        Regex::new(&format!(
            r"\A(?:{}|git@github\.com)[/:]?([^/]+/[^/]+?)(?:\.git|/)?\z",
            regex_escape(server_url)
        ))
        .unwrap()
    }

    #[test]
    fn matches_https_remote() {
        let re = repo_re("https://github.com");
        let cap = re.captures("https://github.com/owner/repo.git").unwrap();
        assert_eq!(&cap[1], "owner/repo");
    }

    #[test]
    fn matches_ssh_remote() {
        let re = repo_re("https://github.com");
        let cap = re.captures("git@github.com:somebody/therepo.git").unwrap();
        assert_eq!(&cap[1], "somebody/therepo");
    }

    #[test]
    fn does_not_match_gitlab() {
        let re = repo_re("https://github.com");
        assert!(
            re.captures("https://gitlab.com/maintainer/project.git")
                .is_none()
        );
    }

    #[test]
    fn enterprise_server_url() {
        // GITHUB_SERVER_URL=git@git.mydomain.com must match the SSH-like form.
        let re = repo_re("git@git.mydomain.com");
        let cap = re
            .captures("git@git.mydomain.com:davidszotten/davidrepo")
            .unwrap();
        assert_eq!(&cap[1], "davidszotten/davidrepo");
    }

    #[test]
    fn repr_list() {
        assert_eq!(
            py_repr_list(&["https://gitlab.com/maintainer/project.git".to_string()]),
            "['https://gitlab.com/maintainer/project.git']"
        );
    }

    // Silence unused warning for the type re-export.
    #[allow(dead_code)]
    fn _t(_: _WatchGhaError) {}
}
