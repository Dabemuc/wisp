use std::os::fd::AsFd;

use nix::sys::termios::{self, SetArg, Termios};

/// Puts the controlling terminal into raw mode and restores it on drop.
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
        Ok(Self { original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let fd = std::io::stdin();
        let _ = termios::tcsetattr(fd.as_fd(), SetArg::TCSANOW, &self.original);
    }
}
