// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use std::io::{self, Write};

use unicode_width::UnicodeWidthStr;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute, queue,
    style::{
        Attribute, Color as CtColor, Colors, Print, SetAttribute, SetBackgroundColor,
        SetColors, SetForegroundColor,
    },
    terminal::{
        self, disable_raw_mode, enable_raw_mode, size, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};

use crate::{
    backend::Backend,
    buffer::Cell,
    capabilities::Capabilities,
    charset::box_to_ascii,
    geometry::Rect,
    style::{Color, ColorDepth, Modifier, Style},
};

// DEC private-mode sequences for the "synchronized output" extension (xterm et al.).
// Wrapping a frame between these markers prevents the terminal from rendering
// partial frames mid-draw, eliminating visible tearing on fast redraws.
const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
const END_SYNC: &[u8] = b"\x1b[?2026l";

// Bi-Directional Support Mode (BDSM — ECMA-48 mode 8), as implemented by VTE
// (gnome-terminal, terminator, …) and other bidi-aware terminals.
//
// mullion's text engine already emits cells in **visual** order (UAX #9 applied
// per line; see `text`), so we ask the terminal for the EXPLICIT setting: do not
// apply your own implicit BiDi reordering, render what you receive. Without this,
// a row that mixes an RTL run with bidi-neutral box-drawing glyphs gets the box
// characters dragged into the RTL run at scanout, so borders appear to "break".
//
// This is a one-time switch at `enter()` (no per-cell cost), restored to the
// common IMPLICIT default on `leave()`. Terminals that do not implement BDSM
// ignore the unknown mode, so it is harmless everywhere.
const BDSM_EXPLICIT: &[u8] = b"\x1b[8l";
const BDSM_IMPLICIT: &[u8] = b"\x1b[8h";

/// A [`Backend`] that drives a real terminal via `crossterm`.
///
/// ## Lifecycle
///
/// Call [`enter`](Backend::enter) before drawing and [`leave`](Backend::leave)
/// when done.  `CrosstermBackend` implements `Drop` as a safety net: if the
/// program exits (or panics) while `entered` is true, `Drop` calls `leave()`
/// best-effort so the user's shell is not left in raw / alternate-screen mode.
/// Additionally, [`enter`](Backend::enter) installs a panic hook that emits the
/// restore sequences to stderr *before* printing the panic message, keeping it
/// readable while in raw mode.
///
/// ## Mouse capture
///
/// By default `enter` enables mouse reporting so click and scroll events are
/// delivered as `crossterm::event::MouseEvent`.  Call
/// [`set_mouse_capture(false)`](CrosstermBackend::set_mouse_capture) before
/// `enter` to opt out (e.g. when the user prefers native terminal text selection).
/// `leave` (and the panic/Drop restore path) will always emit
/// `DisableMouseCapture` when capture is enabled so the terminal is reliably
/// restored.
///
/// ## Style minimization
///
/// [`draw`](Backend::draw) tracks the last SGR (Select Graphic Rendition)
/// sequence it emitted in `last_style`.  A new SGR run is only queued when the
/// style for the next cell differs from the previously emitted style, reducing
/// the number of escape sequences sent per frame.
///
/// ## Capability adaptation
///
/// Call [`apply_capabilities`](CrosstermBackend::apply_capabilities) with a
/// [`Capabilities`] value (from [`Capabilities::detect`](crate::capabilities::Capabilities::detect))
/// to configure all adaptations at once.  Individual setters are also available:
/// [`CrosstermBackend::set_color_depth`], [`CrosstermBackend::set_unicode`], [`CrosstermBackend::set_mouse_capture`].
pub struct CrosstermBackend<W: Write> {
    /// The underlying byte sink (usually `io::Stdout` or a `Vec<u8>` in tests).
    writer: W,
    /// Last style emitted to the terminal.  `None` before the first cell is drawn.
    /// Used to suppress redundant SGR sequences between consecutive same-style cells.
    last_style: Option<Style>,
    /// Set to `true` at the end of [`enter`](Backend::enter), cleared at the end of
    /// [`leave`](Backend::leave).  Guards the [`Drop`] restore so we do not emit
    /// escape sequences when we were never in interactive mode.
    entered: bool,
    /// Whether to enable mouse capture on [`enter`](Backend::enter).
    /// Default: `true`.  Set via [`set_mouse_capture`](CrosstermBackend::set_mouse_capture).
    mouse_enabled: bool,
    /// Colour depth used to downsample [`Color::Rgb`] and [`Color::Indexed`] before
    /// emitting SGR sequences.  Default: [`ColorDepth::TrueColor`] (identity).
    color_depth: ColorDepth,
    /// When `false`, box-drawing glyphs in cell symbols are mapped to ASCII
    /// via [`box_to_ascii`] before emission.  Default: `true`.
    unicode: bool,
    /// When `false`, the `\x1b[?2026h/l` synchronized-output markers are not
    /// emitted in [`begin_frame`](Backend::begin_frame) /
    /// [`end_frame`](Backend::end_frame).  Default: `true`.
    synchronized_output: bool,
}

