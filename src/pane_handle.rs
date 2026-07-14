use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use nix::pty::{ForkptyResult, Winsize, forkpty};
use nix::unistd::execvp;

nix::ioctl_write_ptr_bad!(tiocswinsz, libc::TIOCSWINSZ, Winsize);

pub struct PaneHandle {
    master: File,
    term: Terminal<'static, 'static>,
}

impl PaneHandle {
    pub fn new(ws: Winsize) -> Result<Self, Box<dyn std::error::Error>> {
        match unsafe { forkpty(&ws, None)? } {
            ForkptyResult::Child => {
                let path = CString::new("/bin/bash").unwrap();
                let arg0 = CString::new("bash").unwrap();
                let _ = execvp(&path, &[arg0]);
                unsafe { libc::_exit(1) } // `!` — coerces to Result, so the match still typechecks
            }
            ForkptyResult::Parent { child: _, master } => {
                let term = Terminal::new(TerminalOptions {
                    cols: ws.ws_col,
                    rows: ws.ws_row,
                    max_scrollback: 10_000,
                })?;
                Ok(Self {
                    master: File::from(master),
                    term,
                })
            }
        }
    }

    /// The fd the event loop should poll for shell output.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// keyboard -> shell
    pub fn write_input(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.master.write_all(data)
    }

    /// shell -> emulator. Returns `false` when the shell has exited (EOF).
    pub fn pump(&mut self) -> std::io::Result<bool> {
        let mut buf = [0u8; 4096];
        match self.master.read(&mut buf) {
            Ok(0) => Ok(false),
            Ok(n) => {
                self.term.vt_write(&buf[..n]);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(true),
            Err(e) => Err(e),
        }
    }

    pub fn resize(&mut self, ws: Winsize) -> Result<(), Box<dyn std::error::Error>> {
        // 1) tell the kernel the pty's new size -> it SIGWINCHes bash/vim/…
        unsafe { tiocswinsz(self.master.as_raw_fd(), &ws)? };
        // 2) resize the emulator's grid to match (0,0 px: we're a text renderer)
        self.term.resize(ws.ws_col, ws.ws_row, 0, 0)?;
        Ok(())
    }

    pub fn render(&self) -> Result<(), Box<dyn std::error::Error>> {
        compose(&self.term)
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
    let mut first = true;
    while let Some(row) = row_iter.next() {
        // Newline BETWEEN rows, not after the last one — a trailing \r\n on the
        // bottom line scrolls the whole terminal up and eats the top row.
        if !first {
            write!(out, "\r\n")?;
        }
        first = false;

        let mut cell_iter = cells.update(row)?;
        while let Some(cell) = cell_iter.next() {
            let graphemes: Vec<char> = cell.graphemes()?;
            write!(out, "{}", graphemes.into_iter().collect::<String>())?;
        }
        write!(out, "\x1b[K")?; // clear to end of line (no newline)
    }
    write!(out, "\x1b[J")?; // clear any leftover rows below the bottom of the grid
    out.flush()?;
    Ok(())
}
