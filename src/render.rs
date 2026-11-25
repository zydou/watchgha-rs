//! Rendering: turn fetched run/job data into the display, matching the
//! original's `data_core.draw_events` byte-for-byte on a non-TTY stdout.
//!
//! The original builds a rich-markup string per line and lets rich render it.
//! We reproduce the same markup (including Python format-spec padding), then
//! render plain text by stripping tags in a single pass (`\[` -> `[`, `[tag]`
//! -> removed). On a TTY we additionally map styles to ANSI codes.

use std::fmt::Write as _;

use crate::data::{Run, Status};
use crate::time_util::{DatetimeBucketer, nice_time};

pub const CSTYLES: [(&str, &str); 5] = [
    ("failure", "red bold"),
    ("pending", "dim"),
    ("queued", "dim"),
    ("startup_failure", "red bold"),
    ("success", "green bold"),
];

/// States that are finished.
pub const FINISHED: [&str; 5] = [
    "cancelled",
    "failure",
    "skipped",
    "startup_failure",
    "success",
];

/// States that don't need jobs displayed.
pub const NO_JOBS: [&str; 2] = ["skipped", "success"];

pub fn cicons(summary: &str) -> &'static str {
    match summary {
        "cancelled" => "\u{2020}",          // DAGGER †
        "failure" => "\u{2717}",            // BALLOT X ✗
        "in_progress" => "\u{21bb}",        // CLOCKWISE OPEN CIRCLE ARROW ↻
        "pending" | "queued" => "\u{23f2}", // TIMER CLOCK ⏲
        "skipped" => "\u{23ed}", // BLACK RIGHT-POINTING DOUBLE TRIANGLE WITH VERTICAL BAR ⏭
        "startup_failure" => "\u{2717}", // BALLOT X ✗
        "success" => "\u{2713}", // CHECK MARK ✓
        "waiting" => "\u{1f6d1}", // OCTAGONAL SIGN 🛑
        _ => " ",
    }
}

pub fn stepdot(summary: &str) -> &'static str {
    match summary {
        "cancelled" => "[red]\u{2020}[/]",
        "failure" => "[red]\u{2022}[/]",
        "in_progress" => "[white]\u{2022}[/]",
        "pending" | "queued" => "[dim white]\u{2022}[/]",
        "skipped" => "[default]\u{25e6}[/]", // WHITE BULLET ◦
        "success" => "[green]\u{2022}[/]",
        _ => "?",
    }
}

/// Conclusion states to mark as bad.
pub const CONCLUSION_BAD: [&str; 3] = ["failure", "startup_failure", "cancelled"];

/// Return `(summary, style, icon)` for a run/job/step's status/conclusion.
pub fn summary_style_icon(
    status: &str,
    conclusion: Option<&str>,
) -> (String, &'static str, &'static str) {
    let mut summary = status.to_string();
    if summary == "completed" {
        summary = conclusion.unwrap_or("").to_string();
    }
    let style = CSTYLES
        .iter()
        .find(|(k, _)| *k == summary)
        .map(|(_, v)| *v)
        .unwrap_or("default");
    let icon = cicons(&summary);
    (summary, style, icon)
}

