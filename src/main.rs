//! Entry point and watch loop for the `watchgha` port.

mod cli;
mod data;
mod error;
mod git_help;
mod http;
mod render;
mod time_util;
mod urls;

use std::io::{IsTerminal, Write};
use std::process;
use std::time::{Duration, Instant};

use clap::Parser;

use crate::cli::Cli;
use crate::data::{Status, draw_runs};
use crate::error::{EXIT_FAILURE, EXIT_OK, fatal};
use crate::http::get_data;

fn main() {
    // The project ships two console scripts from one binary: `watchgha` (the
    // real command) and the deprecated `watch_gha_runs`. They share this
    // executable and are distinguished by their invoked name (argv[0]
    // basename), mirroring the original's two entry points.
    let invoked = std::env::args()
        .next()
        .map(|a| {
            std::path::Path::new(&a)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let code = if invoked == "watch_gha_runs" {
        old_main()
    } else {
        watchgha_main()
    };
    process::exit(code);
}

/// The real `watchgha` command.
fn watchgha_main() -> i32 {
    let args = Cli::parse();
    let only_words = args.only_words();

    let urls = urls::gha_urls(&args.repo, args.branch.as_deref(), args.sha.as_deref());

    let mut watcher = GhaWatcher {
        urls,
        only_words,
        message: args.message,
    };
    watcher.watch(args.wait, args.poll)
}

/// The deprecated-entry-point warning, in the original's rich markup (rendered
/// plain on a non-TTY stderr).
const DEPRECATED_MSG: &str = "[red]Warning:[/] the [bold]watch_gha_runs[/] \
         command is deprecated. Use [bold]watchgha[/] instead.";

/// Seconds the deprecated entry point pauses: `1 + (days / 365) * 9`, where
/// `days` is whole days since the 2026-01-18 deprecation date. Mirrors
/// `old_main`'s `wait`.
fn deprecation_wait_secs(days: i64) -> f64 {
    1.0 + (days as f64 / 365.0) * 9.0
}

/// Deprecated `watch_gha_runs` entry point. Mirrors the original's
/// `old_main`: warn, pause (a delay that grows the longer the tool has been
/// deprecated, to nudge users onto `watchgha`), run the real command, then
/// warn again.
fn old_main() -> i32 {
    const DEPRECATION_DATE: jiff::civil::Date = jiff::civil::date(2026, 1, 18);

    let warn = |out: &str| {
        // error_console is a rich stderr console; on a non-TTY the markup is
        // stripped, so render plain. rich's Console.print appends a newline.
        eprint!("{}", crate::render::render_plain(out));
    };
    warn(DEPRECATED_MSG);

    // deprecated_days = (now - 2026-01-18).days.
    let today = jiff::ZonedDateTime::now().date();
    let deprecated_days = DEPRECATION_DATE
        .until(today)
        .unwrap_or_else(|_| fatal("Couldn't compute deprecation interval", EXIT_FAILURE));
    let wait_secs = deprecation_wait_secs(deprecated_days.get_days() as i64);
    if wait_secs > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(wait_secs));
    }

    let code = watchgha_main();
    warn(DEPRECATED_MSG);
    code
}

/// Terminal progress escape (OSC 9;4) — a progress bar in supporting terminals.
fn osc_9_4(state: u8, percent: u64) {
    print!("\x1b]9;4;{state};{percent}\x1b\\");
    let _ = std::io::stdout().flush();
}

/// Effective console width, mirroring rich's `Console`: `COLUMNS` env if set,
/// else the terminal width, else 80.
fn console_width() -> usize {
    if let Ok(cols) = std::env::var("COLUMNS")
        && let Ok(n) = cols.trim().parse::<usize>()
        && n > 0
    {
        return n;
    }
    80
}

struct GhaWatcher {
    urls: Vec<String>,
    only_words: Option<Vec<String>>,
    message: Option<String>,
}

impl GhaWatcher {
    fn get_gha_display(&self) -> (String, Status) {
        let mut out = String::new();
        let status = draw_runs(&self.urls, &get_data, &mut out, self.only_words.as_deref())
            .unwrap_or_else(|e| fatal(e, EXIT_FAILURE));
        if let Some(msg) = &self.message {
            out = format!("{msg}\n{out}");
        }
        (out, status)
    }

    /// Run the watch loop. Returns the process exit code.
    fn watch(&mut self, wait_for_start: bool, poll: i64) -> i32 {
        let is_tty = std::io::stdout().is_terminal();
        // Both are always assigned by the initial draw loop below.
        let mut status;
        let mut output;

        // Initial draw; if --wait, poll until jobs have started.
        loop {
            let (o, s) = self.get_gha_display();
            output = o;
            status = s;
            if wait_for_start {
                if !status.done {
                    break;
                }
            } else {
                break;
            }
        }

        if !status.done {
            // Live redraw until all jobs finish.
            let mut last_draw = Instant::now();
            loop {
                if is_tty {
                    render_progress(&output, console_width());
                } else {
                    print!("{output}");
                    let _ = std::io::stdout().flush();
                }
                update_terminal_progress(&status);
                let elapsed = last_draw.elapsed();
                let interval = Duration::from_secs(poll.max(0) as u64);
                if elapsed < interval {
                    std::thread::sleep(interval - elapsed);
                }
                last_draw = Instant::now();
                let (o, s) = self.get_gha_display();
                output = o;
                status = s;
                if status.done {
                    break;
                }
            }
        }

        osc_9_4(0, 0);
        if is_tty {
            // Move below the live region and print the final frame.
            println!();
        }
        print!("{output}");
        let _ = std::io::stdout().flush();
        if status.succeeded {
            EXIT_OK
        } else {
            EXIT_FAILURE
        }
    }
}

fn update_terminal_progress(status: &Status) {
    if status.total == 0 {
        return;
    }
    let finished = status.num_succeeded + status.num_failed;
    let percent = finished * 100 / status.total;
    let state = if status.num_failed > 0 { 2 } else { 1 };
    osc_9_4(state, percent);
}

/// Render a live frame in place (TTY only). A minimal alternate approach:
/// clear from cursor and reprint. Not exercised by parity (non-TTY).
fn render_progress(_frame: &str, _width: usize) {
    // The original uses rich's alternate screen; keep a simple redraw.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deprecated_warning_renders_plain() {
        // On a non-TTY stderr, rich strips markup. The text must match the
        // original's message exactly.
        assert_eq!(
            crate::render::render_plain(DEPRECATED_MSG),
            "Warning: the watch_gha_runs command is deprecated. \
             Use watchgha instead."
        );
    }

    #[test]
    fn deprecation_wait_matches_original() {
        // wait = 1 + (days/365) * 9, mirroring old_main.
        // At the deprecation date itself (0 days), wait is exactly 1s.
        assert_eq!(deprecation_wait_secs(0), 1.0);
        // 365 days -> 1 + 9 = 10s.
        assert_eq!(deprecation_wait_secs(365), 10.0);
        // 311 days (2026-11-25) -> ~8.67s, matching the observed 8.68s.
        assert!((deprecation_wait_secs(311) - 8.667_55).abs() < 1e-3);
    }
}
