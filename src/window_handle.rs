use std::{collections::HashMap, io::Write, os::fd::BorrowedFd};

use nix::pty::Winsize;

use crate::geometry::PaneRect;
use crate::pane_handle::{PaneCursor, PaneHandle};

#[derive(Clone, Copy)]
pub enum SplitDirection {
    SplitHorizontal,
    SplitVertical,
}

type PaneId = usize;

enum PaneTreeNode {
    Leaf(PaneId),
    Split {
        dir: SplitDirection,
        children: Vec<PaneTreeNode>,
    },
}

impl PaneTreeNode {
    /// Recursively assign each leaf pane a rectangle within `rect`.
    /// Splits divide the space evenly, accumulating x/y offsets so children sit side by
    /// side (or stacked), and the last child absorbs the division remainder so the area
    /// fills exactly.
    fn layout(&self, rect: PaneRect, out: &mut Vec<(PaneId, PaneRect)>) {
        match self {
            PaneTreeNode::Leaf(pane_id) => out.push((*pane_id, rect)),
            PaneTreeNode::Split { dir, children } => {
                let n = children.len() as u16;
                if n == 0 {
                    return;
                }
                match dir {
                    // Stacked: divide the rows, advance y down the screen.
                    SplitDirection::SplitHorizontal => {
                        let base = rect.rows / n;
                        let mut y = rect.y;
                        for (i, child) in children.iter().enumerate() {
                            let last = i as u16 == n - 1;
                            let rows = if last { rect.y + rect.rows - y } else { base };
                            child.layout(
                                PaneRect {
                                    x: rect.x,
                                    y,
                                    cols: rect.cols,
                                    rows,
                                },
                                out,
                            );
                            y += rows;
                        }
                    }
                    // Side by side: divide the columns, advance x across the screen.
                    SplitDirection::SplitVertical => {
                        let base = rect.cols / n;
                        let mut x = rect.x;
                        for (i, child) in children.iter().enumerate() {
                            let last = i as u16 == n - 1;
                            let cols = if last { rect.x + rect.cols - x } else { base };
                            child.layout(
                                PaneRect {
                                    x,
                                    y: rect.y,
                                    cols,
                                    rows: rect.rows,
                                },
                                out,
                            );
                            x += cols;
                        }
                    }
                }
            }
        }
    }

    /// Recursively find the leaf with `pane_id` and replace it with a split of the old
    /// pane plus a new one.
    fn split_node_with_pane_id(
        &mut self,
        pane_id: PaneId,
        new_pane_id: PaneId,
        dir: SplitDirection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            PaneTreeNode::Leaf(id) => {
                if *id == pane_id {
                    *self = PaneTreeNode::Split {
                        dir,
                        children: vec![PaneTreeNode::Leaf(*id), PaneTreeNode::Leaf(new_pane_id)],
                    };
                    Ok(())
                } else {
                    Err("Pane ID not found in tree".into())
                }
            }
            PaneTreeNode::Split { dir: _, children } => {
                for child in children {
                    if child
                        .split_node_with_pane_id(pane_id, new_pane_id, dir)
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                Err("Pane ID not found in tree".into())
            }
        }
    }

    /// Remove the leaf holding `pane_id` from this subtree, collapsing any split that's
    /// left with a single child into that child. Returns `true` if *this* node itself is
    /// the target leaf (so the caller removes it from its own children).
    fn remove_pane(&mut self, pane_id: PaneId) -> bool {
        match self {
            PaneTreeNode::Leaf(id) => *id == pane_id,
            PaneTreeNode::Split { children, .. } => {
                // Recurse; a child that IS the target leaf reports true so we drop it.
                let mut remove_idx = None;
                for (i, child) in children.iter_mut().enumerate() {
                    if child.remove_pane(pane_id) {
                        remove_idx = Some(i);
                        break;
                    }
                }
                if let Some(i) = remove_idx {
                    children.remove(i);
                }
                // Collapse: a split with one child becomes that child (pulls the subtree up).
                if children.len() == 1 {
                    *self = children.remove(0);
                }
                false
            }
        }
    }
}

pub struct WindowHandle {
    panes: HashMap<PaneId, PaneHandle>,
    pane_tree_root: PaneTreeNode,
    pane_id_counter: PaneId,
    focused_pane_id: PaneId,
    // The whole window's rectangle — the root area the tree is laid out within.
    current_rect: PaneRect,
}

impl WindowHandle {
    pub fn new(ws: Winsize) -> Result<Self, Box<dyn std::error::Error>> {
        let rect = PaneRect::from_winsize(&ws);
        let init_pane = PaneHandle::new(rect)?;
        Ok(Self {
            panes: HashMap::from([(0, init_pane)]),
            pane_tree_root: PaneTreeNode::Leaf(0),
            pane_id_counter: 1,
            focused_pane_id: 0,
            current_rect: rect,
        })
    }

