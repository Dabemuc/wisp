use std::fs::File;
use std::io::Read;
use std::os::fd::AsFd;

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::Winsize;

mod raw_mode_guard;
use raw_mode_guard::RawModeGuard;

mod pane_handle;
use pane_handle::PaneHandle;

const ROWS: u16 = 32;
const COLS: u16 = 80;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ws = Winsize {
        ws_row: ROWS,
        ws_col: COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let _raw = RawModeGuard::enable()?;
    let mut pane = PaneHandle::new(ws)?;
    let mut stdin = File::from(std::io::stdin().as_fd().try_clone_to_owned()?);

    let mut buf = [0u8; 4096];
    loop {
        let (key_ready, shell_ready) = {
            let mut fds = [
                PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
                PollFd::new(pane.as_fd(), PollFlags::POLLIN),
            ];
            poll(&mut fds, PollTimeout::NONE)?;
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