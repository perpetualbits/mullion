// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Strips — 1-row Fields that trace a path and **carry content along their length,
//! across corners**.
//!
//! Two strips, both animated:
//! - a **text marquee** running around a box's border perimeter
//!   ([`Field::perimeter`]) — the message turns each corner without a break, the
//!   strip behind "gaps that move across corners";
//! - a **bent wire** ([`Field::strip`] over an orthogonal path) carrying a flowing
//!   brightness wave via the video unit — "wires carry content", and the content
//!   rounds the wire's bends just as the marquee rounds the box's corners.
//!
//! Keys
//!   space          pause / resume
//!   q              quit

use std::f32::consts::PI;
use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{draw_box, BorderStyle, Borders, CornerStyle},
    poll_event,
    style::{Color, Modifier, Style},
    Buffer, Field, LineWeight, Rect, Terminal, BLOCK_RAMP,
};

struct State {
    frame: usize,
    paused: bool,
}

/// An axis-aligned path through `pts`, one unit step per cell (x first, then y), so
/// the run bends at each waypoint — a hand-built stand-in for a routed wire.
fn orthogonal_path(pts: &[(u16, u16)]) -> Vec<(u16, u16)> {
    let mut cells = Vec::new();
    let Some(&first) = pts.first() else { return cells };
    cells.push(first);
    for win in pts.windows(2) {
        let (mut x, mut y) = win[0];
        let (tx, ty) = win[1];
        while (x, y) != (tx, ty) {
            if x != tx {
                x = if x < tx { x + 1 } else { x - 1 };
            } else {
                y = if y < ty { y + 1 } else { y - 1 };
            }
            cells.push((x, y));
        }
    }
    cells
}

fn render(buf: &mut Buffer, st: &State) {
    let area = buf.area;
    if area.width < 56 || area.height < 22 {
        buf.set_string(0, 0, "strips — needs at least 56×22", Style::default().fg(Color::White));
        return;
    }

    // ── A text marquee running around a box border, across all four corners ──
    let bx = Rect::new(4, 2, 44, 9);
    draw_box(buf, bx, Borders::ALL, &BorderStyle {
        weight: LineWeight::Light,
        corners: CornerStyle::Rounded,
        style: Style::default().fg(Color::DarkGray),
    });
    let perim = Field::perimeter(bx);
    let msg: Vec<char> = "  ◆  MULLION STRIPS — CONTENT FLOWS AROUND CORNERS  ".chars().collect();
    let n = perim.width() as usize;
    perim.paint(buf, |col, _| {
        let ch = msg[(col as usize + st.frame) % msg.len()];
        if ch == ' ' {
            return None; // leave the underlying border showing through the gaps
        }
        let hue = (col as f32 / n as f32 * 360.0 + st.frame as f32 * 3.0) % 360.0;
        Some((ch.to_string(), Style::default().fg(Color::from_hsv(hue, 0.85, 1.0)).add_modifier(Modifier::BOLD)))
    });

    // ── A bent wire carrying a flowing brightness wave (the video unit) ──────
    let path = orthogonal_path(&[(6, 15), (6, 19), (30, 19), (30, 14), (50, 14)]);
    let wire = Field::strip(path);
    let t = st.frame as f32 * 0.18;
    // A travelling sine along the strip length; colour by position so the wave and the
    // hue both round the wire's bends.
    wire.render_ramp_xy(
        buf,
        |u, _v| (u * PI * 7.0 - t).sin() * 0.5 + 0.5,
        &BLOCK_RAMP,
        |m, u, _v| Style::default().fg(Color::from_hsv((u * 300.0 + t * 30.0) % 360.0, 0.8, 0.35 + 0.6 * m)),
    );

    // ── Labels & status ─────────────────────────────────────────────────────
    buf.set_string(6, 13, "↑ perimeter strip: a marquee turning every corner", Style::default().fg(Color::Gray));
    buf.set_string(6, 21, "↑ wire strip: a video wave flowing around the bends", Style::default().fg(Color::Gray));
    buf.set_string(0, 0, "strips — space:pause  q:quit",
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let status = format!(" frame {}   {}", st.frame, if st.paused { "paused" } else { "running" });
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
    let mut st = State { frame: 0, paused: false };
    loop {
        term.draw(|buf| render(buf, &st))?;
        match poll_event(Duration::from_millis(80))? {
            Some(Event::Key(KeyEvent { code: KeyCode::Char('q'), .. })) => break,
            Some(Event::Key(KeyEvent { code: KeyCode::Char(' '), .. })) => st.paused = !st.paused,
            _ => {}
        }
        if !st.paused {
            st.frame = st.frame.wrapping_add(1);
        }
    }
    Ok(())
}
