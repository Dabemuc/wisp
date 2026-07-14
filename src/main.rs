use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::atomic::{AtomicBool, Ordering};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::Winsize;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

mod mux;
mod pane_handle;
mod raw_mode_guard;
mod window_handle;

use mux::Mux;
use raw_mode_guard::RawModeGuard;

const ROWS: u16 = 32;
const COLS: u16 = 80;

static RESIZED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigwinch(_: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst); // async-signal-safe: just a flag
}

nix::ioctl_read_bad!(tiocgwinsz, libc::TIOCGWINSZ, Winsize);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- reactor setup: our real terminal + OS event sources ---
    let action = SigAction::new(
        SigHandler::Handler(on_sigwinch),
        SaFlags::empty(),
        SigSet::empty(),
    );
    unsafe { sigaction(Signal::SIGWINCH, &action)? };

    // `_raw` lives until main returns, so raw mode is restored on exit.
    let _raw = RawModeGuard::enable()?;
    let mut stdin = File::from(std::io::stdin().as_fd().try_clone_to_owned()?);

    let mut mux = Mux::new(query_winsize())?;

    let mut buf = [0u8; 4096];
    loop {
        // resize signal -> re-measure -> tell the mux
        if RESIZED.swap(false, Ordering::SeqCst) {
            mux.resize(query_winsize())?;
            mux.render()?;
        }

        // --- reactor: who has data? (poll only, no reading) ---
        let (stdin_ready, ready_panes) = {
            let pane_fds: Vec<(usize, usize, BorrowedFd)> = mux.pane_fds().collect();

            let mut fds = Vec::with_capacity(pane_fds.len() + 1);
            fds.push(PollFd::new(stdin.as_fd(), PollFlags::POLLIN));
            for (_, _, fd) in &pane_fds {
                fds.push(PollFd::new(fd.as_fd(), PollFlags::POLLIN));
            }

            match poll(&mut fds, PollTimeout::NONE) {
                Ok(_) => {}
                Err(Errno::EINTR) => continue, // signal (e.g. SIGWINCH) — loop to handle the flag
                Err(e) => return Err(e.into()),
            }

            let readable = |f: &PollFd| {
                f.revents()
                    .unwrap_or(PollFlags::empty())
                    .intersects(PollFlags::POLLIN | PollFlags::POLLHUP)
            };
            let stdin_ready = readable(&fds[0]);
            let ready_panes: Vec<(usize, usize)> = pane_fds
                .iter()
                .enumerate()
                .filter(|(slot, _)| readable(&fds[slot + 1]))
                .map(|(_, (window_id, pane_id, _))| (*window_id, *pane_id))
                .collect();
            (stdin_ready, ready_panes)
        }; // pane_fds/fds dropped here -> mux is free for &mut again

        // --- keyboard: reactor reads the bytes, mux decides where they go ---
        if stdin_ready {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break;
            }
            mux.handle_input(&buf[..n])?;
        }

        // --- pane output: mux pumps each pane the reactor flagged readable ---
        let had_output = !ready_panes.is_empty();
        let mut exited = Vec::new();
        for &(window_id, pane_id) in &ready_panes {
            if !mux.pump(window_id, pane_id)? {
                exited.push((window_id, pane_id));
            }
        }
        // Remove exited panes high-index-first so lower indices stay valid.
        for (window_id, pane_id) in exited.into_iter().rev() {
            if mux.close_pane(window_id, pane_id) == 0 {
                return Ok(()); // last pane's shell exited -> quit
            }
        }
        if had_output {
            mux.render()?;
        }
    }

    Ok(())
}

/// Ask the real terminal (fd 0) for its current size.
fn query_winsize() -> Winsize {
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ok = unsafe { tiocgwinsz(std::io::stdin().as_raw_fd(), &mut ws) }.is_ok();
    if !ok || ws.ws_col == 0 || ws.ws_row == 0 {
        ws.ws_col = COLS; // fallback when stdin isn't a tty
        ws.ws_row = ROWS;
    }
    ws
}
