use std::{
    collections::{HashMap, HashSet},
    os::fd::BorrowedFd,
};

use nix::pty::Winsize;

use crate::geometry::PaneRect;
use crate::pane_handle::{PaneCursor, PaneHandle};

#[derive(Clone, Copy)]
pub enum SplitDirection {
    SplitHorizontal,
    SplitVertical,
}

#[derive(Clone, Copy)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

type PaneId = usize;

struct Border {
    x: u16,
    y: u16,
    len: u16,
    vertical: bool,
}

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
    fn layout(
        &self,
        rect: PaneRect,
        out_panes: &mut Vec<(PaneId, PaneRect)>,
        out_borders: &mut Vec<Border>,
    ) {
        match self {
            PaneTreeNode::Leaf(pane_id) => out_panes.push((*pane_id, rect)),
            PaneTreeNode::Split { dir, children } => {
                let n = children.len() as u16;
                if n == 0 {
                    return;
                }
                match dir {
                    // Stacked: divide the rows, advance y down the screen.
                    SplitDirection::SplitHorizontal => {
                        // Reserve one row per interior divider, split the rest evenly.
                        let base = rect.rows.saturating_sub(n - 1) / n;
                        let mut y = rect.y;
                        for (i, child) in children.iter().enumerate() {
                            let last = i as u16 == n - 1;
                            // Last child absorbs the remainder so the area fills exactly.
                            let rows = if last { rect.y + rect.rows - y } else { base };
                            child.layout(
                                PaneRect { x: rect.x, y, cols: rect.cols, rows },
                                out_panes,
                                out_borders,
                            );
                            y += rows;
                            if !last {
                                out_borders.push(Border {
                                    x: rect.x,
                                    y,
                                    len: rect.cols,
                                    vertical: false,
                                });
                                y += 1; // skip the divider row
                            }
                        }
                    }
                    // Side by side: divide the columns, advance x across the screen.
                    SplitDirection::SplitVertical => {
                        // Reserve one column per interior divider, split the rest evenly.
                        let base = rect.cols.saturating_sub(n - 1) / n;
                        let mut x = rect.x;
                        for (i, child) in children.iter().enumerate() {
                            let last = i as u16 == n - 1;
                            let cols = if last { rect.x + rect.cols - x } else { base };
                            child.layout(
                                PaneRect { x, y: rect.y, cols, rows: rect.rows },
                                out_panes,
                                out_borders,
                            );
                            x += cols;
                            if !last {
                                out_borders.push(Border {
                                    x,
                                    y: rect.y,
                                    len: rect.rows,
                                    vertical: true,
                                });
                                x += 1; // skip the divider column
                            }
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
    // Divider segments from the last layout pass, painted every render.
    borders: Vec<Border>,
    // Each pane's screen rectangle from the last layout — used for spatial focus nav.
    pane_rects: HashMap<PaneId, PaneRect>,
}

impl WindowHandle {
    pub fn new(rect: PaneRect) -> Result<Self, Box<dyn std::error::Error>> {
        let init_pane = PaneHandle::new(rect)?;
        Ok(Self {
            panes: HashMap::from([(0, init_pane)]),
            pane_tree_root: PaneTreeNode::Leaf(0),
            pane_id_counter: 1,
            focused_pane_id: 0,
            current_rect: rect,
            borders: Vec::new(),
            pane_rects: HashMap::from([(0, rect)]),
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
        let mut out_panes = Vec::new();
        let mut out_borders = Vec::new();
        self.pane_tree_root
            .layout(self.current_rect, &mut out_panes, &mut out_borders);
        // Stash the pane rects (focus nav) and dividers (render) — layout isn't recomputed elsewhere.
        self.pane_rects = out_panes.iter().copied().collect();
        self.borders = out_borders;
        for (pane_id, rect) in out_panes {
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                pane.resize(rect)?;
            }
        }
        Ok(())
    }

    /// Render every pane into its screen rect, composite them, and place the single real
    /// cursor at the focused pane's cursor.
    pub fn render(&mut self) -> Result<(String, Option<PaneCursor>), Box<dyn std::error::Error>> {
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

        // Rasterize every divider segment into a set of border cells, so we can pick
        // each cell's glyph from which neighbors are also borders (junctions fall out).
        let mut cells: HashSet<(u16, u16)> = HashSet::new();
        for b in &self.borders {
            for i in 0..b.len {
                let cell = if b.vertical {
                    (b.x, b.y + i)
                } else {
                    (b.x + i, b.y)
                };
                cells.insert(cell);
            }
        }

        // Paint each border cell once, choosing the box-drawing glyph from its arms.
        use std::fmt::Write as _;
        frame.push_str("\x1b[0m"); // borders in the default pen
        for &(x, y) in &cells {
            let up = y > 0 && cells.contains(&(x, y - 1));
            let down = cells.contains(&(x, y + 1));
            let left = x > 0 && cells.contains(&(x - 1, y));
            let right = cells.contains(&(x + 1, y));
            write!(
                frame,
                "\x1b[{};{}H{}",
                y + 1,
                x + 1,
                box_glyph(up, down, left, right)
            )?;
        }

        Ok((frame, focused_cursor))
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
        // If we're closing the focused pane, pick its spatial neighbor NOW — while its
        // rect is still in pane_rects — trying each direction in turn.
        let new_focus = if self.focused_pane_id == pane {
            self.neighbor(FocusDirection::Left)
                .or_else(|| self.neighbor(FocusDirection::Right))
                .or_else(|| self.neighbor(FocusDirection::Up))
                .or_else(|| self.neighbor(FocusDirection::Down))
        } else {
            None
        };

        self.panes.remove(&pane);
        self.pane_tree_root.remove_pane(pane);

        if self.focused_pane_id == pane {
            // Prefer the spatial neighbor; fall back to any survivor.
            self.focused_pane_id = new_focus
                .or_else(|| self.panes.keys().next().copied())
                .unwrap_or(0);
        }

        // Survivors grow into the freed space and repaint over the stale region.
        if !self.panes.is_empty() {
            self.relayout()?;
        }
        Ok(self.panes.len())
    }

    /// Move focus to the pane adjacent to the focused one in `dir` (if any).
    pub fn focus_pane(&mut self, dir: FocusDirection) {
        if let Some(id) = self.neighbor(dir) {
            self.focused_pane_id = id;
        }
    }

    /// Spatially find the nearest pane on the `dir` side of the focused pane that also
    /// overlaps it on the perpendicular axis. Nearest edge wins; ties break on overlap.
    fn neighbor(&self, dir: FocusDirection) -> Option<PaneId> {
        let f = *self.pane_rects.get(&self.focused_pane_id)?;
        let mut best: Option<(PaneId, u16, u16)> = None; // (id, distance, overlap)

        for (&id, &p) in &self.pane_rects {
            if id == self.focused_pane_id {
                continue;
            }
            let (on_side, distance, overlap) = match dir {
                FocusDirection::Left => (
                    p.x + p.cols <= f.x,
                    f.x.saturating_sub(p.x + p.cols),
                    overlap_1d(f.y, f.rows, p.y, p.rows),
                ),
                FocusDirection::Right => (
                    p.x >= f.x + f.cols,
                    p.x.saturating_sub(f.x + f.cols),
                    overlap_1d(f.y, f.rows, p.y, p.rows),
                ),
                FocusDirection::Up => (
                    p.y + p.rows <= f.y,
                    f.y.saturating_sub(p.y + p.rows),
                    overlap_1d(f.x, f.cols, p.x, p.cols),
                ),
                FocusDirection::Down => (
                    p.y >= f.y + f.rows,
                    p.y.saturating_sub(f.y + f.rows),
                    overlap_1d(f.x, f.cols, p.x, p.cols),
                ),
            };
            if on_side && overlap > 0 {
                let better = match best {
                    None => true,
                    Some((_, bd, bo)) => distance < bd || (distance == bd && overlap > bo),
                };
                if better {
                    best = Some((id, distance, overlap));
                }
            }
        }
        best.map(|(id, _, _)| id)
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
}

/// Length of overlap between two 1D ranges [a, a+alen) and [b, b+blen).
fn overlap_1d(a: u16, alen: u16, b: u16, blen: u16) -> u16 {
    let start = a.max(b);
    let end = (a + alen).min(b + blen);
    end.saturating_sub(start)
}

/// Pick the box-drawing glyph for a border cell from which of its 4 neighbors are also
/// borders. A lone vertical/horizontal arm falls back to the straight line.
fn box_glyph(up: bool, down: bool, left: bool, right: bool) -> char {
    match (up, down, left, right) {
        (true, true, true, true) => '┼',
        (true, true, true, false) => '┤',
        (true, true, false, true) => '├',
        (true, true, false, false) => '│',
        (true, false, true, true) => '┴',
        (false, true, true, true) => '┬',
        (true, false, true, false) => '┘',
        (true, false, false, true) => '└',
        (false, true, true, false) => '┐',
        (false, true, false, true) => '┌',
        (false, false, true, true) => '─',
        // Stubs (segment ends): render as the straight line they belong to.
        (true, false, false, false) | (false, true, false, false) => '│',
        (false, false, true, false) | (false, false, false, true) => '─',
        (false, false, false, false) => ' ',
    }
}
