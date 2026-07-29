use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct FrameDataDTO {
    pub rendered_panes: Vec<u8>,
    pub focused_cursor: Option<CursorDataDTO>,
    pub border_cells: HashSet<(u16, u16)>,
    pub window_ids: Vec<u16>,
    pub focused_window_id: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CursorDataDTO {
    pub screen_x: u16, // 1-based column on the real screen
    pub screen_y: u16, // 1-based row on the real screen
    pub shape: u8,     // DECSCUSR code
}
