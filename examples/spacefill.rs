// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! **An address landscape over an allowed/forbidden grid.** Push a 1-D line of
//! addresses onto the grid with a [generalized Hilbert curve](mullion::spacefill), but
//! only over the **allowed** cells — the *forbidden* cells (holes) are skipped. The
//! line is the Gilbert order filtered to allowed cells (see
//! [`Gilbert::masked_order`](mullion::spacefill::Gilbert::masked_order)), so locality
//! survives the holes: a contiguous run of addresses is still a compact blob, shown by
//! the bright **window** sweeping along the line.
//!
//! The forbidden blocks (different sizes) **bob, recombine and split**, but never
//! overlap — integer collision resolution keeps them disjoint, so the **allowed area is
//! exactly constant** and the line keeps its length as the holes move. Press `i` to
//! invert (mostly-forbidden with the moving blocks as the only allowed inclusions).
//!
//! Keys:  ←/→ window size · i invert allowed/forbidden · space pause · q quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{BorderStyle, CornerStyle, LineWeight},
    panel::{draw_panel, Panel},
    poll_event,
    spacefill::{spanning_curve, strictly_continuous, Gilbert},
    style::{Color, Style},
    Buffer, Field, Rect, Terminal,
};

/// One forbidden block: an integer footprint `w×h` at top-left `(x, y)`, drifting toward
/// a slowly-orbiting target. It only ever moves into free space, so blocks stay disjoint.
#[derive(Clone)]
struct Block {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// Orbit centre and phase for the Lissajous target (in cell units).
    cx: f32,
    cy: f32,
    phase: f32,
}

