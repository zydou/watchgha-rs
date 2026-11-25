# watchgha-rs

A native Rust port of [watchgha](https://github.com/nedbat/watchgha) — a terminal tool that watches GitHub Actions runs on a branch and redraws a live table until every job finishes.

This port tracks upstream **watchgha 2.7.0** at commit `34d1a36096a2b6cfd5fe3d3a7d739d7ab1de9343`. It is a self-contained static binary: no Python interpreter, no runtime, and no bundled upstream source. Correctness is pinned to the original by a parity harness whose fixtures were captured from a real build of that upstream commit.

## Install

Grab a prebuilt archive from the [latest release](https://github.com/zydou/watchgha-rs/releases) and unpack it for your platform:

| Platform                        | Archive |
| ------------------------------- | ------- |
| Linux x86_64 (static, any libc) | `watchgha-x86_64-unknown-linux-musl.tar.xz` |
| Linux x86_64 (glibc ≥ 2.18)     | `watchgha-x86_64-unknown-linux-gnu.tar.xz` |
| Linux arm64 (static)            | `watchgha-aarch64-unknown-linux-musl.tar.xz` |
| Linux arm64 (glibc ≥ 2.18)      | `watchgha-aarch64-unknown-linux-gnu.tar.xz` |
| macOS Apple Silicon             | `watchgha-aarch64-apple-darwin.tar.xz` |
| macOS Intel                     | `watchgha-x86_64-apple-darwin.tar.xz` |

The `musl` Linux builds are fully static and run on any Linux regardless of its glibc version; prefer those unless you have a reason to use the glibc build. The macOS builds target macOS 13.0 and later.

```sh
tar -xf watchgha-x86_64-unknown-linux-musl.tar.xz
./watchgha --help
```

## Usage

Run it from a repository checkout (or pass a GitHub URL) and it polls GitHub Actions until all jobs on the branch complete:

```sh
watchgha                    # watch the current repo's default branch
watchgha . main             # a local dir + branch
watchgha https://github.com/owner/repo.git feature-branch
```

Options:

- `--poll SECONDS` — seconds between refreshes (default 15).
- `--wait` / `--wait-for-start` — keep polling until the jobs have started before showing the live view.
- `--sha FULL_SHA` — watch a specific commit instead of a branch.
- `--only WORDS` — comma-separated substrings; only workflows whose names contain one of them are shown.
- `--message TEXT` — a line shown above the table.

Exit codes: `0` when all runs succeeded, `1` when a run failed or a fatal error occurred, and `2` when interrupted with Ctrl-C.

### Environment

- `GITHUB_TOKEN` — bearer token for the GitHub API. Without it, credentials are read from a `.netrc` file (`NETRC` env var or `~/.netrc`).
- `GITHUB_API_URL` / `GITHUB_SERVER_URL` — override the API and server hosts (used for GitHub Enterprise Server).
- `SAVE_DATA=1` — debug aid: dump every fetched payload to `get_NNN.<ext>` and index them in `get_index.txt`.

## Known divergences from the original

The port matches the original's CLI behavior; where it intentionally differs, the difference is recorded in `PLAN.md`:

- **Help text** — rewritten in this project's own words; the option surface is the same.
- **`--version`** — the original's `click` command has no `--version`; the port adds one.
- **HTTP retries** — a URL that keeps returning 502 is retried 3 times total, where the original could attempt up to 9 (a folded inner/outer retry). The same error and exit code result either way.
- **Concurrent fetches** — the original fetches multiple remotes concurrently; the port fetches sequentially. Output is identical (results are sorted), only latency differs.
- **Deprecated `watch_gha_runs` entry point** — reimplemented, including its growing deprecation delay.

## License

Apache-2.0, matching the upstream project. See `LICENSE.txt`.
