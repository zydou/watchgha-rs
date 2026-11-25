//! Error types and process exit codes.
//!
//! Mirrors the original's `WatchGhaError` plus its `fatal()` helper and exit
//! code contract: 0 = all runs succeeded, 1 = a run failed or a fatal error
//! occurred, 2 = interrupted (Ctrl-C).

use std::fmt;
use std::process;

/// An error raised while fetching or rendering run data.
#[derive(Debug, Clone)]
pub struct WatchGhaError(pub String);

impl fmt::Display for WatchGhaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for WatchGhaError {}

/// Process exit codes (INV: match the original's contract).
pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;
pub const EXIT_INTERRUPTED: i32 = 2;

/// Print a message to stderr and exit with `status` (default 1).
///
/// The original routes this through a rich stderr console; we write plain text
/// plus a trailing newline (rich adds one), which is what the parity fixtures
/// capture on a non-TTY stderr.
pub fn fatal(msg: impl fmt::Display, status: i32) -> ! {
    eprintln!("{}", msg);
    process::exit(status)
}