impl Block {
    /// Integer AABB overlap (touching edges is allowed; shared cells are not).
    fn overlaps(&self, o: &Block) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

struct App {
    g: Gilbert,
    blocks: Vec<Block>,
    /// The current allowed-cell line: filtered Gilbert order (landscape) or the
    /// continuous spanning-tree cycle (strand).
    order: Vec<(u32, u32)>,
    /// `false` = filled landscape (locality only); `true` = continuous 4-connected strand.
    strand: bool,
    /// In strand mode, whether the current mask admits a continuous curve.
    feasible: bool,
    invert: bool,
    head: usize,
    span: usize,
    paused: bool,
    t: f32,
}

/// The panel interior: the whole area minus the one-cell border the `Panel` draws.
/// The landscape (a `Field`) fills this, so the demo reads as a real mullion tile.
fn panel_interior(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

/// Even grid dims for the field. Both even ⇒ same parity (strictly continuous line) and
/// an exact 2×2-block grid, so the continuous-strand mode always applies.
fn grid_dims(fa: Rect) -> (u32, u32) {
    let w = ((fa.width as u32) & !1).max(4);
    let h = ((fa.height as u32) & !1).max(4);
    (w, h)
}

/// Seed a spread of different-sized forbidden blocks with distinct orbits.
fn seed_blocks(w: u32, h: u32) -> Vec<Block> {
    let (fw, fh) = (w as f32, h as f32);
    // Even sizes so a block-aligned (2×2) footprint is exact in strand mode.
    let specs = [
        (0.28, 0.30, 6, 4, 0.0),
        (0.70, 0.28, 4, 4, 1.7),
        (0.30, 0.70, 2, 6, 3.1),
        (0.72, 0.70, 6, 2, 4.6),
        (0.50, 0.50, 4, 2, 5.9),
    ];
    specs
        .iter()
        .map(|&(rx, ry, bw, bh, phase)| Block {
            x: (rx * fw) as i32,
            y: (ry * fh) as i32,
            w: bw,
            h: bh,
            cx: rx * fw,
            cy: ry * fh,
            phase,
        })
        .collect()
}

/// Advance the blocks one frame: each steps at most one cell toward its Lissajous target
/// along each axis, but only if the move stays in bounds and hits no other block. Disjoint
/// in, disjoint out — so the allowed area never changes.
fn step_blocks(blocks: &mut [Block], w: u32, h: u32, t: f32) {
    let (fw, fh) = (w as f32, h as f32);
    let amp = fw.min(fh) * 0.18;
    for i in 0..blocks.len() {
        let b = &blocks[i];
        let tx = (b.cx + amp * (t * 0.6 + b.phase).sin()).clamp(0.0, (w as i32 - b.w) as f32);
        let ty = (b.cy + amp * (t * 0.5 + b.phase * 1.3).cos()).clamp(0.0, (h as i32 - b.h) as f32);
        let (dx, dy) = ((tx.round() as i32 - b.x).signum(), (ty.round() as i32 - b.y).signum());

        // Try the x step, then the y step; cancel either if it would collide/leave.
        for (mut nx, mut ny, use_x) in [(b.x + dx, b.y, true), (0, 0, false)] {
            if !use_x {
                nx = blocks[i].x;
                ny = blocks[i].y + dy;
            }
            let cand = Block { x: nx, y: ny, ..blocks[i].clone() };
            let ok = nx >= 0
                && ny >= 0
                && nx + cand.w <= w as i32
                && ny + cand.h <= h as i32
                && blocks.iter().enumerate().all(|(j, o)| j == i || !cand.overlaps(o));
            if ok {
                blocks[i].x = nx;
                blocks[i].y = ny;
            }
        }
    }
}

impl App {
    /// Recompute the address line for the current mask. Landscape mode filters the
    /// Gilbert order (locality only); strand mode builds a continuous spanning-tree cycle
    /// over 2×2-block-snapped holes, and on an infeasible mask keeps the previous curve
    /// and flags `feasible = false` — the exclusion-zone repulsion made visible.
    fn remask(&mut self) {
        let blocks = &self.blocks;
        let invert = self.invert;
        // `snap` rounds a block to the even 2×2 lattice so strand masks are block-aligned.
        let forbidden = move |x: u32, y: u32, snap: bool| {
            blocks.iter().any(|b| {
                let (bx, by) = if snap { (b.x & !1, b.y & !1) } else { (b.x, b.y) };
                (x as i32) >= bx && (x as i32) < bx + b.w && (y as i32) >= by && (y as i32) < by + b.h
            })
        };
        if self.strand {
            match spanning_curve(self.g.width(), self.g.height(), |x, y| forbidden(x, y, true) == invert) {
                Some(o) => {
                    self.order = o;
                    self.feasible = true;
                }
                None => self.feasible = false, // repel: hold the last good strand
            }
        } else {
            self.order = self.g.masked_order(|x, y| forbidden(x, y, false) == invert);
            self.feasible = true;
        }
    }
}

fn render(buf: &mut Buffer, app: &App) {
    let area = buf.area;
    if area.height < 4 || area.width < 4 {
        return;
    }
    // Frame the landscape in a real mullion Panel: title on top, live status footer.
    let forbidden_cells = (app.g.width() * app.g.height()) as usize - app.order.len();
    let mode = if app.strand {
        if app.feasible { "STRAND (continuous)" } else { "STRAND (repelling — no path)" }
    } else {
        "LANDSCAPE (locality)"
    };
    let status = format!(
        " {}×{}  ·  {mode}  ·  line {}  ·  forbidden {forbidden_cells}  ·  {}  ·  {} ",
        app.g.width(),
        app.g.height(),
        app.order.len(),
        if app.invert { "blocks=allowed" } else { "blocks=forbidden" },
        if strictly_continuous(app.g.width(), app.g.height()) { "same-parity" } else { "mixed-parity" },
    );
    let bstyle = BorderStyle {
        weight: LineWeight::Heavy,
        corners: CornerStyle::Rounded,
        style: Style::default().fg(Color::Rgb(120, 130, 160)),
    };
    let panel = Panel::new(bstyle)
        .title("space-filling over holes — c landscape/strand · i invert · ←/→ window · space · q")
        .footer(&status);
    let interior = draw_panel(buf, area, &panel);
    if interior.width == 0 || interior.height == 0 {
        return;
    }
    let field = Field::rect(interior);
    let g = &app.g;
    let n = app.order.len().max(1);
    let (lo, hi) = (app.head, (app.head + app.span).min(app.order.len()));

    // rank[y*w+x] = position of that allowed cell on the line, or -1 if forbidden.
    let (gw, gh) = (g.width() as usize, g.height() as usize);
    let mut rank = vec![-1i32; gw * gh];
    for (i, &(x, y)) in app.order.iter().enumerate() {
        rank[y as usize * gw + x as usize] = i as i32;
    }

    field.paint(buf, |col, row| {
        let (cx, cy) = (col as usize, row as usize);
        if cx >= gw || cy >= gh {
            return Some((" ".to_string(), Style::default()));
        }
        let r = rank[cy * gw + cx];
        if r < 0 {
            // Forbidden cell — a dim carved-out block.
            return Some(("·".to_string(), Style::default().fg(Color::Rgb(60, 60, 70))));
        }
        let d = r as usize;
        let style = if d >= lo && d < hi {
            let tprog = (d - lo) as f32 / app.span.max(1) as f32;
            Style::default().fg(Color::from_hsv(0.0, 0.0, 0.6 + 0.4 * tprog)) // bright window
        } else {
            Style::default().fg(Color::from_hsv(360.0 * d as f32 / n as f32, 0.75, 0.5))
        };
        Some(("█".to_string(), style))
    });
}

fn build_app(fa: Rect) -> App {
    let (w, h) = grid_dims(fa);
    let g = Gilbert::new(w, h);
    let blocks = seed_blocks(w, h);
    let mut app = App {
        g,
        blocks,
        order: Vec::new(),
        strand: false,
        feasible: true,
        invert: false,
        head: 0,
        span: 1,
        paused: false,
        t: 0.0,
    };
    app.remask();
    app.span = (app.order.len() / 12).max(1);
    app
}

/// Headless check: the allowed area stays exactly constant as the blocks move, and a
/// masked contiguous window is a compact blob. `-- --check`.
fn selfcheck() {
    let fa = panel_interior(Rect::new(0, 0, 90, 34));
    let mut app = build_app(fa);
    let baseline = app.order.len();
    let (mut minlen, mut maxlen) = (baseline, baseline);
    for _ in 0..400 {
        app.t += 0.05;
        step_blocks(&mut app.blocks, app.g.width(), app.g.height(), app.t);
        app.remask();
        minlen = minlen.min(app.order.len());
        maxlen = maxlen.max(app.order.len());
    }
    assert_eq!(minlen, maxlen, "allowed area must be constant ({minlen}..={maxlen})");
    assert_eq!(minlen, baseline, "allowed area must equal the baseline");

    // Compactness of a masked window.
    let span = (app.order.len() / 12).max(1);
    let (mut minx, mut miny, mut maxx, mut maxy) = (u32::MAX, u32::MAX, 0, 0);
    for &(x, y) in &app.order[..span] {
        minx = minx.min(x);
        miny = miny.min(y);
        maxx = maxx.max(x);
        maxy = maxy.max(y);
    }
    let bbox = ((maxx - minx + 1) * (maxy - miny + 1)) as usize;
    assert!(bbox <= 12 * span, "masked window blob bbox {bbox} for span {span}");
    eprintln!(
        "selfcheck landscape: {}×{} grid, allowed area constant at {baseline} across 400 frames; window span {span} → bbox {bbox}",
        app.g.width(),
        app.g.height()
    );

    // Strand mode: the order must be a genuine continuous cycle (unit steps + closure),
    // and it stays continuous as the holes bob (repel keeps it valid).
    app.strand = true;
    app.remask();
    let mut continuous_frames = 0;
    for _ in 0..300 {
        app.t += 0.05;
        let saved = app.blocks.clone();
        step_blocks(&mut app.blocks, app.g.width(), app.g.height(), app.t);
        app.remask();
        if !app.feasible {
            app.blocks = saved;
            app.remask();
        }
        // Verify continuity of the current strand.
        let o = &app.order;
        for i in 0..o.len() {
            let a = o[i];
            let b = o[(i + 1) % o.len()];
            assert_eq!(a.0.abs_diff(b.0) + a.1.abs_diff(b.1), 1, "strand unit step {a:?}->{b:?}");
        }
        continuous_frames += 1;
    }
    eprintln!("selfcheck strand: continuous Hamiltonian cycle held across {continuous_frames} frames (len {})", app.order.len());
    eprintln!("all checks passed");
}

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--check") {
        selfcheck();
        return Ok(());
    }
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;
    term.enter()?;
    let result = run(&mut term);
    term.leave()?;
    result
}

fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let size = mullion::backend::Backend::size(term.backend())?;
    let mut app = build_app(panel_interior(size));

    loop {
        let size = mullion::backend::Backend::size(term.backend())?;
        let fa = panel_interior(size);
        if grid_dims(fa) != (app.g.width(), app.g.height()) {
            app = build_app(fa);
        }
        if !app.paused {
            app.t += 0.05;
            let saved = app.blocks.clone();
            step_blocks(&mut app.blocks, app.g.width(), app.g.height(), app.t);
            app.remask();
            if app.strand && !app.feasible {
                // The morph broke continuity — repel: undo the move, restore the curve.
                app.blocks = saved;
                app.remask();
            }
            let n = app.order.len().max(1);
            app.head = (app.head + (n / 240).max(1)) % n;
            app.span = app.span.min(n);
        }
        term.draw(|buf| render(buf, &app))?;
        if let Some(Event::Key(KeyEvent { code, .. })) = poll_event(Duration::from_millis(50))? {
            let n = app.order.len().max(1);
            match code {
                KeyCode::Char('q') => break,
                KeyCode::Char(' ') => app.paused = !app.paused,
                KeyCode::Char('c') => {
                    app.strand = !app.strand;
                    app.remask();
                    app.head = 0;
                    // A short comet in strand mode reads as a point moving along the line.
                    app.span = if app.strand {
                        (app.order.len() / 40).max(2)
                    } else {
                        (app.order.len() / 12).max(1)
                    };
                }
                KeyCode::Char('i') => {
                    app.invert = !app.invert;
                    app.remask();
                    app.head = 0;
                    app.span = (app.order.len() / 12).max(1);
                }
                KeyCode::Left | KeyCode::Char('-') => {
                    app.span = app.span.saturating_sub((n / 48).max(1)).max(1);
                }
                KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => {
                    app.span = (app.span + (n / 48).max(1)).min(n);
                }
                _ => {}
            }
        }
    }
    Ok(())
}
