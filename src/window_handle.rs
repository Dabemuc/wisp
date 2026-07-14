use std::{collections::HashMap, io::Write, os::fd::BorrowedFd};

use nix::pty::Winsize;

use crate::pane_handle::PaneHandle;

#[derive(Clone, Copy)]
pub enum SplitDirection {
    SPLIT_HORIZONTAL,
    SPLIT_VERTICAL,
    SPLIT_NONE,
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
    /// Recursively compute the layout of panes in this tree node, given the available space.
    /// The result is a list of (pane_id, pane_winsize) tuples.
    /// For now we just split the available space evenly among children, but later we might support user-resizable splits.
    fn layout(&mut self, ws: Winsize, out: &mut Vec<(PaneId, Winsize)>) {
        match self {
            // Leaf node: end recursion and return the pane's size.
            PaneTreeNode::Leaf(pane_id) => {
                out.push((pane_id.clone(), ws));
            }
            // Split node: divide the available space evenly among children and recurse.
            PaneTreeNode::Split { dir: dir, children } => {
                let children_ws;
                match dir {
                    SplitDirection::SPLIT_HORIZONTAL => {
                        let child_height = ws.ws_row / children.len() as u16;
                        children_ws = Winsize {
                            ws_row: child_height,
                            ws_col: ws.ws_col,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        };
                    }
                    SplitDirection::SPLIT_VERTICAL => {
                        let child_width = ws.ws_col / children.len() as u16;
                        children_ws = Winsize {
                            ws_row: ws.ws_row,
                            ws_col: child_width,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        };
                    }
                    SplitDirection::SPLIT_NONE => {
                        // No split, so just resize the single child to the full size.
                        children_ws = ws;
                    }
                }
                for child in children {
                    child.layout(children_ws, out);
                }
            }
        }
    }

    /// Recursively find the leaf node with the given pane_id and replace it with a split node containing the old pane and a new pane.
    fn split_node_with_pane_id(&mut self, pane_id: PaneId, new_pane_id: PaneId, dir: SplitDirection) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            PaneTreeNode::Leaf(id) => {
                if *id == pane_id {
                    // Replace this leaf with a split node containing the old pane and a new pane.
                    *self = PaneTreeNode::Split {
                        dir,
                        children: vec![
                            PaneTreeNode::Leaf(*id),
                            PaneTreeNode::Leaf(new_pane_id),
                        ],
                    };
                    Ok(())
                } else {
                    Err("Pane ID not found in tree".into())
                }
            }
            PaneTreeNode::Split { dir: _, children } => {
                for child in children {
                    if let Ok(_) = child.split_node_with_pane_id(pane_id, new_pane_id, dir.clone()) {
                        return Ok(());
                    }
                }
                Err("Pane ID not found in tree".into())
            }
        }
    }
}

pub struct WindowHandle {
    panes: HashMap<PaneId, PaneHandle>,
    pane_tree_root: PaneTreeNode,
    pane_id_counter: PaneId,
    focused_pane_id: PaneId,
    current_ws: Winsize,   // last size we were told to be
}

impl WindowHandle {
    pub fn new(ws: Winsize) -> Result<Self, Box<dyn std::error::Error>> {
        let init_pane = PaneHandle::new(ws)?;
        let pane_tree_root = PaneTreeNode::Leaf(0);
        Ok(Self {
            panes: HashMap::from([(0, init_pane)]),
            pane_tree_root,
            pane_id_counter: 1,
            focused_pane_id: 0,
            current_ws: ws,
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

    /// Resize all panes to match the new terminal size while conforming to the pane tree layout.
    pub fn resize(&mut self, ws: Winsize) -> Result<(), Box<dyn std::error::Error>> {
        let out = &mut Vec::new();
        self.pane_tree_root.layout(ws, out);
        for (pane_id, pane_ws) in out {
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                pane.resize(pane_ws.to_owned())?;
            }
        }
        Ok(())
    }

    /// Ask all panes to render their state into a frame, then composite them into a single frame according to the pane tree layout.
    pub fn render(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Build a map of pane_id -> frame for all panes
        let mut pane_frames: HashMap<PaneId, String> = HashMap::new();
        for (pane_id, pane) in &mut self.panes {
            pane_frames.insert(*pane_id, pane.render()?);
        }

        // Composite into a single frame according to the pane tree layout
        let mut frame = String::new();
        self.composite_pane_tree(&self.pane_tree_root, &pane_frames, &mut frame)?;

        // Render to stdout
        // One write for the whole frame, instead of a syscall per line.
        let mut out = std::io::stdout().lock();
        out.write_all(frame.as_bytes())?;
        out.flush()?;

        Ok(())
    }

    /// Recursively composite the frames of panes according to the pane tree layout.
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

    /// Remove a pane. Returns how many panes remain.
    pub fn close_pane(&mut self, pane: usize) -> usize {
        self.panes.remove(&pane);
        self.focused_pane_id = self.focused_pane_id.min(self.panes.len().saturating_sub(1));
        self.panes.len()
    }

    /// Split the focused PaneTreeNode in the given direction.
    pub fn split_focused(&mut self, dir: SplitDirection) -> Result<(), Box<dyn std::error::Error>> {
        let new_pane_id = self.pane_id_counter;
        self.pane_id_counter += 1;

        let new_pane = PaneHandle::new(self.current_ws)?;   // current_ws is just used to create
        self.panes.insert(new_pane_id, new_pane);

        // Update the pane tree to include the new pane
        self.pane_tree_root
            .split_node_with_pane_id(self.focused_pane_id, new_pane_id, dir)?;

        // Focus the new pane
        self.focused_pane_id = new_pane_id;

        // resize
        self.resize(self.current_ws)?;   // recompute each pane's rectangle now that the tree has been updated

        Ok(())
    }
}
