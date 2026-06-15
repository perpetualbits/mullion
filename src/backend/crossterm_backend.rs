// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use std::io::{self, Write};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute, queue,
    style::{
        Attribute, Color as CtColor, Colors, Print, ResetColor, SetAttribute,
        SetColors,
    },
    terminal::{
        self, disable_raw_mode, enable_raw_mode, size, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};

use crate::{
    backend::Backend,
    buffer::Cell,
    geometry::Rect,
    style::{Color, Modifier, Style},
};

// DEC private-mode sequences for the "synchronized output" extension (xterm et al.).
// Wrapping a frame between these markers prevents the terminal from rendering
// partial frames mid-draw, eliminating visible tearing on fast redraws.
const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
const END_SYNC: &[u8] = b"\x1b[?2026l";

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
/// ## Style minimization
///
/// [`draw`](Backend::draw) tracks the last SGR (Select Graphic Rendition)
/// sequence it emitted in `last_style`.  A new SGR run is only queued when the
/// style for the next cell differs from the previously emitted style, reducing
/// the number of escape sequences sent per frame.
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
}

impl<W: Write> CrosstermBackend<W> {
    /// Wrap `writer` in a new backend, starting in the non-interactive state.
    pub fn new(writer: W) -> Self {
        Self { writer, last_style: None, entered: false }
    }

    /// Write the escape sequences that restore the terminal to its normal state.
    ///
    /// Emits `LeaveAlternateScreen` and `Show` (show cursor) to `self.writer`
    /// and flushes.  Deliberately does **not** call `disable_raw_mode`, which is
    /// a tty syscall and therefore unavailable in tests using a `Vec<u8>` sink.
    /// [`leave`](Backend::leave) calls both.
    fn write_restore(&mut self) -> io::Result<()> {
        execute!(self.writer, LeaveAlternateScreen, Show)
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

/// Queue the SGR (Select Graphic Rendition) sequences for `style` onto `w`.
///
/// Uses a reset-then-set strategy: it emits `ResetColor` + `Attribute::Reset`
/// first, then the desired colors and modifiers.  This is more bytes than a
/// minimal delta but guarantees correctness: a purely additive approach would
/// leave stale attributes (e.g. bold, underline) set from a previous cell.
fn emit_style<W: Write>(w: &mut W, style: Style) -> io::Result<()> {
    // Reset all prior attributes before applying the new ones.  Without this,
    // attributes from the previous cell (e.g. bold) would bleed into cells that
    // do not set them, because the terminal accumulates SGR state.
    queue!(w, ResetColor, SetAttribute(Attribute::Reset))?;
    // Set colors in one combined SetColors call for efficiency.
    queue!(w, SetColors(Colors::new(to_ct_color(style.fg), to_ct_color(style.bg))))?;
    // Only queue attribute sequences that are actually active; the terminal's
    // SGR state for the rest was already cleared by the reset above.
    if style.mods.contains(Modifier::BOLD) {
        queue!(w, SetAttribute(Attribute::Bold))?;
    }
    if style.mods.contains(Modifier::DIM) {
        queue!(w, SetAttribute(Attribute::Dim))?;
    }
    if style.mods.contains(Modifier::ITALIC) {
        queue!(w, SetAttribute(Attribute::Italic))?;
    }
    if style.mods.contains(Modifier::UNDERLINE) {
        queue!(w, SetAttribute(Attribute::Underlined))?;
    }
    if style.mods.contains(Modifier::REVERSE) {
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
    /// 1. Move the cursor to `(col, row)`.
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
        for (x, y, cell) in changes {
            queue!(self.writer, MoveTo(x, y))?;
            let style = cell.style;
            if self.last_style != Some(style) {
                // Style changed: emit a new SGR sequence and record it.
                emit_style(&mut self.writer, style)?;
                self.last_style = Some(style);
            }
            queue!(self.writer, Print(&cell.symbol))?;
        }
        Ok(())
    }

    /// Emit the synchronized-output begin marker.
    ///
    /// The `?2026h` DEC private mode tells supporting terminals to buffer their
    /// rendering until the matching end marker, preventing partial-frame tearing.
    fn begin_frame(&mut self) -> io::Result<()> {
        self.writer.write_all(BEGIN_SYNC)?;
        Ok(())
    }

    /// Emit the synchronized-output end marker and flush the writer.
    ///
    /// Must be called after all [`draw`](Backend::draw) calls for the current
    /// frame.  The flush ensures the frame reaches the terminal promptly.
    fn end_frame(&mut self) -> io::Result<()> {
        self.writer.write_all(END_SYNC)?;
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

    /// Enter interactive mode: raw mode, alternate screen, hidden cursor.
    ///
    /// Steps (in order):
    /// 1. Enable raw mode so keystrokes are delivered immediately without
    ///    line-editing or echoing.
    /// 2. Enter the alternate screen buffer so the normal shell output is
    ///    preserved and restored on exit.
    /// 3. Hide the cursor to prevent it from flickering over the UI.
    /// 4. Set `entered = true` to arm the [`Drop`] guard.
    /// 5. Install a panic hook that emits restore sequences to stderr before
    ///    printing the panic message.  Without this, a panic in raw mode
    ///    produces garbled output because the terminal is still in raw mode
    ///    when the default panic handler writes to stderr.
    ///
    /// # Errors
    /// Returns an error if raw mode cannot be enabled (e.g. if `stdout` is not
    /// a tty).  In that case neither the alternate screen nor the panic hook is
    /// installed.
    fn enter(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(self.writer, EnterAlternateScreen, Hide)?;
        self.entered = true;

        // Chain the new hook onto the existing one so that any hook previously
        // installed (e.g. by a test framework) still runs.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // These are best-effort; the main cleanup path is the Drop impl.
            let _ = disable_raw_mode();
            let _ = execute!(std::io::stderr(), LeaveAlternateScreen, Show);
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
    }
}
