use std::{collections::HashMap, os::fd::BorrowedFd};

use nix::pty::Winsize;

use crate::window_handle::WindowHandle;

type WindowId = usize;

/// Owns the windows and knows *what* they are, *which* is focused, and *where* input
/// goes. It deliberately knows nothing about `poll`, signals, or reading fds — that
/// OS-readiness plumbing lives in the reactor (main). The reactor only tells it
/// "these fds are readable" and hands it already-read keyboard bytes.
pub struct Mux {
    windows: HashMap<WindowId, WindowHandle>,
    window_id_counter: WindowId,
    focused_window_id: WindowId,
}

impl Mux {
    pub fn new(ws: Winsize) -> Result<Self, Box<dyn std::error::Error>> {
        let init_window = WindowHandle::new(ws)?;
        Ok(Self {
            windows: HashMap::from([(0, init_window)]),
            window_id_counter: 1,
            focused_window_id: 0,
        })
    }

    /// The fds the reactor should poll, each tagged with the window and pane index that own it.
    pub fn pane_fds(&self) -> impl Iterator<Item = (usize, usize, BorrowedFd<'_>)> {
        self.windows.iter().flat_map(|(window_id, window)| {
            window
                .pane_fds()
                .map(move |(pane_id, fd)| (*window_id, pane_id, fd))
        })
    }

    /// Keyboard bytes -> the focused window.
    /// (Later: a prefix-key state machine intercepts commands here instead of forwarding everything.)
    pub fn handle_input(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.windows
            .get_mut(&self.focused_window_id)
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Focused window not found", // TODO: Fixed mux error types
            ))?
            .handle_input(bytes)
    }

    /// Drain the output of a pane the reactor flagged readable.
    /// Returns `false` if that pane's shell has exited.
    pub fn pump(&mut self, window_id: usize, pane_id: usize) -> std::io::Result<bool> {
        self.windows
            .get_mut(&window_id)
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Window not found",
            ))?
            .pump(pane_id)
    }

    /// Resize focused window to match the new terminal size.
    pub fn resize(&mut self, ws: Winsize) -> Result<(), Box<dyn std::error::Error>> {
        self.windows
            .get_mut(&self.focused_window_id)
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Focused window not found",
            ))?
            .resize(ws)
    }

    /// Ask focused window to render its panes into a composited frame, then draw that frame to the real terminal.
    pub fn render(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.windows
            .get_mut(&self.focused_window_id)
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Focused window not found",
            ))?
            .render()
    }

    /// Remove a pane whose shell has exited (also on non focused windows).
    /// Remove window if no pane remains.
    /// Return the number of windows remaining.
    pub fn close_pane(&mut self, window_id: usize, pane_id: usize) -> usize {
        if let Some(window) = self.windows.get_mut(&window_id) {
            let remaining_panes = window.close_pane(pane_id);
            if remaining_panes == 0 {
                self.windows.remove(&window_id);
                if self.focused_window_id == window_id {
                    self.focused_window_id = *self.windows.keys().next().unwrap_or(&0);
                }
            }
        }
        self.windows.len()
    }
}