impl<W: Write> CrosstermBackend<W> {
    /// Wrap `writer` in a new backend, starting in the non-interactive state.
    ///
    /// Mouse capture is enabled by default; call [`set_mouse_capture(false)`](CrosstermBackend::set_mouse_capture)
    /// before [`enter`](Backend::enter) to opt out.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            last_style:         None,
            entered:            false,
            mouse_enabled:      true,
            color_depth:        ColorDepth::default(),
            unicode:            true,
            synchronized_output: true,
        }
    }

    /// Enable or disable mouse capture for the next [`enter`](Backend::enter) call.
    ///
    /// When `enabled` is `false`, neither `EnableMouseCapture` nor
    /// `DisableMouseCapture` is emitted, allowing the user to use native terminal
    /// text selection instead.  Must be called before `enter`; changing this after
    /// `enter` has been called has no effect on the current session.
    pub fn set_mouse_capture(&mut self, enabled: bool) {
        self.mouse_enabled = enabled;
    }

    /// Set the colour depth applied during [`draw`](Backend::draw).
    ///
    /// [`ColorDepth::TrueColor`] (the default) is a no-op.  [`ColorDepth::Palette256`]
    /// maps `Rgb` colours to the nearest xterm-256 entry; [`ColorDepth::Palette16`]
    /// maps `Rgb` and `Indexed` to the nearest of the 16 ANSI named colours.
    /// Call this before the first draw call for consistent results.
    pub fn set_color_depth(&mut self, depth: ColorDepth) {
        self.color_depth = depth;
    }

    /// Enable or disable Unicode box-drawing output.
    ///
    /// When `on` is `false`, each cell's symbol is passed through
    /// [`box_to_ascii`] before emission: box-drawing glyphs become `-`, `|`,
    /// or `+`, while other characters pass through unchanged.  Use this on
    /// terminals that do not render Unicode box-drawing reliably (e.g. the
    /// Linux text console).  Default: `true` (box-drawing emitted as-is).
    pub fn set_unicode(&mut self, on: bool) {
        self.unicode = on;
    }

    /// Apply a [`Capabilities`] value detected from the environment.
    ///
    /// Sets [`color_depth`](CrosstermBackend::set_color_depth),
    /// [`unicode`](CrosstermBackend::set_unicode), and the
    /// synchronized-output flag in one call.  Convenience wrapper for the
    /// [`Capabilities::detect`](crate::capabilities::Capabilities::detect) →
    /// backend setup pattern.
    pub fn apply_capabilities(&mut self, caps: &Capabilities) {
        self.color_depth        = caps.color;
        self.unicode            = caps.unicode;
        self.synchronized_output = caps.synchronized_output;
    }

    /// Write the escape sequences that restore the terminal to its normal state.
    ///
    /// Emits (in order): `BDSM_IMPLICIT` (restore bidi on the alternate screen),
    /// `DisableMouseCapture` (if mouse_enabled), `LeaveAlternateScreen`, `Show`
    /// (show cursor), then `BDSM_IMPLICIT` again (restore bidi on the primary
    /// screen — both were set explicit in `enter`).  Deliberately does **not** call
    /// `disable_raw_mode`, which is a tty syscall and therefore unavailable in
    /// tests using a `Vec<u8>` sink.  [`leave`](Backend::leave) calls
    /// `write_restore` and then `disable_raw_mode`.
    fn write_restore(&mut self) -> io::Result<()> {
        // Restore implicit BiDi on the alternate screen, leave it, then restore it
        // on the primary screen too — both were set explicit in `enter`, so the
        // user's shell is not left with reordering disabled.
        self.writer.write_all(BDSM_IMPLICIT)?;
        if self.mouse_enabled {
            execute!(self.writer, DisableMouseCapture, LeaveAlternateScreen, Show)?;
        } else {
            execute!(self.writer, LeaveAlternateScreen, Show)?;
        }
        self.writer.write_all(BDSM_IMPLICIT)?;
        Ok(())
    }

    /// Simulate having entered interactive mode without calling `enable_raw_mode`.
    ///
    /// **Only for testing.** Sets `entered = true` so that [`Drop`] / [`leave`]
    /// will emit the restore escape sequences.  This lets tests verify the
    /// restore output against a `Vec<u8>` sink without requiring a real tty.
    #[doc(hidden)]
    pub fn mark_entered(&mut self) {
        self.entered = true;
    }
}

