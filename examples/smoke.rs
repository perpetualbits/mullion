use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent};

use tile_engine::{
    backend::CrosstermBackend,
    poll_event,
    style::{Color, Modifier, Style},
    Terminal,
};

fn main() -> io::Result<()> {
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;
    term.enter()?;

    // Ensure we always clean up, even on panic.
    let result = run(&mut term);

    term.leave()?;
    result
}

fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    loop {
        term.draw(|buf| {
            let area = buf.area;
            let title_style = Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD);
            let normal = Style::default();

            buf.set_string(0, 0, "tile-engine smoke test — press q to quit", title_style);
            buf.set_string(0, 1, "Wide glyphs: 世界 🌍 (4 cols + 2 cols)", normal);
            buf.set_string(0, 2, "Combining:   e\u{0301} (one cell)", normal);
            buf.set_string(
                0,
                3,
                &format!("Terminal: {}×{}", area.width, area.height),
                normal,
            );
        })?;

        match poll_event(Duration::from_millis(100))? {
            Some(Event::Key(KeyEvent { code: KeyCode::Char('q'), .. })) => break,
            Some(Event::Resize(_, _)) => {
                // check_resize() is called inside draw(); just loop.
            }
            _ => {}
        }
    }
    Ok(())
}
