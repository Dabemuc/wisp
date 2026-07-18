use std::io::Write;
use std::os::fd::AsFd;

use nix::sys::termios::{self, SetArg, Termios};

/// Puts the controlling terminal into "app mode" for the client's lifetime and restores
/// it on drop: raw mode + the alternate screen buffer.
///
/// The alternate screen (`\x1b[?1049h` / `\x1b[?1049l`) is what makes exiting clean — the
/// terminal saves the primary screen on entry and restores it verbatim on leave, so
/// nothing wisp drew (top bar, panes) is left behind. The terminal does the save/restore;
/// we just send the two escapes.
pub struct RawModeGuard {
    original: Termios,
}

impl RawModeGuard {
    pub fn enable() -> Result<Self, Box<dyn std::error::Error>> {
        let fd = std::io::stdin();
        let original = termios::tcgetattr(fd.as_fd())?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(fd.as_fd(), SetArg::TCSANOW, &raw)?;

        // Switch to the alternate screen so wisp draws on a fresh, throwaway buffer.
        let mut out = std::io::stdout();
        out.write_all(b"\x1b[?1049h")?;
        out.flush()?;

        Ok(Self { original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Reset pen + show cursor, then leave the alternate screen (restoring the primary
        // screen exactly as it was before wisp started).
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = out.flush();

        let fd = std::io::stdin();
        let _ = termios::tcsetattr(fd.as_fd(), SetArg::TCSANOW, &self.original);
    }
}
