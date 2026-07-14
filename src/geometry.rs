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
