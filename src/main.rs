use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;

use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use nix::pty::{ForkptyResult, Winsize, forkpty};
use nix::unistd::execvp;

const ROWS: u16 = 8;
const COLS: u16 = 24;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ws = Winsize { ws_row: ROWS, ws_col: COLS, ws_xpixel: 0, ws_ypixel: 0 };

    // forkpty = openpty + fork + (in the child) setsid + TIOCSCTTY + dup2 the
    // slave onto stdin/stdout/stderr.
    match unsafe { forkpty(&ws, None)? } {
        ForkptyResult::Child => {
            // We ARE the shell now. stdin/out/err already point at the PTY slave.
            let path = CString::new("/bin/zsh").unwrap();
            let arg0 = CString::new("zsh").unwrap();
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

    // `master` is our end of the pipe. A File gives us blocking read/write.
    let mut master = File::from(master);

    // Drive the shell with a fixed script so this first step is deterministic.
    master.write_all(b"echo hello from wisp\r\n")?;
    master.write_all(b"exit\r\n")?;

    // Read the shell's output until it exits (EOF), feeding it to the emulator.
    let mut buf = [0u8; 4096];
    loop {
        match master.read(&mut buf) {
            Ok(0) => break,                                  // shell exited
            Ok(n) => term.vt_write(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }

    compose(&term)?;   // render the final grid once
    Ok(())
}

/// snapshot + iterate the grid to build the composited frame
fn compose(term: &Terminal) -> Result<(), Box<dyn std::error::Error>> {
    let mut render = RenderState::new()?;
    let mut rows = RowIterator::new()?;
    let mut cells = CellIterator::new()?;

    let snap = render.update(term)?;
    let mut row_iter = rows.update(&snap)?;
    while let Some(row) = row_iter.next() {
        let mut cell_iter = cells.update(row)?;
        while let Some(cell) = cell_iter.next() {
            let graphemes: Vec<char> = cell.graphemes()?;
            print!("{}", graphemes.into_iter().collect::<String>());
        }
        println!();
    }

    Ok(())
}