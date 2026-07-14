use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

use libghostty_vt::render::{CellIteration, CellIterator, CursorVisualStyle, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::Underline;
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
                let path = CString::new("/bin/zsh").unwrap();
                let arg0 = CString::new("zsh").unwrap();
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

    /// snapshot + iterate the grid to build the composited frame
    pub fn render(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut render = RenderState::new()?;
        let mut rows = RowIterator::new()?;
        let mut cells = CellIterator::new()?;

        let mut out = std::io::stdout().lock();
        // Hide the cursor while we redraw (so it doesn't skitter across the frame),
        // home, and reset the pen.
        write!(out, "\x1b[?25l\x1b[H\x1b[0m")?;

        let snap = render.update(&self.term)?;
        let mut row_iter = rows.update(&snap)?;
        let mut first = true;
        while let Some(row) = row_iter.next() {
            // Newline BETWEEN rows, not after the last one — a trailing \r\n on the
            // bottom line scrolls the whole terminal up and eats the top row.
            if !first {
                write!(out, "\r\n")?;
            }
            first = false;

            // Each row starts with the pen reset (we emit \x1b[0m at row end), so the
            // terminal's current pen matches this string. We only re-emit on change.
            let mut last_pen = String::from("\x1b[0m");

            let mut cell_iter = cells.update(row)?;
            while let Some(cell) = cell_iter.next() {
                // Width: a wide glyph occupies 2 columns — render the head, skip the
                // spacer that follows it, or everything shifts right.
                match cell.raw_cell()?.wide()? {
                    CellWide::SpacerTail | CellWide::SpacerHead => continue,
                    CellWide::Narrow | CellWide::Wide => {}
                }

                // Color/attributes: emit the SGR pen only when it changes cell-to-cell.
                let pen = cell_sgr(&cell)?;
                if pen != last_pen {
                    write!(out, "{pen}")?;
                    last_pen = pen;
                }

                let graphemes: String = cell.graphemes()?.into_iter().collect();
                if graphemes.is_empty() {
                    write!(out, " ")?; // blank cell — a space keeps columns aligned
                } else {
                    write!(out, "{graphemes}")?;
                }
            }
            write!(out, "\x1b[0m\x1b[K")?; // reset pen, then clear to EOL in the default bg
        }
        write!(out, "\x1b[0m\x1b[J")?; // reset, then clear any rows below the grid

        // Reflect the emulator's logical cursor onto the REAL cursor: set its shape,
        // move it to the app's cursor cell (1-based), and reveal it. If the app hid its
        // cursor (or it scrolled off-viewport), cursor_viewport() is None -> stay hidden.
        if let Some(cur) = snap.cursor_viewport()? {
            // DECSCUSR (CSI Ps SP q): odd codes blink, even are steady.
            let blink = snap.cursor_blinking()?;
            let shape = match snap.cursor_visual_style()? {
                CursorVisualStyle::Block | CursorVisualStyle::BlockHollow => {
                    if blink { 1 } else { 2 }
                }
                CursorVisualStyle::Underline => {
                    if blink { 3 } else { 4 }
                }
                CursorVisualStyle::Bar => {
                    if blink { 5 } else { 6 }
                }
                _ => 2, // non_exhaustive fallback: steady block
            };
            write!(out, "\x1b[{shape} q\x1b[{};{}H\x1b[?25h", cur.y + 1, cur.x + 1)?;
        }

        out.flush()?;
        Ok(())
    }
}

/// Build the SGR ("Select Graphic Rendition") escape for a cell: a full pen that
/// resets first (`0`) then applies this cell's attributes and truecolor fg/bg.
fn cell_sgr(cell: &CellIteration) -> Result<String, Box<dyn std::error::Error>> {
    let style = cell.style()?;
    let mut codes = String::from("0"); // reset base, so each pen is self-contained

    if style.bold {
        codes.push_str(";1");
    }
    if style.faint {
        codes.push_str(";2");
    }
    if style.italic {
        codes.push_str(";3");
    }
    if !matches!(style.underline, Underline::None) {
        codes.push_str(";4");
    }
    if style.inverse {
        codes.push_str(";7"); // let the outer terminal do the fg/bg swap
    }
    if style.strikethrough {
        codes.push_str(";9");
    }

    // None == "use the terminal default", so we simply omit the color code.
    if let Some(c) = cell.fg_color()? {
        codes.push_str(&format!(";38;2;{};{};{}", c.r, c.g, c.b));
    }
    if let Some(c) = cell.bg_color()? {
        codes.push_str(&format!(";48;2;{};{};{}", c.r, c.g, c.b));
    }

    Ok(format!("\x1b[{codes}m"))
}
