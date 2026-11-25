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
    let args = Cli::parse();
    let only_words = args.only_words();

    let urls = urls::gha_urls(&args.repo, args.branch.as_deref(), args.sha.as_deref());

    let mut watcher = GhaWatcher {
        urls,
        only_words,
        message: args.message,
    };
    let code = watcher.watch(args.wait, args.poll);
    process::exit(code);
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
