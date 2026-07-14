use std::os::fd::BorrowedFd;

use nix::pty::Winsize;

use crate::pane_handle::PaneHandle;

/// Owns the panes and knows *what* they are, *which* is focused, and *where* input
/// goes. It deliberately knows nothing about `poll`, signals, or reading fds — that
/// OS-readiness plumbing lives in the reactor (main). The reactor only tells it
/// "these fds are readable" and hands it already-read keyboard bytes.
pub struct Mux {
    panes: Vec<PaneHandle>,
    focused: usize,
}

impl Mux {
    pub fn new(ws: Winsize) -> Result<Self, Box<dyn std::error::Error>> {
        let pane = PaneHandle::new(ws)?;
        Ok(Self {
            panes: vec![pane],
            focused: 0,
        })
    }

    /// The fds the reactor should poll, each tagged with the pane index that owns it.
    pub fn pane_fds(&self) -> impl Iterator<Item = (usize, BorrowedFd<'_>)> {
        self.panes.iter().enumerate().map(|(i, p)| (i, p.as_fd()))
    }

    /// Keyboard bytes -> the focused pane.
    /// (Later: a prefix-key state machine intercepts commands here instead of forwarding.)
    pub fn handle_input(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.panes[self.focused].write_input(bytes)
    }

    /// Drain the output of a pane the reactor flagged readable.
    /// Returns `false` if that pane's shell has exited.
    pub fn pump(&mut self, pane: usize) -> std::io::Result<bool> {
        self.panes[pane].pump()
    }

    pub fn resize(&mut self, ws: Winsize) -> Result<(), Box<dyn std::error::Error>> {
        // Single pane == full screen for now. With tiling, each pane gets a sub-rect.
        for pane in &mut self.panes {
            pane.resize(ws)?;
        }
        Ok(())
    }

    /// Compose the visible frame.
    /// (Focused pane full-screen for now; tiling multiple panes by layout is a later step.)
    pub fn render(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.panes[self.focused].render()
    }

    /// Remove a pane whose shell has exited. Returns how many panes remain.
    pub fn close_pane(&mut self, pane: usize) -> usize {
        self.panes.remove(pane);
        self.focused = self.focused.min(self.panes.len().saturating_sub(1));
        self.panes.len()
    }
}
