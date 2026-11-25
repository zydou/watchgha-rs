//! Terminal screen control: the alternate-screen live region and SIGWINCH
//! redraw. Mirrors the original's `console.screen()` (rich) and its
//! `handle_resize` context manager (LOOP-3).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the SIGWINCH handler; the watch loop checks it to redraw the live
/// region at the new size (LOOP-3). Windows has no SIGWINCH and never sets it.
static RESIZED: AtomicBool = AtomicBool::new(false);

/// Enter the terminal's alternate screen buffer, clearing it (rich's
/// `Console.screen.__enter__`).
pub fn enter_alt_screen() {
    print!("\x1b[?1049h\x1b[H\x1b[2J");
    let _ = std::io::stdout().flush();
}

/// Leave the alternate screen and restore the main screen (rich's
/// `Console.screen.__exit__`).
pub fn leave_alt_screen() {
    print!("\x1b[?1049l");
    let _ = std::io::stdout().flush();
}

/// Repaint the live region: home the cursor, clear from there, and write
/// `frame` (rich's `Screen.update`).
pub fn redraw(frame: &str) {
    print!("\x1b[H\x1b[2J{frame}");
    let _ = std::io::stdout().flush();
}

/// Take and clear the resize flag: true if SIGWINCH fired since the last call.
pub fn take_resized() -> bool {
    RESIZED.swap(false, Ordering::SeqCst)
}

/// Register a SIGWINCH handler that sets [`RESIZED`]. On Windows there is no
/// SIGWINCH, so this is a no-op there — mirroring the original swallowing the
/// missing signal.
pub fn register_sigwinch() {
    #[cfg(unix)]
    unsafe {
        // SAFETY: the handler only stores to an atomic and does no allocation
        // or I/O, so it is async-signal-safe. libc accepts a `size_t` for the
        // signal number; the double cast goes through a raw pointer as clippy
        // requires.
        libc::signal(libc::SIGWINCH, handle_sigwinch as *const () as usize);
    }
}

#[cfg(unix)]
extern "C" fn handle_sigwinch(_sig: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_flag_round_trips() {
        // take_resized clears the flag, so this is order-independent even if
        // tests run in one process.
        RESIZED.store(true, Ordering::SeqCst);
        assert!(take_resized());
        assert!(!take_resized());
    }
}
