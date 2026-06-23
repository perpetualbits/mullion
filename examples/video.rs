// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Field demo — the **video unit**: an image rendered into a `Field` of cells.
//!
//! An animated plasma (a sum of travelling sines) is sampled as an intensity image
//! and drawn into a `Field` three ways:
//!
//! - **braille** — 2×4 sub-pixels per cell, ordered-dithered so dot density tracks
//!   brightness (fine sub-cell detail, no hard banding);
//! - **ramp** — one glyph per cell from its brightness (`░▒▓█`);
//! - **glyphs** — structure-aware matching: flat cells get a brightness glyph, edge
//!   cells get a directional stroke (`─ │ ╱ ╲`) tracing the contour.
//!
//! The glyph carries the *shape*; a separate colour layer (value → hue) shines
//! through it — the glyph and the colour are independent.
//!
//! Keys
//!   space          cycle encoder (braille → ramp → glyphs)
//!   c              toggle colour
//!   q              quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    poll_event,
    style::{Color, Modifier, Style},
    Buffer, Field, Rect, Terminal, BLOCK_RAMP,
};

const ENCODERS: [&str; 3] = ["braille (dithered 2×4)", "ramp (density)", "glyphs (structure)"];

struct State {
    t: f32,
    encoder: usize,
    colour: bool,
}

/// A plasma intensity field, animated by `t`. `x, y` are in **cell** units so its
/// wavelength is a fixed number of cells — the per-cell gradient (and so the glyph
/// matcher's edge threshold) stays the same at any terminal size. Returns 0..1.
fn plasma(x: f32, y: f32, t: f32) -> f32 {
    let f = 0.42;
    let mut s = (x * f + t).sin() + (y * f * 1.25 + t * 0.7).sin() + ((x + y) * f * 0.6 + t * 1.3).sin();
    let (cx, cy) = (x * f - 3.0 + 2.0 * (t * 0.5).sin(), y * f - 3.0 + 2.0 * (t * 0.4).cos());
    s += ((cx * cx + cy * cy).sqrt() - t).sin();
    (s / 4.0) * 0.5 + 0.5
}

fn render(buf: &mut Buffer, st: &State) {
    let area = buf.area;
    if area.height < 3 {
        return;
    }
    let field = Field::rect(Rect::new(0, 1, area.width, area.height - 2));
    let t = st.t;
    let (fw, fh) = (field.width() as f32, field.height() as f32);
    let img = |u: f32, v: f32| plasma(u * fw, v * fh, t);
    let colour = st.colour;
    // value → hue: a moving colour band that shines through the glyphs.
    let style = move |m: f32| {
        if colour {
            let hue = (m * 140.0 + t * 25.0) % 360.0;
            Style::default().fg(Color::from_hsv(hue, 0.85, 0.45 + 0.5 * m))
        } else {
            let g = (m * 255.0) as u8;
            Style::default().fg(Color::Rgb(g, g, g))
        }
    };
    match st.encoder {
        0 => field.render_braille(buf, img, style),
        1 => field.render_ramp(buf, img, &BLOCK_RAMP, style),
        _ => field.render_glyphs(buf, img, &BLOCK_RAMP, 0.07, style),
    }

    buf.set_string(0, 0, "video — space:encoder  c:colour  q:quit",
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let status = format!(" encoder: {}   colour: {}",
        ENCODERS[st.encoder], if st.colour { "on" } else { "grey" });
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
    let mut st = State { t: 0.0, encoder: 0, colour: true };
    loop {
        term.draw(|buf| render(buf, &st))?;
        match poll_event(Duration::from_millis(33))? {
            None | Some(Event::Resize(_, _)) => st.t += 0.05,
            Some(Event::Key(KeyEvent { code, .. })) => match code {
                KeyCode::Char('q') => break,
                KeyCode::Char(' ') => st.encoder = (st.encoder + 1) % ENCODERS.len(),
                KeyCode::Char('c') => st.colour = !st.colour,
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}