impl<W: Write> Drop for CrosstermBackend<W> {
    /// Restore the terminal if `enter()` was called but `leave()` was not.
    ///
    /// This fires on both normal drops and on unwinding (panic), providing a
    /// safety net complementary to the panic hook installed by `enter()`.
    fn drop(&mut self) {
        if self.entered {
            let _ = self.leave(); // best-effort; ignore errors during cleanup
        }
    }
}

/// Map a mullion [`Color`] to a crossterm [`CtColor`].
///
/// The 16-color mapping follows the standard ANSI 8+8 palette.  The three
/// names that are easiest to confuse:
///
/// | Our name  | ANSI | Crossterm  | Appearance      |
/// |-----------|------|------------|-----------------|
/// | `White`   | 15   | `White`    | Bright white    |
/// | `Gray`    | 7    | `Grey`     | Standard white  |
/// | `DarkGray`| 8    | `DarkGrey` | Bright black    |
fn to_ct_color(c: Color) -> CtColor {
    match c {
        Color::Reset => CtColor::Reset,
        Color::Black => CtColor::Black,
        Color::Red => CtColor::DarkRed,
        Color::Green => CtColor::DarkGreen,
        Color::Yellow => CtColor::DarkYellow,
        Color::Blue => CtColor::DarkBlue,
        Color::Magenta => CtColor::DarkMagenta,
        Color::Cyan => CtColor::DarkCyan,
        Color::White => CtColor::White,       // bright white (ANSI 15)
        Color::DarkGray => CtColor::DarkGrey, // bright black (ANSI 8)
        Color::LightRed => CtColor::Red,
        Color::LightGreen => CtColor::Green,
        Color::LightYellow => CtColor::Yellow,
        Color::LightBlue => CtColor::Blue,
        Color::LightMagenta => CtColor::Magenta,
        Color::LightCyan => CtColor::Cyan,
        Color::Gray => CtColor::Grey,         // standard grey (ANSI 7)
        Color::Indexed(i) => CtColor::AnsiValue(i),
        Color::Rgb(r, g, b) => CtColor::Rgb { r, g, b },
    }
}

