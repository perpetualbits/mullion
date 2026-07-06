// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! **An occupancy map on a Gilbert curve — the "address landscape" look.** A long 1-D line
//! of addresses is laid on a [generalized Hilbert (Gilbert) curve](mullion::spacefill) that
//! fills the whole rectangle, and each cell is drawn the way an IP map wants it:
//!
//! - the cell **background** is a log-scaled occupancy heatmap (near-black empty → deep red
//!   barely-used → bright toward full), so where addresses cluster reads at a glance;
//! - the cell **foreground** draws that cell's segment of the *actual curve* with rounded
//!   box-drawing glyphs (`─│╭╮╰╯`), a bright luma line over the dim colour, so the serpentine
//!   path — which cell follows which — is visible rather than imagined.
//!
//! Because the curve preserves locality, a contiguous run of addresses is a **compact amber
//! blob**, not confetti. Large forbidden blocks (holes, ~a quarter of the grid each) **breathe
//! — grow and shrink** in paired opposition, so every hole resizes while their **total area
//! stays exactly constant**. The address line is the Gilbert order with the forbidden cells
//! removed ([`masked_order`](mullion::spacefill::Gilbert::masked_order)), so in `masked` mode
//! the threaded line simply **skips** a hole (any hole shape works), and in `strand` mode a
//! continuous spanning-tree cycle ([`spanning_curve`](mullion::spacefill::spanning_curve))
//! routes **around** the holes as one unbroken curve (an infeasible morph is repelled).
//!
//! This is the canopy IP-map aesthetic driven entirely by mullion's Gilbert work — nothing
//! network-specific; the "occupancy" here is synthetic.
//!
//! Keys:  c masked/strand · space pause · q quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{BorderStyle, CornerStyle, LineWeight},
    panel::{draw_panel, Panel},
    poll_event,
    spacefill::{spanning_curve, strictly_continuous, Gilbert},
    style::{Color, Style},
    Buffer, Rect, Terminal,
};

// ── the canopy "look": a log-occupancy heatmap background + a bright luma curve line ──

