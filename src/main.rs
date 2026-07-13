use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};

use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{ForkptyResult, Winsize, forkpty};
use nix::sys::termios::{self, SetArg, Termios};
use nix::unistd::execvp;

const ROWS: u16 = 32;
const COLS: u16 = 80;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ws = Winsize { ws_row: ROWS, ws_col: COLS, ws_xpixel: 0, ws_ypixel: 0 };

    // forkpty = openpty + fork + (in the child) setsid + TIOCSCTTY + dup2 the
    // slave onto stdin/stdout/stderr.
    match unsafe { forkpty(&ws, None)? } {
        ForkptyResult::Child => {
            // We ARE the shell now. stdin/out/err already point at the PTY slave.
            let path = CString::new("/bin/bash").unwrap();
            let arg0 = CString::new("bash").unwrap();
            let _ = execvp(&path, &[arg0]);   // replaces this process image
            unsafe { libc::_exit(1) }         // only reached if exec failed
        }
        ForkptyResult::Parent { child: _, master } => run_parent(master),
    }
}

fn run_parent(master: OwnedFd) -> Result<(), Box<dyn std::error::Error>> {
    let mut term = Terminal::new(TerminalOptions {
        cols: COLS,
        rows: ROWS,
        max_scrollback: 10_000,
    })?;

    // Put OUR terminal into raw mode; restored automatically on any exit.
    let _raw = RawModeGuard::enable()?;

    // `master` is our end of the pipe. A File gives us blocking read/write.
    let mut master = File::from(master);

    // An UNBUFFERED handle on fd 0. We dup it and wrap in a File so poll() and
    // read() agree — a buffered stdin could hide bytes from poll(). See note.
    let mut stdin = File::from(std::io::stdin().as_fd().try_clone_to_owned()?);

    let mut buf = [0u8; 4096];
    loop {
        // Ask the kernel: which of these fds has something for me?
        let (key_ready, shell_ready) = {
            let mut fds = [
                PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
                PollFd::new(master.as_fd(), PollFlags::POLLIN),
            ];
            poll(&mut fds, PollTimeout::NONE)?; // block until at least one is ready
            let readable = |f: &PollFd| {
                f.revents()
                    .unwrap_or(PollFlags::empty())
                    .intersects(PollFlags::POLLIN | PollFlags::POLLHUP)
            };
            (readable(&fds[0]), readable(&fds[1]))
        }; // `fds` (and its borrows of stdin/master) dropped here, freeing them for read/write

        // keyboard -> shell
        if key_ready {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break;
            }
            master.write_all(&buf[..n])?;
        }

        // shell -> emulator -> screen
        if shell_ready {
            match master.read(&mut buf) {
                Ok(0) => break, // shell exited
                Ok(n) => {
                    term.vt_write(&buf[..n]);
                    compose(&term)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    Ok(())
}

/// Puts the controlling terminal into raw mode and restores it on drop.
struct RawModeGuard {
    original: Termios,
}

impl RawModeGuard {
    fn enable() -> Result<Self, Box<dyn std::error::Error>> {
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

/// snapshot + iterate the grid to build the composited frame
fn compose(term: &Terminal) -> Result<(), Box<dyn std::error::Error>> {
    let mut render = RenderState::new()?;
    let mut rows = RowIterator::new()?;
    let mut cells = CellIterator::new()?;

    let mut out = std::io::stdout().lock();
    write!(out, "\x1b[H")?; // cursor home — redraw over the previous frame

    let snap = render.update(term)?;
    let mut row_iter = rows.update(&snap)?;
    while let Some(row) = row_iter.next() {
        let mut cell_iter = cells.update(row)?;
        while let Some(cell) = cell_iter.next() {
            let graphemes: Vec<char> = cell.graphemes()?;
            write!(out, "{}", graphemes.into_iter().collect::<String>())?;
        }
        write!(out, "\x1b[K\r\n")?; // clear to end of line, then CR+LF (raw mode!)
    }
    out.flush()?;
    Ok(())
}