/// Queue the SGR sequences to move the terminal from `from` to `to` — a **minimal
/// delta**, so a run of cells that only change colour does not re-emit a full reset.
///
/// A terminal accumulates SGR state, so *removing* an attribute (e.g. bold) needs a
/// `\x1b[0m` reset followed by re-applying everything. But the common case — same
/// modifiers, a different colour (e.g. video, where every cell recolours) — emits
/// just the changed colour. `from` is `None` before the first cell, which forces the
/// reset path to establish a known state.
fn emit_style<W: Write>(w: &mut W, from: Option<Style>, to: Style) -> io::Result<()> {
    // A reset is required when we have no known prior state, or when any modifier was
    // turned off (only `\x1b[0m` can clear an attribute).
    let reset = match from {
        None => true,
        Some(f) => !f.mods.difference(to.mods).is_empty(),
    };
    if reset {
        // `\x1b[0m` clears all attributes *and* colours, so re-apply the full style.
        queue!(w, SetAttribute(Attribute::Reset))?;
        queue!(w, SetColors(Colors::new(to_ct_color(to.fg), to_ct_color(to.bg))))?;
        emit_mods(w, to.mods)?;
    } else {
        let from = from.unwrap(); // `None` took the reset path above
        if to.fg != from.fg {
            queue!(w, SetForegroundColor(to_ct_color(to.fg)))?;
        }
        if to.bg != from.bg {
            queue!(w, SetBackgroundColor(to_ct_color(to.bg)))?;
        }
        // Only modifiers newly added (none were removed, or we'd have reset).
        emit_mods(w, to.mods.difference(from.mods))?;
    }
    Ok(())
}

/// Queue `SetAttribute` for each modifier present in `mods`.
fn emit_mods<W: Write>(w: &mut W, mods: Modifier) -> io::Result<()> {
    if mods.contains(Modifier::BOLD) {
        queue!(w, SetAttribute(Attribute::Bold))?;
    }
    if mods.contains(Modifier::DIM) {
        queue!(w, SetAttribute(Attribute::Dim))?;
    }
    if mods.contains(Modifier::ITALIC) {
        queue!(w, SetAttribute(Attribute::Italic))?;
    }
    if mods.contains(Modifier::UNDERLINE) {
        queue!(w, SetAttribute(Attribute::Underlined))?;
    }
    if mods.contains(Modifier::REVERSE) {
        queue!(w, SetAttribute(Attribute::Reverse))?;
    }
    Ok(())
}

impl<W: Write> Backend for CrosstermBackend<W> {
    /// Query the current terminal size from the OS.
    fn size(&self) -> io::Result<Rect> {
        let (w, h) = size()?;
        Ok(Rect::new(0, 0, w, h))
    }