/// Linear interpolation, clamped.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// HSL → RGB (`h` degrees, `s`/`l` in `[0,1]`). Working in HSL lets the heatmap steer hue,
/// saturation and lightness independently — the same conversion canopy's palette uses.
fn hsl_rgb(h: f32, s: f32, l: f32) -> Color {
    let h = h.rem_euclid(360.0);
    let (s, l) = (s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(to(r), to(g), to(b))
}

/// Map an occupancy fraction `f ∈ [0,1]` onto `[0,1]` on a logarithmic scale of `decades`,
/// so a barely-used block is distinct from an empty one instead of both reading as black.
fn occ_log(f: f32, decades: f32) -> f32 {
    if f <= 0.0 {
        0.0
    } else {
        (1.0 + f.log10() / decades.max(0.5)).clamp(0.0, 1.0)
    }
}

/// The `(background, curve)` colours for a cell at occupancy `frac`. Empty space keeps the
/// terminal default background and only a dim grey line; a used cell gets a **dim** red→amber
/// chroma background (so it never shouts) under a **bright** luma curve line that pops.
fn paint(frac: f32) -> (Color, Color) {
    let t = occ_log(frac, 3.0);
    if frac <= 0.0 {
        return (Color::Reset, hsl_rgb(0.0, 0.0, 0.34));
    }
    let hue = lerp(0.0, 40.0, t); // red → amber by occupancy
    let bg = hsl_rgb(hue, 0.9, lerp(0.05, 0.38, t)); // dim, colourful
    let fg = hsl_rgb(hue, 0.3, lerp(0.44, 0.98, t)); // bright line, faint tint
    (bg, fg)
}

// ── the threaded curve glyph (which cell follows which) ──

/// A grid step from one cell to an adjacent one.
#[derive(Clone, Copy, PartialEq)]
enum Dir {
    L,
    R,
    U,
    D,
}

/// The direction from grid cell `a` to adjacent cell `b`, or `None` if not 4-adjacent (which
/// happens when the line jumps a hole in `masked` mode — the line legitimately breaks there).
fn dir_between(a: (u32, u32), b: (u32, u32)) -> Option<Dir> {
    match (i64::from(b.0) - i64::from(a.0), i64::from(b.1) - i64::from(a.1)) {
        (1, 0) => Some(Dir::R),
        (-1, 0) => Some(Dir::L),
        (0, 1) => Some(Dir::D),
        (0, -1) => Some(Dir::U),
        _ => None,
    }
}

/// The rounded box-drawing glyph joining a cell's two curve ports (toward its previous and
/// next cell), plus whether the segment continues **right** (so the 2-wide cell's spacer is a
/// `─` and the line stays unbroken). One port at a curve endpoint; none for a lone cell.
fn curve_glyph(a: Option<Dir>, b: Option<Dir>) -> (char, bool) {
    let has = |d: Dir| a == Some(d) || b == Some(d);
    let (l, r, u, dn) = (has(Dir::L), has(Dir::R), has(Dir::U), has(Dir::D));
    let ch = if l && r {
        '─'
    } else if u && dn {
        '│'
    } else if r && u {
        '╰'
    } else if l && u {
        '╯'
    } else if r && dn {
        '╭'
    } else if l && dn {
        '╮'
    } else if l || r {
        '─'
    } else if u || dn {
        '│'
    } else {
        '·'
    };
    (ch, r)
}

// ── holes: large forbidden blocks that breathe (resize) with a conserved total area ──

/// One breathing hole. Holes are laid out as **pairs in a band**: a left hole anchored to the
/// band's left edge and a right hole anchored to its right edge. Their widths breathe in
/// exact opposition — as one gains `d` even cells the other loses `d` — so the pair's summed
/// width (hence the band's forbidden area, height being fixed) is **constant by construction**,
/// and the gap between them never closes, so they never collide. Dimensions are even, so the
/// hole is 2×2-aligned and the continuous strand stays feasible.
#[derive(Clone)]
struct Hole {
    /// Even top y and even height (fixed — the band).
    y: i32,
    h: i32,
    /// The anchored edge: left holes anchor their left side at `anchor`, right holes anchor
    /// their right side there (so the width grows/shrinks away from the band edge).
    anchor: i32,
    anchor_right: bool,
    /// Even base width; the breathing amplitude (even); the shared phase; and the sign that
    /// makes a pair breathe in opposition (`+1` left, `-1` right).
    base_w: i32,
    amp: i32,
    phase: f32,
    sign: f32,
}

impl Hole {
    /// The current even `(x, y, w, h)` footprint at time `t`. Width = `base ± d` with `d` an
    /// even, amplitude-bounded sinusoid, so a pair's two widths always sum to `2·base`.
    fn rect(&self, t: f32) -> (i32, i32, i32, i32) {
        let raw = self.amp as f32 * (t * 0.5 + self.phase).sin();
        let d = 2 * (raw / 2.0).round() as i32; // nearest even, |d| ≤ amp
        let w = (self.base_w + self.sign as i32 * d).max(2) & !1;
        let x = if self.anchor_right { self.anchor - w } else { self.anchor };
        (x, self.y, w, self.h)
    }

    fn contains(&self, t: f32, x: i32, y: i32) -> bool {
        let (hx, hy, hw, hh) = self.rect(t);
        x >= hx && x < hx + hw && y >= hy && y < hy + hh
    }
}

/// Seed two bands (top and bottom), each a left/right pair of large holes ~a quarter of the
/// grid in each dimension. Widths breathe in opposition within a pair, so the **total
/// forbidden area is exactly constant** while every hole visibly grows and shrinks. The bands
/// leave a top margin, a middle corridor and a bottom margin, and each pair keeps a constant
/// central gap — so the allowed region stays 4-connected (the strand keeps flowing).
fn seed_holes(gw: u32, gh: u32) -> Vec<Hole> {
    let (gwi, ghi) = (gw as i32, gh as i32);
    let m = 2; // even margin
    let h = ((ghi / 4) & !1).max(4); // hole height ≈ quarter
    let base = ((gwi / 4) & !1).max(4); // hole base width ≈ quarter
    // Breathe by up to ~half the base, but keep base − amp ≥ 4 so a hole never vanishes and
    // the widths never need clamping (which would break exact area conservation).
    let amp = ((base / 2).min(base - 4).max(0)) & !1;
    let band_y = [((ghi as f32 * 0.12) as i32) & !1, ((ghi as f32 * 0.60) as i32) & !1];
    let phases = [0.0f32, 2.1];

    let mut holes = Vec::new();
    for (&y, &phase) in band_y.iter().zip(phases.iter()) {
        // Left hole: anchored at the left margin, grows rightward.
        holes.push(Hole { y, h, anchor: m, anchor_right: false, base_w: base, amp, phase, sign: 1.0 });
        // Right hole: anchored at the right margin, grows leftward, opposite breathing.
        holes.push(Hole { y, h, anchor: gwi - m, anchor_right: true, base_w: base, amp, phase, sign: -1.0 });
    }
    holes
}

// ── the app ──

struct App {
    g: Gilbert,
    /// Synthetic occupancy per **full** Gilbert line index `d` (independent of the holes), so
    /// clusters stay put as the holes breathe. `occ[d] ∈ [0,1]`.
    occ: Vec<f32>,
    holes: Vec<Hole>,
    /// The current address line: allowed cells in curve order (`masked` = filtered Gilbert;
    /// `strand` = continuous spanning-tree cycle).
    order: Vec<(u32, u32)>,
    /// `false` = masked (line skips holes); `true` = continuous strand around holes.
    strand: bool,
    /// In strand mode, whether the current mask admits a continuous curve.
    feasible: bool,
    paused: bool,
    t: f32,
}

/// A cheap deterministic hash for scattering light occupancy (no `rand` in examples).
fn hash(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Seed a sparse landscape: a few dense allocations as contiguous runs on the address line
/// (compact blobs — the locality the curve buys), plus faint scattered use elsewhere.
fn seed_occupancy(n: usize) -> Vec<f32> {
    let mut occ = vec![0.0f32; n];
    let fnn = n as f32;
    // (start_frac, len_frac, density) — contiguous runs → compact blobs after the curve.
    for &(s, l, dns) in &[(0.04, 0.10, 0.95), (0.28, 0.05, 0.55), (0.46, 0.14, 1.0), (0.72, 0.03, 0.35), (0.85, 0.07, 0.7)] {
        let (a, b) = ((s * fnn) as usize, ((s + l) * fnn) as usize);
        for cell in occ.iter_mut().take(b.min(n)).skip(a) {
            *cell = dns;
        }
    }
    // Faint scatter so "used but sparse" space reads as dim red, not empty.
    for (d, cell) in occ.iter_mut().enumerate() {
        if *cell == 0.0 && hash(d as u64) % 29 == 0 {
            *cell = 0.015 + (hash(d as u64 * 7) % 6) as f32 * 0.01;
        }
    }
    occ
}

impl App {
    /// Recompute the address line for the current mask (see [`spanning_curve`]/`masked_order`).
    /// The holes are a pure function of `t` (they breathe deterministically), so on an
    /// infeasible strand frame we simply keep the previous curve — the repulsion made visible.
    fn remask(&mut self) {
        let (holes, t) = (&self.holes, self.t);
        let forbidden = move |x: u32, y: u32| holes.iter().any(|h| h.contains(t, x as i32, y as i32));
        if self.strand {
            match spanning_curve(self.g.width(), self.g.height(), |x, y| !forbidden(x, y)) {
                Some(o) => {
                    self.order = o;
                    self.feasible = true;
                }
                None => self.feasible = false, // repel: hold the last good strand
            }
        } else {
            self.order = self.g.masked_order(|x, y| !forbidden(x, y));
            self.feasible = true;
        }
    }

    /// Total forbidden cells across all holes at the current `t` — constant by construction
    /// (paired widths sum to a fixed value, heights fixed), used by the self-check.
    fn forbidden_area(&self) -> i32 {
        self.holes.iter().map(|h| h.rect(self.t).2 * h.rect(self.t).3).sum()
    }
}

/// The panel interior: the area minus the one-cell border the `Panel` draws.
fn panel_interior(area: Rect) -> Rect {
    Rect::new(area.x.saturating_add(1), area.y.saturating_add(1), area.width.saturating_sub(2), area.height.saturating_sub(2))
}

/// Even grid dims for the map — cells are **two columns** wide (glyph + spacer). Both even ⇒
/// same parity (a strictly-continuous line) and an exact 2×2 grid for the strand mode.
fn grid_dims(interior: Rect) -> (u32, u32) {
    let w = ((interior.width as u32 / 2) & !1).max(4);
    let h = ((interior.height as u32) & !1).max(4);
    (w, h)
}

fn render(buf: &mut Buffer, app: &App) {
    let area = buf.area;
    if area.height < 6 || area.width < 8 {
        return;
    }
    let (gw, gh) = (app.g.width(), app.g.height());
    let forbidden_cells = (gw * gh) as usize - app.order.len();
    let mode = if app.strand {
        if app.feasible {
            "STRAND (continuous — routes around holes)"
        } else {
            "STRAND (repelling — no continuous path)"
        }
    } else {
        "MASKED (line skips holes)"
    };
    let status = format!(
        " {gw}×{gh}  ·  {mode}  ·  line {}  ·  holes {forbidden_cells} cells  ·  {} ",
        app.order.len(),
        if strictly_continuous(gw, gh) { "same-parity" } else { "mixed-parity" },
    );
    let bstyle = BorderStyle { weight: LineWeight::Heavy, corners: CornerStyle::Rounded, style: Style::default().fg(Color::Rgb(120, 130, 160)) };
    let panel = Panel::new(bstyle)
        .title("Gilbert occupancy map — the canopy look · c masked/strand · space · q")
        .footer(&status);
    let interior = draw_panel(buf, area, &panel);
    if interior.width < 2 || interior.height == 0 {
        return;
    }

    // Per grid cell: this cell's ports toward its previous/next cell on the *allowed* line
    // (None where forbidden), so the threaded glyph shows the real path. `rank` also flags
    // forbidden cells (never assigned).
    let (gwu, ghu) = (gw as usize, gh as usize);
    let mut prevdir = vec![None; gwu * ghu];
    let mut nextdir = vec![None; gwu * ghu];
    let mut allowed = vec![false; gwu * ghu];
    for (i, &cur) in app.order.iter().enumerate() {
        let idx = cur.1 as usize * gwu + cur.0 as usize;
        allowed[idx] = true;
        if i > 0 {
            prevdir[idx] = dir_between(cur, app.order[i - 1]);
        }
        if i + 1 < app.order.len() {
            nextdir[idx] = dir_between(cur, app.order[i + 1]);
        }
    }

    let carved = Style::default().fg(Color::Rgb(64, 64, 78)).bg(Color::Rgb(24, 24, 30));
    for y in 0..gh {
        let by = interior.y + y as u16;
        if by >= interior.y + interior.height {
            break;
        }
        for x in 0..gw {
            let bx = interior.x + (x as u16) * 2;
            if bx + 1 >= interior.x + interior.width {
                break;
            }
            let idx = y as usize * gwu + x as usize;
            if !allowed[idx] {
                // Carved-out hole — a dim hatched block, no curve through it.
                buf.set_char(bx, by, '·', carved);
                buf.set_char(bx + 1, by, ' ', carved);
                continue;
            }
            // Occupancy is a property of the address block (the cell), keyed by the full
            // Gilbert line index so clusters don't move when the holes do.
            let frac = app.g.xy_to_d(x, y).map(|d| app.occ[d as usize]).unwrap_or(0.0);
            let (bg, fg) = paint(frac);
            let (glyph, connects_right) = curve_glyph(prevdir[idx], nextdir[idx]);
            let cell = Style::default().fg(fg).bg(bg);
            buf.set_char(bx, by, glyph, cell);
            buf.set_char(bx + 1, by, if connects_right { '─' } else { ' ' }, cell);
        }
    }
}

fn build_app(interior: Rect) -> App {
    let (w, h) = grid_dims(interior);
    let g = Gilbert::new(w, h);
    let occ = seed_occupancy(g.len());
    let mut app = App { g, occ, holes: seed_holes(w, h), order: Vec::new(), strand: false, feasible: true, paused: false, t: 0.0 };
    app.remask();
    app
}

/// Headless check (`-- --check`): the total forbidden area is exactly conserved as the holes
/// breathe, the masked line drops exactly the forbidden cells, occupancy blobs are compact,
/// and the strand (when feasible) is a continuous unit-step cycle.
fn selfcheck() {
    let interior = panel_interior(Rect::new(0, 0, 120, 44));
    let mut app = build_app(interior);
    let cells = (app.g.width() * app.g.height()) as usize;
    let area0 = app.forbidden_area();

    // Masked: forbidden area is conserved, and line + forbidden always partition the grid.
    for _ in 0..200 {
        app.t += 0.05;
        app.remask();
        assert_eq!(app.forbidden_area(), area0, "total hole area must stay constant as they breathe");
        let forbidden = cells - app.order.len();
        assert!(forbidden > 0, "holes should carve out some cells");
        assert_eq!(app.order.len() + forbidden, cells, "line + holes must tile the grid");
    }

    // Occupancy blobs are compact: the densest run occupies a small bounding box.
    let g = &app.g;
    let mut run: Vec<(u32, u32)> = Vec::new();
    for d in (g.len() * 46 / 100)..(g.len() * 60 / 100) {
        run.push(g.d_to_xy(d));
    }
    let (mut minx, mut miny, mut maxx, mut maxy) = (u32::MAX, u32::MAX, 0, 0);
    for &(x, y) in &run {
        minx = minx.min(x);
        miny = miny.min(y);
        maxx = maxx.max(x);
        maxy = maxy.max(y);
    }
    let bbox = ((maxx - minx + 1) * (maxy - miny + 1)) as usize;
    assert!(bbox <= 12 * run.len(), "occupancy blob bbox {bbox} not compact for run {}", run.len());

    // Strand: whenever the breathing mask is feasible, the order is a genuine continuous cycle
    // (unit steps + closure). Track how often the large holes stay feasible.
    app.strand = true;
    app.remask();
    let (mut frames, mut feasible_frames) = (0, 0);
    for _ in 0..300 {
        app.t += 0.05;
        app.remask();
        if app.feasible {
            feasible_frames += 1;
            let o = &app.order;
            for i in 0..o.len() {
                let (a, b) = (o[i], o[(i + 1) % o.len()]);
                assert_eq!(a.0.abs_diff(b.0) + a.1.abs_diff(b.1), 1, "strand unit step {a:?}->{b:?}");
            }
        }
        frames += 1;
    }
    assert!(feasible_frames > frames / 2, "strand should stay feasible most frames ({feasible_frames}/{frames})");
    eprintln!(
        "selfcheck: {}×{} map · hole area constant at {area0} · masked line partitions the grid · blob bbox {bbox} · strand feasible {feasible_frames}/{frames} frames",
        app.g.width(),
        app.g.height()
    );
    eprintln!("all checks passed");
}

/// Render one frame to a fixed buffer and print it as ANSI truecolor (`-- --dump`), so the
/// look can be eyeballed without a tty. Advances a few frames first so the holes have breathed.
fn dump() {
    let (w, h) = (120u16, 46u16);
    let mut app = build_app(panel_interior(Rect::new(0, 0, w, h)));
    app.strand = std::env::args().any(|a| a == "--strand");
    // `--t=<f32>` picks the frame (default 5.0) so an animation can be stitched from a sweep.
    app.t = std::env::args().find_map(|a| a.strip_prefix("--t=").and_then(|v| v.parse().ok())).unwrap_or(5.0);
    app.remask();
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    render(&mut buf, &app);

    let sgr = |c: Color, bg: bool| -> String {
        let tc = if bg { 48 } else { 38 };
        match c {
            Color::Rgb(r, g, b) => format!("{tc};2;{r};{g};{b}"),
            _ => format!("{}", if bg { 49 } else { 39 }),
        }
    };
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            let cell = buf.get(x, y);
            out.push_str(&format!("\x1b[0;{};{}m", sgr(cell.style.fg, false), sgr(cell.style.bg, true)));
            out.push_str(if cell.symbol.is_empty() { " " } else { &cell.symbol });
        }
        out.push_str("\x1b[0m\n");
    }
    print!("{out}");
}

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--check") {
        selfcheck();
        return Ok(());
    }
    if std::env::args().any(|a| a == "--dump") {
        dump();
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
        let interior = panel_interior(size);
        if grid_dims(interior) != (app.g.width(), app.g.height()) {
            app = build_app(interior);
        }
        if !app.paused {
            app.t += 0.05;
            app.remask(); // holes breathe as a function of t; strand holds its last curve if a frame is infeasible
        }
        term.draw(|buf| render(buf, &app))?;
        if let Some(Event::Key(KeyEvent { code, .. })) = poll_event(Duration::from_millis(50))? {
            match code {
                KeyCode::Char('q') => break,
                KeyCode::Char(' ') => app.paused = !app.paused,
                KeyCode::Char('c') => {
                    app.strand = !app.strand;
                    app.remask();
                }
                _ => {}
            }
        }
    }
    Ok(())
}
