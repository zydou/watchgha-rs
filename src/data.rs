//! Data collection and aggregation for watchgha. Mirrors `data_core`.

use jiff::Timestamp;
use serde_json::Value;

use crate::error::WatchGhaError;
use crate::render::CONCLUSION_BAD;
use crate::time_util::{DatetimeBucketer, human_key, to_datetime};

/// Aggregated status of all fetched jobs.
#[derive(Debug, Default, Clone)]
pub struct Status {
    pub done: bool,
    pub succeeded: bool,
    pub total: u64,
    pub num_succeeded: u64,
    pub num_failed: u64,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub name: Option<String>,
    pub status: String,
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub created_dt: Timestamp,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone)]
pub struct Run {
    pub name: String,
    pub display_title: String,
    pub head_branch: String,
    pub head_sha: String,
    pub event: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: String,
    pub jobs_url: String,
    pub started_dt: Timestamp,
    /// The canonicalized start time after bucketer.defuzz, used only for
    /// sorting and grouping (the raw `started_dt` is used for display).
    pub defuzzed_dt: Timestamp,
    pub jobs: Vec<Job>,
}

/// A fetch function: URL -> body text. Lets the demo/tests inject sample data.
pub type DataFn<'a> = &'a dyn Fn(&str) -> Result<String, WatchGhaError>;

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn i(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

/// Fetch and aggregate runs into grouped events, mirroring `get_events`.
///
/// Runs are fetched from all URLs, de-duplicated by id, sorted, grouped by
/// (bucketed start time, sha, event), filtered (drop unparsable `.github/`
/// names, events older than 7 days, and apply `--only`), then jobs are loaded
/// for each surviving run.
pub fn get_events(
    urls: &[String],
    datafn: DataFn<'_>,
    only_words: Option<&[String]>,
    bucketer: &mut DatetimeBucketer,
) -> Result<Vec<Vec<Run>>, WatchGhaError> {
    // Fetch runs from all URLs, then de-duplicate by id.
    let mut runs: Vec<Value> = Vec::new();
    for url in urls {
        let body = datafn(url)?;
        let parsed: Value = serde_json::from_str(&body)
            .map_err(|e| WatchGhaError(format!("Couldn't parse runs JSON: {e}")))?;
        if let Some(list) = parsed.get("workflow_runs").and_then(|x| x.as_array()) {
            runs.extend(list.iter().cloned());
        }
    }

    // De-duplicate by id. Like Python's `{r["id"]: r for r in runs}`, a
    // duplicate id keeps its first insertion position but takes the last
    // value, so order is by first occurrence.
    let mut order: Vec<i64> = Vec::new();
    let mut by_id: std::collections::HashMap<i64, Value> = std::collections::HashMap::new();
    for r in runs {
        let id = i(&r, "id");
        if !by_id.contains_key(&id) {
            order.push(id);
        }
        by_id.insert(id, r);
    }
    let runs: Vec<Value> = order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect();

    // Parse each run and canonicalize its start time through the shared
    // bucketer, in list order (the bucketer is stateful, so registration order
    // matters and mirrors the original computing sort keys over the list).
    let mut runs: Vec<Run> = runs
        .iter()
        .map(|v| {
            let started = v
                .get("run_started_at")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let started_dt = to_datetime(started).unwrap_or_else(|_| Timestamp::now());
            let defuzzed = bucketer.defuzz(started_dt);
            let mut r = to_run(v, started_dt, Vec::new());
            r.defuzzed_dt = defuzzed;
            r
        })
        .collect();

    // Sort by (defuzzed_start, sha, event, name), descending.
    runs.sort_by(|a, b| {
        let key = |r: &Run| {
            (
                r.defuzzed_dt,
                r.head_sha.clone(),
                r.event.clone(),
                r.name.clone(),
            )
        };
        key(b).cmp(&key(a))
    });

    // Group consecutive runs by (defuzzed_start, sha, event) — itertools.groupby.
    let mut events: Vec<Vec<Run>> = Vec::new();
    let mut run_names_seen: std::collections::HashSet<String> =
        ["Cancel".to_string()].into_iter().collect();

    for group in group_consecutive_by(&runs, |r| {
        (r.defuzzed_dt, r.head_sha.clone(), r.event.clone())
    }) {
        // Names of the runs in this group, excluding unparsable .yml paths.
        let these_names: std::collections::HashSet<String> = group
            .iter()
            .map(|r| r.name.clone())
            .filter(|n| !n.starts_with(".github/"))
            .collect();

        // Drop the event if none of its names are new (duplicate remotes).
        if these_names.is_subset(&run_names_seen) {
            continue;
        }

        let started_dt = group[0].started_dt;
        // Mirror the original's `(now - started_dt).days`: whole days elapsed,
        // floor division so a future start yields a negative count.
        let days_old = (Timestamp::now().as_second() - started_dt.as_second()).div_euclid(86_400);
        if days_old > 7 {
            continue;
        }

        let mut event_runs = group;
        if let Some(words) = only_words {
            event_runs.retain(|r| words.iter().any(|w| r.name.to_lowercase().contains(w)));
            if event_runs.is_empty() {
                continue;
            }
        }

        // Load jobs for each run in this event.
        for r in event_runs.iter_mut() {
            let jobs_url = format!("{}?per_page=100", r.jobs_url);
            let body = datafn(&jobs_url)?;
            let parsed: Value = serde_json::from_str(&body)
                .map_err(|e| WatchGhaError(format!("Couldn't parse jobs JSON: {e}")))?;
            let mut jobs: Vec<Job> = parsed
                .get("jobs")
                .and_then(|x| x.as_array())
                .map(|arr| arr.iter().map(to_job).collect())
                .unwrap_or_default();
            jobs.sort_by(|a, b| {
                let ka = (a.created_dt.as_millisecond(), human_key(&a.name));
                let kb = (b.created_dt.as_millisecond(), human_key(&b.name));
                ka.cmp(&kb)
            });
            r.jobs = jobs;
        }

        run_names_seen.extend(these_names);
        events.push(event_runs);
    }

    Ok(events)
}

fn to_run(v: &Value, started_dt: Timestamp, jobs: Vec<Job>) -> Run {
    Run {
        name: s(v, "name"),
        display_title: s(v, "display_title"),
        head_branch: s(v, "head_branch"),
        head_sha: s(v, "head_sha"),
        event: s(v, "event"),
        status: s(v, "status"),
        conclusion: opt_s(v, "conclusion"),
        html_url: s(v, "html_url"),
        jobs_url: s(v, "jobs_url"),
        started_dt,
        defuzzed_dt: started_dt,
        jobs,
    }
}

fn to_job(v: &Value) -> Job {
    let created_at = s(v, "created_at");
    let created_dt = to_datetime(&created_at).unwrap_or_else(|_| Timestamp::now());
    let steps = v
        .get("steps")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|st| Step {
                    name: opt_s(st, "name"),
                    status: s(st, "status"),
                    conclusion: opt_s(st, "conclusion"),
                })
                .collect()
        })
        .unwrap_or_default();
    Job {
        name: s(v, "name"),
        status: s(v, "status"),
        conclusion: opt_s(v, "conclusion"),
        created_dt,
        steps,
    }
}