    /// Apply changed cells to the terminal, minimizing SGR output.
    ///
    /// For each `(col, row, cell)` in `changes`:
    /// 1. Move the cursor to `(col, row)` — **only if it is not already there**.
    ///    After printing a glyph the terminal auto-advances the cursor by the
    ///    glyph's width, so a run of adjacent changed cells (the common case in a
    ///    repaint, since the diff is row-major) needs just one `MoveTo` at its
    ///    start, not one per cell. We track the expected cursor column and skip the
    ///    move when the next cell is exactly where the cursor already sits. (A cell
    ///    at `x + width` can only exist when `x + width` is on-screen, so this never
    ///    skips a move across a line wrap.)
    /// 2. Emit a new SGR run **only if** the cell's style differs from the last
    ///    emitted style (`last_style`), avoiding redundant escape sequences for
    ///    consecutive cells that share the same style.
    /// 3. Print the cell's symbol.
    ///
    /// `last_style` is **not** reset between frames intentionally: if the first
    /// cell of a new frame has the same style as the last cell of the previous
    /// frame, we skip the redundant SGR.  The reset happens implicitly because
    /// `begin_frame` does not clear `last_style`; a style change in the new
    /// frame will trigger a fresh emit.
    fn draw<'a>(
        &mut self,
        changes: impl Iterator<Item = (u16, u16, &'a Cell)>,
    ) -> io::Result<()> {
        let depth   = self.color_depth;
        let unicode = self.unicode;
        // Where the cursor will sit after the last print, or `None` if unknown
        // (start of frame, or after a gap). Reset each call so a new frame always
        // begins with an explicit move.
        let mut cursor: Option<(u16, u16)> = None;
        for (x, y, cell) in changes {
            if cursor != Some((x, y)) {
                queue!(self.writer, MoveTo(x, y))?;
            }
            // Downsample the cell's fg/bg before comparing and emitting so that
            // last_style always tracks what was actually sent to the terminal.
            let style = Style {
                fg:   cell.style.fg.downsample(depth),
                bg:   cell.style.bg.downsample(depth),
                mods: cell.style.mods,
            };
            if self.last_style != Some(style) {
                // Style changed: emit the minimal SGR delta from the last style.
                emit_style(&mut self.writer, self.last_style, style)?;
                self.last_style = Some(style);
            }
            if unicode {
                queue!(self.writer, Print(&cell.symbol))?;
            } else {
                // Map box-drawing glyphs to ASCII; content chars are unchanged.
                let mapped: String = cell.symbol.chars().map(box_to_ascii).collect();
                queue!(self.writer, Print(mapped))?;
            }
            // The terminal advanced the cursor past the glyph (width 1 or 2).
            let w = UnicodeWidthStr::width(cell.symbol.as_str()).max(1) as u16;
            cursor = Some((x + w, y));
        }
        Ok(())
    }

    /// Emit the synchronized-output begin marker (if enabled).
    ///
    /// The `?2026h` DEC private mode tells supporting terminals to buffer their
    /// rendering until the matching end marker, preventing partial-frame tearing.
    /// Skipped when `synchronized_output` is `false` (set via
    /// [`apply_capabilities`](CrosstermBackend::apply_capabilities)).
    fn begin_frame(&mut self) -> io::Result<()> {
        if self.synchronized_output {
            self.writer.write_all(BEGIN_SYNC)?;
        }
        Ok(())
    }

    /// Emit the synchronized-output end marker (if enabled) and flush the writer.
    ///
    /// Must be called after all [`draw`](Backend::draw) calls for the current
    /// frame.  The flush ensures the frame reaches the terminal promptly.
    fn end_frame(&mut self) -> io::Result<()> {
        if self.synchronized_output {
            self.writer.write_all(END_SYNC)?;
        }
        self.flush()
    }

    /// Clear the entire terminal screen and home the cursor to `(0, 0)`.
    fn clear(&mut self) -> io::Result<()> {
        execute!(
            self.writer,
            terminal::Clear(terminal::ClearType::All),
            MoveTo(0, 0)
        )
    }

