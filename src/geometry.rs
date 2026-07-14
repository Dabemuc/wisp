/// A pane's rectangle on the real screen: position (`x`/`y`, 0-based, from the
/// top-left) plus size (`cols`/`rows`). Position is a *wisp compositing* concept —
/// it never crosses into the pty/emulator, which only ever knows `cols`/`rows`.
#[derive(Clone, Copy, Debug)]
pub struct PaneRect {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

impl PaneRect {
    pub fn from_winsize(ws: &nix::pty::Winsize) -> Self {
        Self {
            x: 0,
            y: 0,
            cols: ws.ws_col,
            rows: ws.ws_row,
        }
    }

    pub fn to_winsize(&self) -> nix::pty::Winsize {
        nix::pty::Winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}
