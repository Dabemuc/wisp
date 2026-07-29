use std::collections::HashSet;

use crate::common::dtos::CursorDataDTO;

use nix::pty::Winsize;

pub fn draw_ui(
    frame: &mut String,
    ws: Winsize,
    border_cells: HashSet<(u16, u16)>,
    window_ids: Vec<u16>,
    focused_window_id: u16,
) {
    // Draw top bar
    frame.push_str(&render_top_bar(ws, window_ids, focused_window_id));

    // Draw borders
    frame.push_str(&render_borders(border_cells));
}

pub fn draw_cursor(frame: &mut String, cursor_data: CursorDataDTO) {
    // One real cursor: place + reveal it only for the focused pane.
    frame.push_str(&format!(
        "\x1b[{} q\x1b[{};{}H\x1b[?25h",
        cursor_data.shape, cursor_data.screen_y, cursor_data.screen_x,
    ));
}

/// Render the top bar with window IDs and highlight the focused window.
fn render_top_bar(ws: Winsize, window_ids: Vec<u16>, focused_window_id: u16) -> String {
    let cols = ws.ws_col as usize;

    // Build the visible label text first, in a STABLE order (HashMap iteration
    // order is nondeterministic, which would make the tabs jump around).
    window_ids.to_owned().sort_unstable();

    let mut labels = String::new();
    for window_id in window_ids {
        if window_id == focused_window_id {
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

fn render_borders(border_cells: HashSet<(u16, u16)>) -> String {
    let mut borders = String::new();
    // Paint each border cell once, choosing the box-drawing glyph from its arms.
    borders.push_str("\x1b[0m"); // borders in the default pen.
    for &(x, y) in &border_cells {
        let up = y > 0 && border_cells.contains(&(x, y - 1));
        let down = border_cells.contains(&(x, y + 1));
        let left = x > 0 && border_cells.contains(&(x - 1, y));
        let right = border_cells.contains(&(x + 1, y));
        borders.push_str(&format!(
            "\x1b[{};{}H{}",
            y + 1,
            x + 1,
            box_glyph(up, down, left, right)
        ));
    }

    borders
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
