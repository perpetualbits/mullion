// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Colour sources — a [`Flame`] cellular automaton and an analytic [`Wave`] driving a
//! [`Field`]'s **colour**, independently of its glyphs.
//!
//! The field's value per cell comes from the chosen source (fire heat, or a plasma
//! wave); a [`Palette`] turns it into colour. The glyph is either a brightness block
//! (so you see the source's shape) or tiled text (so the colour shines *through* the
//! letters) — the glyph and the colour are separate.
//!
//! Keys
//!   s              switch source (flame ↔ wave)
//!   p              cycle palette (fire → ice → rainbow)
//!   t              toggle glyph: brightness blocks ↔ text
//!   space          pause / resume
//!   q              quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    colorfield::{Flame, Palette, Wave},
    poll_event,
    style::{Color, Modifier, Style},
    Buffer, Field, Rect, Terminal, BLOCK_RAMP,
};

const SOURCES: [&str; 2] = ["flame (cellular automaton)", "wave (plasma)"];
const PALETTES: [Palette; 3] = [Palette::Fire, Palette::Ice, Palette::Rainbow];
const PALETTE_NAMES: [&str; 3] = ["fire", "ice", "rainbow"];
const MSG: &str = "MULLION·COLOUR·SOURCES·";

struct State {
    source: usize,
    palette: usize,
    text: bool,
    paused: bool,
    t: f32,
    flame: Flame,
    wave: Wave,
}

/// The animated area (below the help row, above the status row).
fn field_area(area: Rect) -> Rect {
    Rect::new(0, 1, area.width, area.height.saturating_sub(2))
}

fn render(buf: &mut Buffer, st: &State) {
    let area = buf.area;
    if area.height < 4 {
        return;
    }
    let field = Field::rect(field_area(area));
    let (w, h) = (field.width() as f32, field.height() as f32);
    let pal = PALETTES[st.palette];
    let msg: Vec<char> = MSG.chars().collect();

    field.paint(buf, |col, row| {
        let value = if st.source == 0 {
            st.flame.at(col, row)
        } else {
            st.wave.value((col as f32 + 0.5) / w, (row as f32 + 0.5) / h, st.t)
        };
        let glyph = if st.text {
            msg[(col as usize + row as usize * 3) % msg.len()]
        } else {
            let idx = ((value * (BLOCK_RAMP.len() - 1) as f32).round() as usize).min(BLOCK_RAMP.len() - 1);
            BLOCK_RAMP[idx]
        };
        Some((glyph.to_string(), Style::default().fg(pal.color(value))))
    });

    buf.set_string(0, 0, "colour sources — s:source  p:palette  t:glyph  space:pause  q:quit",
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let status = format!(" source: {}   palette: {}   glyph: {}   {}",
        SOURCES[st.source], PALETTE_NAMES[st.palette],
        if st.text { "text" } else { "blocks" },
        if st.paused { "paused" } else { "running" });
    let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, area.height - 1, " ", sstyle);
    }
    buf.set_string(0, area.height - 1, &status, sstyle);
}

fn main() -> io::Result<()> {
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;
    term.enter()?;
    let result = run(&mut term);
    term.leave()?;
    result
}

fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let size = mullion::backend::Backend::size(term.backend())?;
    let fa = field_area(size);
    let mut st = State {
        source: 0,
        palette: 0,
        text: false,
        paused: false,
        t: 0.0,
        flame: Flame::new(fa.width, fa.height),
        wave: Wave::plasma(),
    };
    loop {
        // Keep the flame grid sized to the field; advance the chosen source per frame.
        let size = mullion::backend::Backend::size(term.backend())?;
        let fa = field_area(size);
        if (st.flame.width(), st.flame.height()) != (fa.width, fa.height) {
            st.flame = Flame::new(fa.width, fa.height);
        }
        if !st.paused {
            if st.source == 0 {
                st.flame.step(0.18);
            }
            st.t += 0.1;
        }
        term.draw(|buf| render(buf, &st))?;
        if let Some(Event::Key(KeyEvent { code, .. })) = poll_event(Duration::from_millis(70))? {
            match code {
                KeyCode::Char('q') => break,
                KeyCode::Char('s') => st.source = (st.source + 1) % SOURCES.len(),
                KeyCode::Char('p') => st.palette = (st.palette + 1) % PALETTES.len(),
                KeyCode::Char('t') => st.text = !st.text,
                KeyCode::Char(' ') => st.paused = !st.paused,
                _ => {}
            }
        }
    }
    Ok(())
}
