use std::io;

use crate::{
    backend::Backend,
    buffer::{Buffer, Cell},
    geometry::Rect,
};

/// A headless [`Backend`] that accumulates draw calls into an in-memory buffer.
///
/// `TestBackend` is the standard surface for unit and integration tests.
/// Construct it with [`new`](TestBackend::new), wrap it in a
/// [`Terminal`](crate::Terminal), call [`draw`](crate::Terminal::draw) to
/// render frames, then call [`render`](TestBackend::render) to obtain a
/// plain-text snapshot for assertions.
///
/// The `clears` counter lets tests assert that a resize frame (and only a
/// resize frame) triggers a physical clear.
pub struct TestBackend {
    /// The in-memory cell grid that draw calls write into.
    buffer: Buffer,
    /// Number of times [`clear`](Backend::clear) has been called.
    ///
    /// Incremented by every [`clear`](Backend::clear) call.  Tests use this to
    /// verify that `Terminal` issues exactly one clear per resize frame and none
    /// for steady-state frames.
    pub clears: usize,
}

impl TestBackend {
    /// Create a new headless backend with the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        Self { buffer: Buffer::empty(Rect::new(0, 0, width, height)), clears: 0 }
    }

    /// Return a shared reference to the current in-memory cell buffer.
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Resize the headless surface, discarding all existing content.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.buffer.resize(Rect::new(0, 0, width, height));
    }

    /// Render the buffer as a plain-text string for snapshot assertions.
    ///
    /// Iterates the buffer row by row.  Continuation cells (the right-hand
    /// column of a 2-wide grapheme) are skipped — the wide glyph itself already
    /// occupies both visual columns in the string.  Rows are separated by `'\n'`;
    /// no trailing newline is added.
    pub fn render(&self) -> String {
        let area = self.buffer.area;
        // +1 per row for the newline separator; over-allocates for the last row.
        let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
        for row in area.y..area.bottom() {
            for col in area.x..area.right() {
                let cell = self.buffer.get(col, row);
                if !cell.is_continuation() {
                    out.push_str(&cell.symbol);
                }
            }
            // Add a newline between rows but not after the last one.
            if row + 1 < area.bottom() {
                out.push('\n');
            }
        }
        out
    }
}

impl Backend for TestBackend {
    /// Return the current headless surface dimensions.
    fn size(&self) -> io::Result<Rect> {
        Ok(self.buffer.area)
    }

    /// Apply changed cells to the in-memory buffer.
    ///
    /// Uses [`Buffer::set_grapheme`] rather than the lower-level
    /// [`Buffer::set`] so that wide-grapheme continuation cells are established
    /// correctly when replaying a diff.
    fn draw<'a>(
        &mut self,
        changes: impl Iterator<Item = (u16, u16, &'a Cell)>,
    ) -> io::Result<()> {
        for (x, y, cell) in changes {
            // set_grapheme re-establishes the continuation cell for wide glyphs,
            // keeping the in-memory buffer consistent with the wide-grapheme rule.
            self.buffer.set_grapheme(x, y, &cell.symbol, cell.style);
        }
        Ok(())
    }

    /// No-op: the headless backend does not use synchronized output.
    fn begin_frame(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// No-op: the headless backend does not flush to a real tty.
    fn end_frame(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Reset the in-memory buffer to blank and increment the `clears` counter.
    fn clear(&mut self) -> io::Result<()> {
        self.clears += 1;
        self.buffer.reset();
        Ok(())
    }

    /// No-op: no underlying byte sink to flush.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// No-op: the headless backend requires no interactive-mode setup.
    fn enter(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// No-op: the headless backend requires no interactive-mode teardown.
    fn leave(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Assert that a [`TestBackend`]'s rendered output equals an expected golden string.
///
/// Calls [`TestBackend::render`] on the backend inside `$term` and compares it
/// to `$expected` with a human-readable failure message.
///
/// # Example
/// ```
/// use tile_engine::{Terminal, backend::TestBackend, geometry::Rect, style::Style};
/// use tile_engine::assert_backend_snapshot;
///
/// let backend = TestBackend::new(5, 1);
/// let mut term = Terminal::new(backend).unwrap();
/// term.draw(|buf| { buf.set_string(0, 0, "Hello", Style::default()); }).unwrap();
/// assert_backend_snapshot!(term, "Hello");
/// ```
#[macro_export]
macro_rules! assert_backend_snapshot {
    ($term:expr, $expected:expr) => {{
        let actual = $term.backend().render();
        assert_eq!(
            actual, $expected,
            "\nSnapshot mismatch.\nExpected:\n{}\nActual:\n{}",
            $expected, actual
        );
    }};
}
