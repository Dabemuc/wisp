use std::io::Write;
use std::{collections::HashMap, os::fd::BorrowedFd};

use nix::pty::Winsize;

use crate::command_state_machine::{CommandStateMachine, WispCommand};
use crate::geometry::PaneRect;
use crate::window_handle::WindowHandle;

type WindowId = usize;

const MAX_WINDOWS: WindowId = 9;

/// Owns the windows and knows *what* they are, *which* is focused, and *where* input
/// goes. It deliberately knows nothing about `poll`, signals, or reading fds — that
/// OS-readiness plumbing lives in the reactor (main). The reactor only tells it
/// "these fds are readable" and hands it already-read keyboard bytes.
pub struct Mux {
    windows: HashMap<WindowId, WindowHandle>,
    focused_window_id: WindowId,
    command_state: CommandStateMachine,
    current_ws: Winsize, // Last known terminal size
}

impl Mux {
    pub fn new(ws: Winsize) -> Result<Self, Box<dyn std::error::Error>> {
        // Move window to row 1, leaving row 0 for the top bar.
        let init_window = WindowHandle::new(PaneRect {
            cols: ws.ws_col,
            rows: ws.ws_row - 1,
            x: 0,
            y: 1,
        })?;
        Ok(Self {
            windows: HashMap::from([(1, init_window)]),
            focused_window_id: 1,
            command_state: CommandStateMachine::new(),
            current_ws: ws,
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
    /// Also extract and handle mux commands (prefix + command byte).
    pub fn handle_input(&mut self, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        // Extract
        let (commands, remaining_bytes) = self.command_state.parse_input(bytes);

        // Handle commands
        for command in commands {
            match command {
                WispCommand::SplitFocusedWindow(dir) => {
                    let window = self.focused_window_mut()?;
                    window.split_focused(dir)?; // tree mutation + new pane, below
                    window.render()?; // geometry changed -> redraw now
                }
                WispCommand::CreateNewWindow => {
                    // tmux-style: take the smallest free id in 1..=9 (fills gaps left by
                    // closed windows). If all 9 are taken, do nothing.
                    if let Some(new_window_id) = self.next_free_window_id() {
                        let new_window = WindowHandle::new(PaneRect {
                            cols: self.current_ws.ws_col,
                            rows: self.current_ws.ws_row - 1,
                            x: 0,
                            y: 1,
                        })?;
                        self.windows.insert(new_window_id, new_window);
                        self.focused_window_id = new_window_id;
                        self.render()?; // rerender everything
                    }
                }
                WispCommand::SwitchToWindow(window_id) => {
                    if self.windows.contains_key(&window_id) {
                        self.focused_window_id = window_id;
                        self.render()? // rerender everything
                    }
                }
                WispCommand::FocusPane(dir) => {
                    self.focused_window_mut()?.focus_pane(dir);
                    self.render()?; // cursor moves to the newly focused pane
                }
            }
        }

        // Forward remaining bytes to the focused window
        self.windows
            .get_mut(&self.focused_window_id)
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Focused window not found",
            ))?
            .handle_input(remaining_bytes.as_slice())?;

        Ok(())
    }

    /// The smallest unused window id in 1..=MAX_WINDOWS, or None if all are taken.
    fn next_free_window_id(&self) -> Option<WindowId> {
        (1..=MAX_WINDOWS).find(|id| !self.windows.contains_key(id))
    }

    fn focused_window_mut(&mut self) -> std::io::Result<&mut WindowHandle> {
        self.windows
            .get_mut(&self.focused_window_id)
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Focused window not found",
            ))
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
        self.current_ws = ws;
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
        // Render focused window
        let (mut frame, focused_cursor) = self
            .windows
            .get_mut(&self.focused_window_id)
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "[Mux Rendering] Focused window not found",
            ))?
            .render()?;

        // Render top bar
        frame.push_str(&self.render_top_bar());

        // One real cursor: place + reveal it only for the focused pane.
        if let Some(c) = &focused_cursor {
            use std::fmt::Write as _;
            write!(
                frame,
                "\x1b[{} q\x1b[{};{}H\x1b[?25h",
                c.shape, c.screen_y, c.screen_x
            )?;
        }

        // One write for the whole frame
        let mut out = std::io::stdout().lock();
        out.write_all(frame.as_bytes())?;
        out.flush()?;

        Ok(())
    }

    /// Render the top bar with window IDs and highlight the focused window.
    fn render_top_bar(&self) -> String {
        let cols = self.current_ws.ws_col as usize;

        // Build the visible label text first, in a STABLE order (HashMap iteration
        // order is nondeterministic, which would make the tabs jump around).
        let mut ids: Vec<WindowId> = self.windows.keys().copied().collect();
        ids.sort_unstable();

        let mut labels = String::new();
        for window_id in ids {
            if window_id == self.focused_window_id {
                labels.push_str(&format!(" [{}] ", window_id));
            } else {
                labels.push_str(&format!("  {}  ", window_id));
            }
        }

        // Truncate to the width, then pad with spaces so the ENTIRE row is repainted
        // each frame — this both erases stale chars and extends the bar's background.
        let mut visible: String = labels.chars().take(cols).collect();
        for _ in visible.chars().count()..cols {
            visible.push(' ');
        }

        let mut top_bar = String::new();
        top_bar.push_str("\x1b[?25l"); // hide the cursor while we redraw everything
        top_bar.push_str("\x1b[H"); // move cursor to top-left
        top_bar.push_str("\x1b[7m"); // reverse video for the top bar
        top_bar.push_str(&visible);
        top_bar.push_str("\x1b[0m"); // reset attributes
        top_bar
    }

    /// Remove a pane whose shell has exited (also on non focused windows).
    /// Remove window if no pane remains.
    /// Return the number of windows remaining.
    pub fn close_pane(
        &mut self,
        window_id: usize,
        pane_id: usize,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        if let Some(window) = self.windows.get_mut(&window_id) {
            let remaining_panes = window.close_pane(pane_id)?;
            if remaining_panes == 0 {
                self.windows.remove(&window_id);
                if self.focused_window_id == window_id {
                    // Focus the nearest lower id; if none lower, the smallest remaining.
                    self.focused_window_id = self
                        .windows
                        .keys()
                        .copied()
                        .filter(|&id| id < window_id)
                        .max()
                        .or_else(|| self.windows.keys().copied().min())
                        .unwrap_or(0);
                }
            }

            self.render()? // rerender everything
        }

        Ok(self.windows.len())
    }
}