/// Group consecutive runs sharing a key, mimicking `itertools.groupby`.
fn group_consecutive_by<K: PartialEq>(runs: &[Run], key: impl Fn(&Run) -> K) -> Vec<Vec<Run>> {
    let mut groups: Vec<Vec<Run>> = Vec::new();
    for run in runs {
        match groups.last_mut() {
            Some(g) if key(&g[0]) == key(run) => g.push(run.clone()),
            _ => groups.push(vec![run.clone()]),
        }
    }
    groups
}

/// Fetch and render all events. Mirrors `draw_runs`: returns the aggregated
/// `Status` and appends the rendered markup to `out`.
pub fn draw_runs(
    urls: &[String],
    datafn: DataFn<'_>,
    out: &mut String,
    only_words: Option<&[String]>,
) -> Result<Status, WatchGhaError> {
    let mut bucketer = DatetimeBucketer::new(5);
    let events = get_events(urls, datafn, only_words, &mut bucketer)?;

    let mut status = Status::default();
    for runs in &events {
        for run in runs {
            for job in &run.jobs {
                status.total += 1;
                if job.status == "completed" {
                    if let Some(c) = &job.conclusion {
                        if CONCLUSION_BAD.contains(&c.as_str()) {
                            status.num_failed += 1;
                        } else {
                            status.num_succeeded += 1;
                        }
                    } else {
                        status.num_succeeded += 1;
                    }
                }
            }
        }
    }

    let mut markup = String::new();
    let (done, succeeded) =
        crate::render::draw_events(&events, &mut markup, &mut status, &mut bucketer);
    out.push_str(&crate::render::render_plain(&markup));
    status.done = done;
    status.succeeded = succeeded;
    Ok(status)
}
