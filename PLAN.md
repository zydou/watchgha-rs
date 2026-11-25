# Port plan: watchgha

## Progress snapshot

- Upstream pin: <https://github.com/nedbat/watchgha.git> @ 34d1a36096a2b6cfd5fe3d3a7d739d7ab1de9343 (2.7.0)
- Sync target: none
- Oracle binary: `.oracle-venv/bin/watchgha` (watchgha 2.7.0, built from `upstream/` @ 34d1a36 via `uv pip install ./upstream`, Python 3.13.18)
- Working end-to-end: CLI skeleton + URL/GIT resolution + HTTP fetch + DATA aggregation + RENDER (plain) + watch LOOP (incl. Ctrl-C→exit 2) + deprecated `watch_gha_runs` entry point; 7/7 parity cases green
- Current stage: S3
- Next: S3 slices — remaining LOOP (TTY alt-screen live redraw + SIGWINCH, LOOP-3); D-2 `.netrc` auth register update
- Blockers: none
- CI: all required targets green on `main`; nightly publishes on push (Release run green post PR #2)

Notes from landing S2 (CI/build):

- Replaced `chrono` 0.4.45 with `jiff` 1.0.3: RUSTSEC-2026-0543 flags chrono unmaintained (no fixed upgrade), which failed G-AUDIT and violated INV-DEPS.
- reqwest uses `rustls-no-provider` + the `ring` crypto provider (not the default `aws-lc-rs`), because aws-lc-sys needs cmake and a musl C compiler that the glibc CI runners lack.
- `release.yml` installs `musl-tools` on the musl jobs and points cc-rs (`CC_<target>`) and the cargo linker at `musl-gcc` for the **test step only** (via `exec env`, not `$GITHUB_ENV`). Scoping to the test step keeps the release `cargo zigbuild` on zig, so the musl artifact stays a plain static executable (a musl-gcc link produces static-pie, which the artifact-verify grep rejects).

Dependency decisions (INV-DEPS mapping, finalized in S2):

- `click` → `clap` (derive)
- `rich` → hand-rolled renderer (`render.rs`): same markup, plain-text strip on non-TTY
- `httpx` (+`NetRCAuth`) → `reqwest` (blocking, rustls) + manual `.netrc` parse (`http.rs`)
- `trio` → std threads (`main.rs` watch loop)
- `dulwich` → pure-Rust `.git` reader (`git_help.rs`); no git dependency
- `datetime`/`pytz` → `jiff` 1.0.3 (replaced `chrono` 0.4.45, which RUSTSEC-2026-0543 flags unmaintained; G-AUDIT red until swapped)
- `exceptiongroup` → not needed (no structured concurrency)

## Original overview (from reading upstream @ 34d1a36)

`watchgha` is a Python terminal application that watches GitHub Actions runs on a branch. It resolves a repo (local dir or GitHub URL) + branch into GitHub API URLs, repeatedly fetches workflow runs and their jobs, groups runs into "events", renders a live table (rich), and refreshes until all jobs finish. Entry points: `watchgha.watch_runs:main` (the `watchgha` command) and a deprecated `watch_gha_runs` wrapper.

The finalized original-deps → Rust mapping is recorded in the Progress snapshot above.

Environment variables the original reads:

- `GITHUB_TOKEN` — bearer auth (else `.netrc` via httpx `NetRCAuth`)
- `GITHUB_SERVER_URL` (default `https://github.com`) — used to parse remote URLs
- `GITHUB_API_URL` (default `https://api.github.com`) — base for API calls
- `SAVE_DATA` — if `1`/truthy int, dump every fetched payload to `get_NNN.<ext>` and index them in `get_index.txt`

## Capability inventory

| ID          | Form | Capability                             | Args/options (type, default, allowed) | I/O & exit codes                                               | Env/config                        | Deps           | Class            | Slice | Notes |
| ----------- | ---- | -------------------------------------- | ------------------------------------- | -------------------------------------------------------------- | --------------------------------- | -------------- | ---------------- | ----- | ----- |
| CLI-1       | CLI  | `watchgha` top-level command           | see OPT-* / ARG-* rows                | stdout render, stderr errors, exit 0/1/2                       | —                                 | clap           | direct match     | S2    | Docstring becomes help; rewrite in own words |
| OPT-SHA     | CLI  | `--sha`                                | string, no default, full SHA          | —                                                              | —                                 | clap           | direct match     | S2    | Passed as `head_sha` param |
| OPT-POLL    | CLI  | `--poll`                               | int, default 15                       | seconds between refreshes                                      | —                                 | clap           | direct match     | S2    | `show_default` in help |
| OPT-WAIT    | CLI  | `--wait` / `--wait-for-start`          | flag, false                           | wait for jobs to start before live view                        | —                                 | clap           | direct match     | S2    | Two spellings, one flag |
| OPT-ONLY    | CLI  | `--only`                               | string, no default                    | comma-separated case-insensitive substrings filter on run name | —                                 | clap           | direct match     | S3    | Split on `,`, trim whitespace |
| OPT-MESSAGE | CLI  | `--message`                            | string, no default                    | line shown at top of output                                    | —                                 | clap           | direct match     | S3    | Prepended to render |
| ARG-REPO    | CLI  | `REPO` positional                      | string, default `.`                   | local dir or GitHub URL                                        | —                                 | clap           | direct match     | S2    | Dispatch: isdir vs contains `:` |
| ARG-BRANCH  | CLI  | `BRANCH` positional                    | string, optional                      | defaults from git                                              | —                                 | clap           | direct match     | S2    |       |
| HELP-1      | CLI  | `--help`                               | flag                                  | stdout, exit 0                                                 | —                                 | clap           | known divergence | S2    | Wording is ours; structure mirrors click |
| URL-1       | CLI  | Resolve local dir → remote URLs        | REPO isdir                            | list of remote URLs                                            | —                                 | git            | direct match     | S2    | git_help.git_repo_urls |
| URL-2       | CLI  | Resolve URL repo                       | REPO contains `:`                     | single URL                                                     | —                                 | —              | direct match     | S2    | branch required else fatal |
| URL-3       | CLI  | Branch defaulting from git             | —                                     | current branch name                                            | —                                 | git            | direct match     | S2    | git_help.git_branch |
| URL-4       | CLI  | Build API URL from remote              | per_page=100, branch\|head_sha        | `…/actions/runs?…`                                             | GITHUB_SERVER_URL, GITHUB_API_URL | —              | direct match     | S2    | Regex over https/git@/enterprise forms; urlencode params |
| URL-5       | CLI  | Fatal: branch required for URL repo    | —                                     | stderr msg, exit 1                                             | —                                 | —              | direct match     | S2    |       |
| URL-6       | CLI  | Fatal: unrecognized repo               | —                                     | stderr msg, exit 1                                             | —                                 | —              | direct match     | S2    | `Don't understand repo {repo!r}` |
| URL-7       | CLI  | Fatal: no GitHub repo found            | —                                     | stderr msg, exit 1                                             | —                                 | —              | direct match     | S3    | Lists the remote URLs |
| GIT-1       | CLI  | Read git remote URLs                   | dir                                   | yields `remote.*.url`                                          | .git/config                       | git lib        | direct match     | S2    | dulwich → git2/git |
| GIT-2       | CLI  | Read active branch                     | dir                                   | branch name                                                    | HEAD                              | git lib        | direct match     | S2    |       |
| HTTP-1      | CLI  | Fetch URL with auth                    | —                                     | response text                                                  | GITHUB_TOKEN                      | reqwest        | direct match     | S3    | Bearer token if set |
| HTTP-2      | CLI  | `.netrc` auth fallback                 | —                                     | —                                                              | ~/.netrc                          | reqwest/.netrc | known divergence | S3    | httpx NetRCAuth; decide Rust approach |
| HTTP-3      | CLI  | Retry on 502 + outer retry             | —                                     | —                                                              | —                                 | —              | direct match     | S3    | Inner 3 tries on 502, outer 3 tries any error; backoff 0.05*2^n |
| HTTP-4      | CLI  | SAVE_DATA dump                         | —                                     | get_NNN.ext + get_index.txt                                    | SAVE_DATA                         | —              | direct match     | S3    | ext from content-type |
| HTTP-5      | CLI  | Error message w/ JSON body             | —                                     | WatchGhaError text                                             | —                                 | —              | direct match     | S3    | Appends `label: text` lines or raw text |
| DATA-1      | CLI  | Fetch runs from all URLs concurrently  | urls                                  | workflow_runs list                                             | —                                 | async          | direct match     | S3    | trio nursery → Rust concurrency |
| DATA-2      | CLI  | Dedupe runs by id                      | —                                     | —                                                              | —                                 | —              | direct match     | S3    | Duplicate remotes |
| DATA-3      | CLI  | Sort runs                              | run_sort_key                          | —                                                              | —                                 | —              | direct match     | S3    | (defuzz(started_dt), head_sha, event, name) reverse |
| DATA-4      | CLI  | Group runs into events                 | run_group_key                         | —                                                              | —                                 | —              | direct match     | S3    | (defuzz(started_dt), head_sha, event) |
| DATA-5      | CLI  | Exclude `.github/` run names           | —                                     | —                                                              | —                                 | —              | direct match     | S3    | Bad yml parse names |
| DATA-6      | CLI  | Drop events older than 7 days          | —                                     | —                                                              | now (UTC)                         | —              | direct match     | S3    | days_old > 7 |
| DATA-7      | CLI  | `--only` filter                        | only_words                            | —                                                              | —                                 | —              | direct match     | S3    | substring in lowercased name |
| DATA-8      | CLI  | Fetch jobs per run                     | jobs_url                              | sorted jobs                                                    | —                                 | async          | direct match     | S3    | `?per_page=100`, job_sort_key |
| DATA-9      | CLI  | Status totals                          | —                                     | done/succeeded/total/num_succeeded/num_failed                  | —                                 | —              | direct match     | S3    | CONCLUSION_BAD → failed |
| RENDER-1    | CLI  | Event header line                      | —                                     | display_title head_branch [event] sha@time                     | —                                 | term           | direct match     | S3    | sha `.12`, nice_time |
| RENDER-2    | CLI  | Run line                               | —                                     | icon summary name view <id>                                    | —                                 | term           | direct match     | S3    | CSTYLES/CICONS; hyperlink |
| RENDER-3    | CLI  | Job lines + stepdots                   | —                                     | name icon stepdots current_step                                | —                                 | term           | direct match     | S3    | Step state machine (failure/in_progress/last/skipped) |
| RENDER-4    | CLI  | Control-char scrub                     | —                                     | strip `[\x00-\x1f\x7f-\x9f]`                                   | —                                 | —              | direct match     | S3    | Applied to every output line |
| TIME-1      | CLI  | nice_time                              | datetime                              | localized `%I:%M%p` / `%m-%d` / `%Y-%m-%d`                     | local tz                          | chrono         | direct match     | S3    | astimezone local; lowercased |
| TIME-2      | CLI  | to_datetime                            | ISO string                            | datetime                                                       | —                                 | chrono         | direct match     | S3    | Replace `Z`→`+00:00` |
| TIME-3      | CLI  | human_key                              | string                                | natural sort key                                               | —                                 | —              | direct match     | S3    | Split on `\d+`, tryint, casefold |
| TIME-4      | CLI  | DatetimeBucketer.defuzz                | window=5                              | canonical instant                                              | —                                 | —              | direct match     | S3    | rounds map, 2 roundings |
| LOOP-1      | CLI  | Watch loop                             | poll, wait                            | redraw until done                                              | —                                 | term           | direct match     | S3    | Interval accounts for work time |
| LOOP-2      | CLI  | wait-for-start behavior                | —                                     | exit live view once started                                    | —                                 | —              | direct match     | S3    | Break when not done pre-start |
| LOOP-3      | CLI  | Alt-screen + SIGWINCH redraw           | —                                     | —                                                              | —                                 | term           | direct match     | S3    | Windows: no SIGWINCH (swallow) |
| LOOP-4      | CLI  | OSC 9;4 terminal progress              | —                                     | `\033]9;4;st;pr\033\\`                                         | —                                 | —              | direct match     | S3    | state 1/2, percent |
| LOOP-5      | CLI  | Exit codes                             | —                                     | 0 success, 1 fail/error, 2 interrupted                         | —                                 | —              | direct match     | S3    | fatal default 1 |
| LOOP-6      | CLI  | Ctrl-C handling                        | —                                     | prints final, exit 2                                           | —                                 | —              | direct match     | S3    | interrupted flag |
| DEP-1       | CLI  | Deprecated `watch_gha_runs`            | same as main                          | warning + delay + run main                                     | —                                 | clap           | known divergence | S3    | Delay = 1+(days since 2026-01-18)/365*9 |
| DEMO-1      | lib  | `python -m watchgha.demo` styling demo | —                                     | renders sample data                                            | —                                 | term           | not portable     | —     | Dev-only styling preview; see N-1 |

## Registers

### Divergences (D-###)

| ID  | Capability | Scenario                      | Original behavior                              | Rust behavior                                                                                                                                    | Rationale                                                                                                                                    | Parity case |
| --- | ---------- | ----------------------------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------- | ----------- |
| D-1 | HELP-1     | `watchgha --help`             | click-generated help text                      | clap-generated help; same options/structure, our wording                                                                                         | Help text is not functional (INV-PARITY); wording is rewritten per guide                                                                     | parity: options present, exit 0 |
| D-2 | HTTP-2     | Private repo, no GITHUB_TOKEN | httpx `NetRCAuth` reads `~/.netrc`             | TBD in S3 (reqwest + .netrc crate or manual parse)                                                                                               | Must preserve `.netrc` auth or register non-portable                                                                                         | parity: auth header from .netrc |
| D-3 | DEP-1      | `watch_gha_runs` invoked      | Deprecated warning + sleep + delegate          | Reimplemented: warn (markup stripped on non-TTY stderr) + delay `1+(days/365)*9` + run real command + warn again; dispatched by argv[0] basename | Backward-compat entry point is in scope (a CLI)                                                                                              | unit: warning text + delay math (integration case would sleep unboundedly, so verified by unit test) |
| D-4 | CLI-1      | `watchgha --version`          | click command has no `--version` flag (errors) | clap `version` → prints `watchgha 2.7.0`, exit 0                                                                                                 | G-SMOKE gate requires a version invocation; version output is non-functional (INV-PARITY), so adding it is safe and expected of a native CLI | smoke: `watchgha --version` exit 0 |

### Bugs fixed (B-###)

| ID         | Capability | Defect | Fix | Evidence | Parity case |
| ---------- | ---------- | ------ | --- | -------- | ----------- |
| (none yet) |            |        |     |          |             |

### Non-portable (N-###)

| ID  | Capability             | Original behavior                                 | Why not portable                                                                                    | Mitigation |
| --- | ---------------------- | ------------------------------------------------- | --------------------------------------------------------------------------------------------------- | ---------- |
| N-1 | DEMO-1 `watchgha.demo` | `python -m watchgha.demo` prints a styling sample | It is a dev-only styling preview module, not part of the shipped CLI contract; a Python-only helper | Omit; the same rendering is exercised via RENDER-* slices and the parity harness |

## Phases

- [x] Phase 0: Recon
  - [x] Commit starting scaffold (cargo check, Cargo.lock)
  - [x] Pin upstream @ 34d1a36, adopt Apache-2.0 license
  - [x] Full inventory classified (this table) — every row has class + slice
  - [x] Push main, install rulesets (main + tags active)
- [x] Phase 1: Oracle
  - [x] Build `tests/parity/` harness (capture/check/selfcheck + mock GitHub API server)
  - [x] Capture from original v2.7.0 binary; provenance recorded in Progress snapshot
  - [x] Self-check green against original (7/7 cases deterministic)
- [ ] Phase 2: Skeleton
  - [x] CLI skeleton (clap), error/exit model, config/env loading
  - [x] One self-contained slice end to end (URL/GIT → HTTP → DATA → RENDER → LOOP with 7 mock parity cases)
  - [x] CI green: G-FMT/G-LINT/G-TEST/G-PARITY (parity job wired into release.yml; local gates green)
- [ ] Phase 3: Slices (bottom-up)
  - [ ] TIME-* pure helpers
  - [ ] URL-* / GIT-* repo+branch resolution
  - [ ] HTTP-* fetching
  - [ ] DATA-* aggregation
  - [ ] RENDER-* display
  - [ ] LOOP-* watch loop + exit codes
    - [x] LOOP-6 Ctrl-C → stop after final frame, exit 2 (functional: mock in-progress run, SIGINT → exit 2 + `** interrupted **` on stderr)
    - [ ] LOOP-3 TTY alt-screen live redraw + SIGWINCH (render_progress currently a stub; non-TTY path is the parity-exercised one)
- [ ] Phase 4: Harden
- [ ] Phase 5: Review
- [ ] Phase 6: Ship
