use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd};
use std::sync::atomic::{AtomicBool, Ordering};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::Winsize;
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

mod raw_mode_guard;
use raw_mode_guard::RawModeGuard;

mod pane_handle;
use pane_handle::PaneHandle;

const ROWS: u16 = 32;
const COLS: u16 = 80;

static RESIZED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigwinch(_: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst); // async-signal-safe: just a flag
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let action = SigAction::new(SigHandler::Handler(on_sigwinch), SaFlags::empty(), SigSet::empty());
    unsafe { sigaction(Signal::SIGWINCH, &action)? };

    let ws = query_winsize();

    let _raw = RawModeGuard::enable()?;
    let mut pane = PaneHandle::new(ws)?;
    let mut stdin = File::from(std::io::stdin().as_fd().try_clone_to_owned()?);

    let mut buf = [0u8; 4096];
    loop {
        if RESIZED.swap(false, Ordering::SeqCst) {
            let ws = query_winsize();
            pane.resize(ws)?;
            pane.render()?;
        }

        let (key_ready, shell_ready) = {
            let mut fds = [
                PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
                PollFd::new(pane.as_fd(), PollFlags::POLLIN),
            ];
            match poll(&mut fds, PollTimeout::NONE) {
                Ok(_) => {}
                Err(Errno::EINTR) => continue, // a signal (likely SIGWINCH) woke us — go handle the flag
                Err(e) => return Err(e.into()),
            }
            let readable = |f: &PollFd| {
                f.revents()
                    .unwrap_or(PollFlags::empty())
                    .intersects(PollFlags::POLLIN | PollFlags::POLLHUP)
            };
            (readable(&fds[0]), readable(&fds[1]))
        };

        if key_ready {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break;
            }
            pane.write_input(&buf[..n])?;
        }

        if shell_ready {
            if !pane.pump()? {
                break; // shell exited
            }
            pane.render()?;
        }
    }

    Ok(())
}

nix::ioctl_read_bad!(tiocgwinsz, libc::TIOCGWINSZ, Winsize);

/// Ask the real terminal (fd 0) for its current size.
fn query_winsize() -> Winsize {
    let mut ws = Winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    let ok = unsafe { tiocgwinsz(std::io::stdin().as_raw_fd(), &mut ws) }.is_ok();
    if !ok || ws.ws_col == 0 || ws.ws_row == 0 {
        ws.ws_col = COLS; // your consts become fallbacks
        ws.ws_row = ROWS;
    }
    ws
}