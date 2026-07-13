use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut term = Terminal::new(TerminalOptions {
        cols: 24,
        rows: 24,
        max_scrollback: 10_000,
    })?;

    // The main application loop
    let mut iter_count = 0;
    loop {
        // write-back for query responses (e.g. DA, kitty graphics acks) → goes to the lower PTY
        term.on_pty_write(|_term, data| {
            // forward `data` to the PTY master
            let _ = data;
        })?;

        // Feed simulated data bytes read from the lower PTY into the emulator
        let data = format!("\x1b[1;32mhello {iter_count}\x1b[0m\r\n");
        term.vt_write(data.as_bytes());

        // compose state to frame
        compose(&term)?;
        iter_count += 1;
    }
}


/// snapshot + iterate the grid to build the composited frame
fn compose(term: &Terminal) -> Result<(), Box<dyn std::error::Error>> {
    let mut render = RenderState::new()?;
    let mut rows = RowIterator::new()?;
    let mut cells = CellIterator::new()?;

    let snap = render.update(term)?;
    let mut row_iter = rows.update(&snap)?;
    while let Some(row) = row_iter.next() {
        let mut cell_iter = cells.update(row)?;
        while let Some(cell) = cell_iter.next() {
            let graphemes: Vec<char> = cell.graphemes()?;
            print!("{}", graphemes.into_iter().collect::<String>());
        }
        println!();
    }

    Ok(())
}