    /// The fds that should be collected by the mux, each tagged with the pane index that owns it.
    pub fn pane_fds(&self) -> impl Iterator<Item = (usize, BorrowedFd<'_>)> {
        self.panes.iter().map(|(id, pane)| (*id, pane.as_fd()))
    }

    /// Keyboard bytes -> the focused pane.
    pub fn handle_input(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.panes
            .get_mut(&self.focused_pane_id)
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Focused pane not found",
            ))?
            .write_input(bytes)
    }

    /// Drain the output of a pane.
    pub fn pump(&mut self, pane: usize) -> std::io::Result<bool> {
        self.panes
            .get_mut(&pane)
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Pane not found",
            ))?
            .pump()
    }

    /// Resize the window to the new terminal size, re-laying out all panes.
    pub fn resize(&mut self, ws: Winsize) -> Result<(), Box<dyn std::error::Error>> {
        self.current_rect = PaneRect::from_winsize(&ws);
        self.relayout()
    }

    /// Recompute every pane's rectangle from the tree and apply it. Call after any
    /// change to size or tree structure (resize, split, close).
    fn relayout(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mut out = Vec::new();
        self.pane_tree_root.layout(self.current_rect, &mut out);
        for (pane_id, rect) in out {
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                pane.resize(rect)?;
            }
        }
        Ok(())
    }

    /// Render every pane into its screen rect, composite them, and place the single real
    /// cursor at the focused pane's cursor.
    pub fn render(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Build a map of pane_id -> frame for all panes
        let mut pane_frames: HashMap<PaneId, String> = HashMap::new();
        let mut focused_cursor: Option<PaneCursor> = None;
        for (pane_id, pane) in &mut self.panes {
            let rendered = pane.render()?;
            if *pane_id == self.focused_pane_id {
                focused_cursor = rendered.cursor;
            }
            pane_frames.insert(*pane_id, rendered.frame);
        }

        let mut frame = String::new();
        frame.push_str("\x1b[?25l"); // hide the cursor once while we redraw everything
        self.composite_pane_tree(&self.pane_tree_root, &pane_frames, &mut frame)?;

        // One real cursor: place + reveal it only for the focused pane.
        if let Some(c) = focused_cursor {
            use std::fmt::Write as _;
            write!(
                frame,
                "\x1b[{} q\x1b[{};{}H\x1b[?25h",
                c.shape, c.screen_y, c.screen_x
            )?;
        }

        // One write for the whole frame, instead of a syscall per line.
        let mut out = std::io::stdout().lock();
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        Ok(())
    }

    /// Recursively concatenate pane frames. Order doesn't matter — each pane's bytes are
    /// absolutely positioned at its own rect.
    fn composite_pane_tree(
        &self,
        node: &PaneTreeNode,
        pane_frames: &HashMap<PaneId, String>,
        out_frame: &mut String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match node {
            PaneTreeNode::Leaf(pane_id) => {
                if let Some(pane_frame) = pane_frames.get(pane_id) {
                    out_frame.push_str(pane_frame);
                }
            }
            PaneTreeNode::Split { dir: _, children } => {
                for child in children {
                    self.composite_pane_tree(child, pane_frames, out_frame)?;
                }
            }
        }
        Ok(())
    }

    /// Remove a pane: drop it from the arena AND the tree (collapsing its split), move
    /// focus off it if needed, and relayout so survivors reclaim the space. Returns how
    /// many panes remain.
    pub fn close_pane(&mut self, pane: usize) -> Result<usize, Box<dyn std::error::Error>> {
        self.panes.remove(&pane);
        self.pane_tree_root.remove_pane(pane);

        // If the closed pane was focused, hand focus to some survivor.
        // (Picking the "neighbor" is a refinement for when focus navigation exists.)
        if self.focused_pane_id == pane {
            if let Some(&id) = self.panes.keys().next() {
                self.focused_pane_id = id;
            }
        }

        // Survivors grow into the freed space and repaint over the stale region.
        if !self.panes.is_empty() {
            self.relayout()?;
        }
        Ok(self.panes.len())
    }

    /// Split the focused pane in the given direction, spawning a new pane.
    pub fn split_focused(&mut self, dir: SplitDirection) -> Result<(), Box<dyn std::error::Error>> {
        let new_pane_id = self.pane_id_counter;
        self.pane_id_counter += 1;

        // Born at the window size (any valid, non-zero size); relayout fixes it below.
        let new_pane = PaneHandle::new(self.current_rect)?;
        self.panes.insert(new_pane_id, new_pane);

        self.pane_tree_root
            .split_node_with_pane_id(self.focused_pane_id, new_pane_id, dir)?;
        self.focused_pane_id = new_pane_id;

        // Tree changed -> recompute all rectangles.
        self.relayout()
    }

    /// Return the window's rectangle (the root area the tree is laid out within).
    pub fn get_rect(&self) -> PaneRect {
        self.current_rect
    }
}