    /// Flush buffered output to the terminal.
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    /// Enter interactive mode: raw mode, alternate screen, hidden cursor, mouse capture.
    ///
    /// Steps (in order):
    /// 1. Enable raw mode so keystrokes are delivered immediately without
    ///    line-editing or echoing.
    /// 2. Switch the terminal to BDSM *explicit* mode so it does not re-apply BiDi
    ///    to mullion's already visual-ordered cells, enter the alternate screen
    ///    buffer (so the normal shell output is preserved and restored on exit),
    ///    then set BDSM explicit *again* — VTE tracks the mode per screen buffer,
    ///    so it is set on both the primary and the alternate screen (see
    ///    [`BDSM_EXPLICIT`]).
    /// 3. Hide the cursor to prevent it from flickering over the UI.
    /// 4. Enable mouse capture (if [`mouse_enabled`](CrosstermBackend::set_mouse_capture)
    ///    is `true`) so click and scroll events are delivered.
    /// 5. Set `entered = true` to arm the [`Drop`] guard.
    /// 6. Install a panic hook that emits restore sequences to stderr before
    ///    printing the panic message.  Without this, a panic in raw mode
    ///    produces garbled output because the terminal is still in raw mode
    ///    when the default panic handler writes to stderr.
    ///
    /// # Errors
    /// Returns an error if raw mode cannot be enabled (e.g. if `stdout` is not a
    /// tty), or if writing the setup escape sequences to the writer fails.  If raw
    /// mode fails, neither the alternate screen nor the panic hook is installed.
    fn enter(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        // Tell bidi-aware terminals (VTE: gnome-terminal, terminator, …) not to
        // reorder — mullion supplies cells in visual order. VTE tracks BDSM per
        // screen buffer, so set it on the primary screen first, then again on the
        // alternate screen after switching; setting it only after the switch does
        // not stick. Restored on both in `write_restore`.
        self.writer.write_all(BDSM_EXPLICIT)?;
        execute!(self.writer, EnterAlternateScreen, Hide)?;
        self.writer.write_all(BDSM_EXPLICIT)?;
        if self.mouse_enabled {
            // Enable basic button press/release, button-motion, and SGR mouse
            // (extended coords for wide terminals).
            execute!(self.writer, EnableMouseCapture)?;
        }
        self.entered = true;

        // Capture mouse_enabled at hook-install time so the panic closure can
        // emit the matching disable sequence without accessing self.
        let mouse_enabled_for_hook = self.mouse_enabled;
        // Chain the new hook onto the existing one so that any hook previously
        // installed (e.g. by a test framework) still runs.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // These are best-effort; the main cleanup path is the Drop impl.
            let _ = disable_raw_mode();
            let _ = std::io::stderr().write_all(BDSM_IMPLICIT);
            if mouse_enabled_for_hook {
                let _ = execute!(std::io::stderr(), DisableMouseCapture, LeaveAlternateScreen, Show);
            } else {
                let _ = execute!(std::io::stderr(), LeaveAlternateScreen, Show);
            }
            prev(info);
        }));

        Ok(())
    }

    /// Leave interactive mode: restore cursor, alternate screen, and raw mode.
    ///
    /// Calls the internal `write_restore` helper for the escape sequences,
    /// then `disable_raw_mode` best-effort (errors are swallowed so the function
    /// returns the result of the write, not the syscall).
    /// Clears `entered` so [`Drop`] does not repeat the cleanup.
    fn leave(&mut self) -> io::Result<()> {
        let r = self.write_restore();
        let _ = disable_raw_mode(); // best-effort; harmless if not in raw mode (e.g. in tests)
        self.entered = false;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_mapping_white_gray_darkgray() {
        assert_eq!(to_ct_color(Color::White), CtColor::White,
            "White must map to bright white (ANSI 15)");
        assert_eq!(to_ct_color(Color::Gray), CtColor::Grey,
            "Gray must map to standard grey (ANSI 7)");
        assert_eq!(to_ct_color(Color::DarkGray), CtColor::DarkGrey,
            "DarkGray must map to bright black (ANSI 8)");
    }

    #[test]
    fn leave_writes_restore_sequences() {
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            backend.entered = true; // test seam: skip enable_raw_mode
            backend.leave().unwrap();
            assert!(!backend.entered, "entered flag must be cleared");
        } // drop backend here to release the &mut buf borrow
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains("\x1b[?1049l"), "missing leave-alt-screen in: {out:?}");
        assert!(out.contains("\x1b[?25h"), "missing show-cursor in: {out:?}");
        // Default mouse_enabled=true → leave must also disable mouse capture.
        assert!(out.contains("\x1b[?1000l"), "missing disable-mouse in: {out:?}");
        // BiDi support mode is restored to the terminal's implicit default.
        assert!(out.contains("\x1b[8h"), "missing BDSM-implicit restore in: {out:?}");
    }

    #[test]
    fn drop_writes_restore_sequences() {
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            backend.entered = true; // test seam: skip enable_raw_mode
        } // Drop triggers here; releases borrow so buf is readable below
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains("\x1b[?1049l"), "Drop must emit leave-alt-screen");
        assert!(out.contains("\x1b[?25h"), "Drop must emit show-cursor");
        assert!(out.contains("\x1b[?1000l"), "Drop must disable mouse capture");
    }

    #[test]
    fn color_depth_truecolor_emits_rgb_sequence() {
        use crate::buffer::Cell;
        use crate::style::{Modifier, Style};
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            // TrueColor is the default: Rgb must produce a `38;2;` truecolor sequence.
            let cell = Cell {
                symbol: "X".into(),
                style:  Style { fg: Color::Rgb(200, 100, 50), bg: Color::Reset, mods: Modifier::empty() },
            };
            backend.draw(std::iter::once((0u16, 0u16, &cell))).unwrap();
        } // drop backend to release &mut buf borrow
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains("38;2;"), "TrueColor must emit truecolor fg: {out:?}");
    }

    #[test]
    fn adjacent_cells_share_one_move() {
        use crate::buffer::Cell;
        use crate::style::Style;
        let (a, b) = (Cell { symbol: "X".into(), style: Style::default() },
                      Cell { symbol: "Y".into(), style: Style::default() });
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            backend.draw([(0u16, 0u16, &a), (1u16, 0u16, &b)].into_iter()).unwrap();
        }
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains("\x1b[1;1H"), "first cell needs a move: {out:?}");
        assert!(!out.contains("\x1b[1;2H"), "adjacent cell must not re-emit a move: {out:?}");
    }

    #[test]
    fn gap_between_cells_emits_a_move() {
        use crate::buffer::Cell;
        use crate::style::Style;
        let (a, b) = (Cell { symbol: "X".into(), style: Style::default() },
                      Cell { symbol: "Y".into(), style: Style::default() });
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            backend.draw([(0u16, 0u16, &a), (5u16, 0u16, &b)].into_iter()).unwrap();
        }
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains("\x1b[1;6H"), "non-adjacent cell needs its own move: {out:?}");
    }

    #[test]
    fn emit_style_colour_only_change_skips_reset() {
        use crate::style::{Modifier, Style};
        let from = Style { fg: Color::Red, bg: Color::Reset, mods: Modifier::BOLD };
        let to = Style { fg: Color::Blue, bg: Color::Reset, mods: Modifier::BOLD };
        let mut buf = Vec::<u8>::new();
        emit_style(&mut buf, Some(from), to).unwrap();
        let out = String::from_utf8_lossy(&buf);
        assert!(!out.contains("\x1b[0m"), "same modifiers + colour change must not reset: {out:?}");
        assert!(!out.is_empty(), "the colour change must still be emitted");
    }

    #[test]
    fn emit_style_removing_a_modifier_resets() {
        use crate::style::{Modifier, Style};
        let from = Style { fg: Color::Red, bg: Color::Reset, mods: Modifier::BOLD };
        let to = Style { fg: Color::Red, bg: Color::Reset, mods: Modifier::empty() };
        let mut buf = Vec::<u8>::new();
        emit_style(&mut buf, Some(from), to).unwrap();
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains("\x1b[0m"), "removing a modifier must emit a reset: {out:?}");
    }

    #[test]
    fn color_depth_palette16_omits_rgb_sequence() {
        use crate::buffer::Cell;
        use crate::style::{Modifier, Style};
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            backend.set_color_depth(ColorDepth::Palette16);
            // Rgb is downsampled to a named color before emitting; no 38;2; should appear.
            let cell = Cell {
                symbol: "X".into(),
                style:  Style { fg: Color::Rgb(200, 100, 50), bg: Color::Reset, mods: Modifier::empty() },
            };
            backend.draw(std::iter::once((0u16, 0u16, &cell))).unwrap();
        } // drop backend to release &mut buf borrow
        let out = String::from_utf8_lossy(&buf);
        assert!(!out.contains("38;2;"), "Palette16 must not emit truecolor fg: {out:?}");
        assert!(!out.contains("48;2;"), "Palette16 must not emit truecolor bg: {out:?}");
    }

    #[test]
    fn unicode_true_emits_box_char_unchanged() {
        use crate::buffer::Cell;
        use crate::style::{Modifier, Style};
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            // unicode=true is the default.
            let cell = Cell {
                symbol: "─".into(),
                style:  Style { fg: Color::Reset, bg: Color::Reset, mods: Modifier::empty() },
            };
            backend.draw(std::iter::once((0u16, 0u16, &cell))).unwrap();
        }
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains('─'), "unicode=true must emit '─' as-is: {out:?}");
    }

    #[test]
    fn unicode_false_maps_horizontal_box_char_to_dash() {
        use crate::buffer::Cell;
        use crate::style::{Modifier, Style};
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            backend.set_unicode(false);
            let cell = Cell {
                symbol: "─".into(),
                style:  Style { fg: Color::Reset, bg: Color::Reset, mods: Modifier::empty() },
            };
            backend.draw(std::iter::once((0u16, 0u16, &cell))).unwrap();
        }
        let out = String::from_utf8_lossy(&buf);
        assert!(!out.contains('─'), "unicode=false must replace '─': {out:?}");
        assert!(out.contains('-'), "unicode=false must emit '-' for '─': {out:?}");
    }

    #[test]
    fn unicode_false_maps_corner_to_plus() {
        use crate::buffer::Cell;
        use crate::style::{Modifier, Style};
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            backend.set_unicode(false);
            let cell = Cell {
                symbol: "┌".into(),
                style:  Style { fg: Color::Reset, bg: Color::Reset, mods: Modifier::empty() },
            };
            backend.draw(std::iter::once((0u16, 0u16, &cell))).unwrap();
        }
        let out = String::from_utf8_lossy(&buf);
        assert!(out.contains('+'), "unicode=false must emit '+' for '┌': {out:?}");
    }

    #[test]
    fn apply_capabilities_combines_color_and_unicode() {
        use crate::buffer::Cell;
        use crate::capabilities::Capabilities;
        use crate::style::{Modifier, Style};
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            backend.apply_capabilities(&Capabilities {
                color: ColorDepth::Palette16,
                unicode: false,
                synchronized_output: true,
            });
            let cell = Cell {
                symbol: "─".into(),
                style:  Style { fg: Color::Rgb(255, 0, 0), bg: Color::Reset, mods: Modifier::empty() },
            };
            backend.draw(std::iter::once((0u16, 0u16, &cell))).unwrap();
        }
        let out = String::from_utf8_lossy(&buf);
        // Palette16: no truecolor sequence.
        assert!(!out.contains("38;2;"), "Palette16 must not emit truecolor fg: {out:?}");
        // unicode=false: box char replaced.
        assert!(!out.contains('─'), "unicode=false must replace '─': {out:?}");
        assert!(out.contains('-'), "unicode=false must emit '-': {out:?}");
    }

    #[test]
    fn leave_omits_mouse_disable_when_capture_disabled() {
        let mut buf = Vec::<u8>::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            backend.set_mouse_capture(false);
            backend.mark_entered();
            backend.leave().unwrap();
        }
        let out = String::from_utf8_lossy(&buf);
        assert!(!out.contains("\x1b[?1000l"),
            "set_mouse_capture(false) must suppress disable-mouse: {out:?}");
        // Normal restore sequences must still be present.
        assert!(out.contains("\x1b[?1049l"), "leave-alt-screen must still appear");
        assert!(out.contains("\x1b[?25h"), "show-cursor must still appear");
    }
}
