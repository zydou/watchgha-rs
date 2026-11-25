//! Read remote URLs and the active branch from a local git repository.
//!
//! The original uses `dulwich`, which reads `.git/config` and `.git/HEAD`
//! directly without shelling out to `git`. We do the same in pure Rust so the
//! binary stays self-contained and statically linkable (no libgit2 C dep, no
//! `git` subprocess).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::WatchGhaError;

/// Locate the git directory for `dir`, walking up through parent directories.
///
/// Handles the two layouts:
/// - a normal repo: `<dir>/.git/` is a directory;
/// - a worktree/submodule: `<dir>/.git` is a file containing `gitdir: <path>`.
///
/// Returns `None` when no repository is found (matching dulwich's `NotGitRepository`).
fn discover_gitdir(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir.to_path_buf());
    while let Some(d) = cur {
        let dotgit = d.join(".git");
        if dotgit.is_dir() {
            return Some(dotgit);
        }
        if dotgit.is_file() {
            // A "gitdir: <path>" pointer file (worktree / submodule).
            let contents = std::fs::read_to_string(&dotgit).ok()?;
            let line = contents.lines().next()?.trim();
            let target = line.strip_prefix("gitdir:")?.trim();
            let p = PathBuf::from(target);
            return Some(if p.is_absolute() { p } else { d.join(p) });
        }
        // A bare repo has HEAD/config/objects directly in `d`.
        if d.join("HEAD").is_file() && d.join("config").is_file() {
            return Some(d);
        }
        cur = d.parent().map(|p| p.to_path_buf());
    }
    None
}

/// Find all the remote URLs for the git repo at `dir`, in config-file order.
///
/// Mirrors `git_help.git_repo_urls`: iterate the `[remote "<name>"]` sections
/// of `.git/config` and yield each `url` value.
pub fn git_repo_urls(dir: &Path) -> Result<Vec<String>, WatchGhaError> {
    let gitdir = discover_gitdir(dir)
        .ok_or_else(|| WatchGhaError(format!("No git repository found at {}", dir.display())))?;
    let config_path = gitdir.join("config");
    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| WatchGhaError(format!("Couldn't read {}: {e}", config_path.display())))?;
    Ok(parse_remote_urls(&contents))
}

/// Get the current git branch name for the repo at `dir`.
///
/// Mirrors `git_help.git_branch` (dulwich `active_branch`): read `HEAD` and
/// return the branch name after `refs: refs/heads/`.
pub fn git_branch(dir: &Path) -> Result<String, WatchGhaError> {
    let gitdir = discover_gitdir(dir)
        .ok_or_else(|| WatchGhaError(format!("No git repository found at {}", dir.display())))?;
    let head_path = gitdir.join("HEAD");
    let contents = std::fs::read_to_string(&head_path)
        .map_err(|e| WatchGhaError(format!("Couldn't read {}: {e}", head_path.display())))?;
    let line = contents.lines().next().unwrap_or("").trim();
    if let Some(r#ref) = line.strip_prefix("ref:") {
        let r#ref = r#ref.trim();
        let name = r#ref.strip_prefix("refs/heads/").unwrap_or(r#ref);
        Ok(name.to_string())
    } else {
        // Detached HEAD: HEAD holds a raw object id.
        Ok(line.to_string())
    }
}

/// Parse a git-config INI and return `remote -> url` in file order.
///
/// Only what `git_repo_urls` needs: `[remote "<name>"]` sections and their
/// `url` key. Config values may be double-quoted; we strip matching quotes.
fn parse_remote_urls(contents: &str) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut remotes: HashMap<String, String> = HashMap::new();
    let mut current_remote: Option<String> = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            let body = line.trim_start_matches('[').trim_end_matches(']').trim();
            // `[remote "origin"]` -> section "remote", subsection "origin".
            let (section, subsection) = match body.split_once(char::is_whitespace) {
                Some((s, rest)) => (s, Some(rest.trim())),
                None => (body, None),
            };
            if section.eq_ignore_ascii_case("remote") {
                let name = subsection
                    .map(|s| s.trim_matches('"').to_string())
                    .unwrap_or_default();
                if !order.contains(&name) {
                    order.push(name.clone());
                }
                current_remote = Some(name);
            } else {
                current_remote = None;
            }
        } else if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
            && let Some(remote) = &current_remote
        {
            let val = unquote(value.trim());
            remotes.insert(remote.clone(), val);
        }
    }

    order
        .into_iter()
        .filter_map(|name| remotes.get(&name).cloned())
        .collect()
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_urls_in_order() {
        let cfg = "[core]\n\
                   \trepositoryformatversion = 0\n\
                   [remote \"origin\"]\n\
                   \turl = https://github.com/owner/repo.git\n\
                   \tfetch = +refs/heads/*:refs/remotes/origin/*\n\
                   [remote \"upstream\"]\n\
                   \turl = git@github.com:up/stream.git\n";
        assert_eq!(
            parse_remote_urls(cfg),
            vec![
                "https://github.com/owner/repo.git".to_string(),
                "git@github.com:up/stream.git".to_string(),
            ]
        );
    }

    #[test]
    fn no_remotes_yields_empty() {
        let cfg = "[core]\n\trepositoryformatversion = 0\n";
        assert!(parse_remote_urls(cfg).is_empty());
    }

    #[test]
    fn git_branch_reads_head() {
        let tmp = std::env::temp_dir().join("watchgha-test-head");
        let gitdir = tmp.join(".git");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        let b = git_branch(&tmp).unwrap();
        assert_eq!(b, "feature/x");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