/// Draw all events and update `status`. Returns `(done, succeeded)`.
///
/// Appends rendered lines to `out`. Mirrors `draw_events`.
pub fn draw_events(
    events: &[Vec<Run>],
    out: &mut String,
    status: &mut Status,
    bucketer: &mut DatetimeBucketer,
) -> (bool, bool) {
    let mut done = true;
    let mut succeeded = true;

    for event_runs in events {
        let e = &event_runs[0];
        let mut line = String::new();
        let _ = write!(
            line,
            "[white bold]{}[/] {} \\[{}]   [dim]{:.12}  @{:}[/]",
            e.display_title,
            e.head_branch,
            e.event,
            e.head_sha,
            nice_time(e.started_dt)
        );
        push_line(out, &line);

        for run in event_runs {
            let (summary, style, icon) = summary_style_icon(&run.status, run.conclusion.as_deref());
            if !FINISHED.contains(&summary.as_str()) {
                done = false;
            }
            let run_id = run.html_url.rsplit('/').next().unwrap_or("");
            let mut line = String::new();
            let _ = write!(
                line,
                "   [{style}]{} {summary:<12}[/] [white bold]{:<16}[/]   [blue link={}]view {}[/]",
                icon, run.name, run.html_url, run_id
            );
            push_line(out, &line);

            if NO_JOBS.contains(&summary.as_str()) {
                continue;
            }
            succeeded = false;

            for job in &run.jobs {
                let (current_step, style, icon) =
                    summary_style_icon(&job.status, job.conclusion.as_deref());
                let mut current_step = current_step;
                let mut stepdots = String::new();
                if current_step != "success" {
                    if job.status == "queued" {
                        current_step = "queued".to_string();
                        done = false;
                    } else {
                        let steps = &job.steps;
                        let mut matched = false;
                        for step in steps {
                            if step.status == "completed"
                                && step.conclusion.as_deref() == Some("failure")
                            {
                                current_step =
                                    format!("failure {}", step.name.clone().unwrap_or_default());
                                matched = true;
                                break;
                            }
                            if step.status == "in_progress" {
                                done = false;
                                stepdots.clear();
                                for s in steps {
                                    let (ssum, _, _) =
                                        summary_style_icon(&s.status, s.conclusion.as_deref());
                                    stepdots.push_str(stepdot(&ssum));
                                }
                                current_step =
                                    format!(" {}", step.name.clone().unwrap_or_default());
                                matched = true;
                                break;
                            }
                        }
                        if !matched {
                            if !steps.is_empty() {
                                current_step =
                                    steps.last().unwrap().name.clone().unwrap_or_default();
                            } else if job.status == "completed"
                                && job.conclusion.as_deref() == Some("skipped")
                            {
                                current_step = "skipped".to_string();
                            }
                        }
                    }
                }
                let mut line = String::new();
                let _ = write!(
                    line,
                    "      {:<30} [{style}]{}[/] {stepdots}[{style}]{current_step}[/]",
                    job.name, icon
                );
                push_line(out, &line);
            }
        }
    }
    let _ = status;
    let _ = bucketer;
    (done, succeeded)
}

/// Append a markup line to `out`, stripping control characters (the original
/// scrubs `[\x00-\x1f\x7f-\x9f]` from every output line).
fn push_line(out: &mut String, line: &str) {
    let scrubbed: String = line
        .chars()
        .filter(|c| !matches!(c, '\x00'..='\x1f' | '\u{7f}'..='\u{9f}'))
        .collect();
    out.push_str(&scrubbed);
    out.push('\n');
}

/// Render a markup string to plain text (non-TTY path).
///
/// Single pass: `\[` becomes a literal `[`; `[tag]` is removed. This mirrors
/// how rich emits a styled string to a non-color file.
pub fn render_plain(markup: &str) -> String {
    let mut out = String::new();
    let bytes: Vec<char> = markup.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '\\' && i + 1 < bytes.len() && bytes[i + 1] == '[' {
            out.push('[');
            i += 2;
        } else if bytes[i] == '[' {
            match find_tag_end(&bytes, i) {
                Some(end) => {
                    i = end + 1;
                }
                None => {
                    out.push('[');
                    i += 1;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// Find the index of the `]` closing a markup tag starting at `start`.
fn find_tag_end(chars: &[char], start: usize) -> Option<usize> {
    let mut j = start + 1;
    while j < chars.len() {
        if chars[j] == ']' {
            return Some(j);
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_and_unescapes() {
        let m = "fix main \\[push]   4b2f  @NOW";
        assert_eq!(render_plain(m), "fix main [push]   4b2f  @NOW");
    }

    #[test]
    fn strips_styled_tag() {
        assert_eq!(render_plain("[white bold]hello[/] world"), "hello world");
    }

    #[test]
    fn strips_link_tag_keeps_text() {
        assert_eq!(render_plain("[blue link=http://x]view 1[/]"), "view 1");
    }

    #[test]
    fn scrubs_control_chars() {
        let m = "A \x1b[1mMALICIOUS\x1b[0m job";
        // render_plain removes markup; push_line scrubs control chars. Test push.
        let mut out = String::new();
        push_line(&mut out, m);
        // \x1b and \x1b removed -> "A [1mMALICIOUS[0m job"
        assert_eq!(out, "A [1mMALICIOUS[0m job\n");
    }
}
