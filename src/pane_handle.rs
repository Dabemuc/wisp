use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

use libghostty_vt::render::{CellIteration, CellIterator, CursorVisualStyle, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, Underline};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use nix::pty::{ForkptyResult, Winsize, forkpty};
use nix::unistd::execvp;

nix::ioctl_write_ptr_bad!(tiocswinsz, libc::TIOCSWINSZ, Winsize);

pub struct PaneHandle {
    master: File,
    term: Terminal<'static, 'static>,
    // Reused across frames instead of re-allocated every render.
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    // Reused output buffer: the whole frame is built here, then written in one syscall.
    frame: String,
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
                let master = File::from(master);
                // Non-blocking master so `pump` can drain the whole burst in one call
                // (read until EAGAIN) instead of one chunk per poll wakeup.
                set_nonblocking(&master)?;
                Ok(Self {
                    master,
                    term,
                    render: RenderState::new()?,
                    rows: RowIterator::new()?,
                    cells: CellIterator::new()?,
                    frame: String::new(),
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

    /// shell -> emulator. Drains ALL currently-available output (until it would block)
    /// so a burst becomes one render, not one render per 4 KB. Returns `false` on EOF.
    pub fn pump(&mut self) -> std::io::Result<bool> {
        let mut buf = [0u8; 8192];
        loop {
            match self.master.read(&mut buf) {
                Ok(0) => return Ok(false), // shell exited
                Ok(n) => self.term.vt_write(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(true), // drained
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
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
    pub fn render(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        use std::fmt::Write as _;

        let snap = self.render.update(&self.term)?;

        let frame = &mut self.frame;
        frame.clear();
        // Hide the cursor while we redraw, home, and reset the pen.
        frame.push_str("\x1b[?25l\x1b[H\x1b[0m");

        let mut row_iter = self.rows.update(&snap)?;
        let mut first = true;
        while let Some(row) = row_iter.next() {
            // Newline BETWEEN rows, not after the last one — a trailing \r\n on the
            // bottom line scrolls the whole terminal up and eats the top row.
            if !first {
                frame.push_str("\r\n");
            }
            first = false;

            // Terminal pen is reset at each row start (we emit \x1b[0m at row end).
            let mut last_pen = Pen::DEFAULT;

            let mut cell_iter = self.cells.update(row)?;
            while let Some(cell) = cell_iter.next() {
                // Width: a wide glyph occupies 2 columns — render the head, skip the
                // spacer that follows it, or everything shifts right.
                match cell.raw_cell()?.wide()? {
                    CellWide::SpacerTail | CellWide::SpacerHead => continue,
                    CellWide::Narrow | CellWide::Wide => {}
                }

                // Emit the SGR pen only when it changes cell-to-cell (no allocation to compare).
                let pen = Pen::of(&cell)?;
                if pen != last_pen {
                    pen.write_sgr(frame);
                    last_pen = pen;
                }

                let graphemes = cell.graphemes()?;
                if graphemes.is_empty() {
                    frame.push(' '); // blank cell — a space keeps columns aligned
                } else {
                    frame.extend(graphemes);
                }
            }
            frame.push_str("\x1b[0m\x1b[K"); // reset pen, then clear to EOL in the default bg
        }
        frame.push_str("\x1b[0m\x1b[J"); // reset, then clear any rows below the grid

        // Reflect the emulator's logical cursor onto the REAL cursor: set its shape,
        // move it to the app's cursor cell (1-based), and reveal it. If the app hid its
        // cursor (or it scrolled off-viewport), cursor_viewport() is None -> stay hidden.
        if let Some(cur) = snap.cursor_viewport()? {
            // DECSCUSR (CSI Ps SP q): odd codes blink, even are steady.
            let blink = snap.cursor_blinking()?;
            let shape = match snap.cursor_visual_style()? {
                CursorVisualStyle::Block | CursorVisualStyle::BlockHollow => {
                    if blink {
                        1
                    } else {
                        2
                    }
                }
                CursorVisualStyle::Underline => {
                    if blink {
                        3
                    } else {
                        4
                    }
                }
                CursorVisualStyle::Bar => {
                    if blink {
                        5
                    } else {
                        6
                    }
                }
                _ => 2, // non_exhaustive fallback: steady block
            };
            write!(
                frame,
                "\x1b[{shape} q\x1b[{};{}H\x1b[?25h",
                cur.y + 1,
                cur.x + 1
            )?;
        }

        Ok(frame.clone())
    }
}

fn set_nonblocking(fd: &impl AsRawFd) -> std::io::Result<()> {
    let raw = fd.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL);
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// A cell's visual "pen": colors + attributes. Cheap to build and compare (no
/// allocation), so we only emit an SGR escape when it actually changes.
#[derive(Clone, Copy, PartialEq)]
struct Pen {
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    bold: bool,
    faint: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    strike: bool,
}

impl Pen {
    /// The reset state — matches the terminal right after `\x1b[0m`.
    const DEFAULT: Pen = Pen {
        fg: None,
        bg: None,
        bold: false,
        faint: false,
        italic: false,
        underline: false,
        inverse: false,
        strike: false,
    };

    fn of(cell: &CellIteration) -> Result<Pen, Box<dyn std::error::Error>> {
        let s = cell.style()?;
        Ok(Pen {
            fg: cell.fg_color()?,
            bg: cell.bg_color()?,
            bold: s.bold,
            faint: s.faint,
            italic: s.italic,
            underline: !matches!(s.underline, Underline::None),
            inverse: s.inverse,
            strike: s.strikethrough,
        })
    }

    /// Write a self-contained SGR: reset (`0`) then this pen's attributes + truecolor.
    fn write_sgr(&self, frame: &mut String) {
        use std::fmt::Write as _;
        frame.push_str("\x1b[0");
        if self.bold {
            frame.push_str(";1");
        }
        if self.faint {
            frame.push_str(";2");
        }
        if self.italic {
            frame.push_str(";3");
        }
        if self.underline {
            frame.push_str(";4");
        }
        if self.inverse {
            frame.push_str(";7"); // let the outer terminal do the fg/bg swap
        }
        if self.strike {
            frame.push_str(";9");
        }
        if let Some(c) = self.fg {
            let _ = write!(frame, ";38;2;{};{};{}", c.r, c.g, c.b);
        }
        if let Some(c) = self.bg {
            let _ = write!(frame, ";48;2;{};{};{}", c.r, c.g, c.b);
        }
        frame.push('m');
    }
}
