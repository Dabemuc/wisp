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

use crate::geometry::PaneRect;

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
    // Where this pane sits on the real screen. `x`/`y` are used only for rendering;
    // `cols`/`rows` are what the pty/emulator get.
    rect: PaneRect,
}

/// What a pane produces for one frame: its cells (positioned absolutely), plus where
/// its cursor is *in screen coordinates* — the window decides whether to show it.
pub struct PaneRender {
    pub frame: String,
    pub cursor: Option<PaneCursor>,
}

pub struct PaneCursor {
    pub screen_x: u16, // 1-based column on the real screen
    pub screen_y: u16, // 1-based row on the real screen
    pub shape: u8,     // DECSCUSR code
}

impl PaneHandle {
    pub fn new(rect: PaneRect) -> Result<Self, Box<dyn std::error::Error>> {
        // The pty/emulator only know size, not position — drop x/y here.
        let ws = Winsize {
            ws_row: rect.rows,
            ws_col: rect.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        match unsafe { forkpty(&ws, None)? } {
            ForkptyResult::Child => {
                let path = CString::new("/bin/zsh").unwrap();
                let arg0 = CString::new("zsh").unwrap();
                let _ = execvp(&path, &[arg0]);
                unsafe { libc::_exit(1) } // `!` — coerces to Result, so the match still typechecks
            }
            ForkptyResult::Parent { child: _, master } => {
                let term = Terminal::new(TerminalOptions {
                    cols: rect.cols,
                    rows: rect.rows,
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
                    rect,
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

    /// The single PaneRect -> Winsize conversion boundary: keep x/y for rendering,
    /// hand only cols/rows to the pty and emulator.
    pub fn resize(&mut self, rect: PaneRect) -> Result<(), Box<dyn std::error::Error>> {
        self.rect = rect;
        let ws = Winsize {
            ws_row: rect.rows,
            ws_col: rect.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // 1) tell the kernel the pty's new size -> it SIGWINCHes bash/vim/…
        unsafe { tiocswinsz(self.master.as_raw_fd(), &ws)? };
        // 2) resize the emulator's grid to match (0,0 px: we're a text renderer)
        self.term.resize(rect.cols, rect.rows, 0, 0)?;
        Ok(())
    }

    /// Render this pane's grid into a frame, positioned at the pane's screen rect.
    /// Draws ONLY its own columns (no clear-to-EOL, which would wipe a neighbor to the
    /// right) and emits no cursor commands — the window owns the single real cursor.
    pub fn render(&mut self) -> Result<PaneRender, Box<dyn std::error::Error>> {
        let rect = self.rect; // Copy — read freely without borrowing self
        let snap = self.render.update(&self.term)?;

        let frame = &mut self.frame;
        frame.clear();

        let mut row_iter = self.rows.update(&snap)?;
        let mut row: u16 = 0;
        while let Some(cells_row) = row_iter.next() {
            // Absolute-position each row at (rect.x, rect.y + row), 1-based, and reset
            // the pen — the previous row/pane may have left a non-default pen active.
            {
                use std::fmt::Write as _;
                write!(frame, "\x1b[{};{}H\x1b[0m", rect.y + row + 1, rect.x + 1)?;
            }
            row += 1;

            let mut last_pen = Pen::DEFAULT;
            let mut cell_iter = self.cells.update(cells_row)?;
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
        }

        // Cursor in SCREEN coordinates (pane-local + rect offset). The window shows it
        // only for the focused pane. None => the app hid it / it's off-viewport.
        let cursor = if let Some(cur) = snap.cursor_viewport()? {
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
            Some(PaneCursor {
                screen_x: rect.x + cur.x + 1,
                screen_y: rect.y + cur.y + 1,
                shape,
            })
        } else {
            None
        };

        Ok(PaneRender {
            frame: frame.clone(),
            cursor,
        })
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
