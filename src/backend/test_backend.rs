use std::io;

use crate::{
    backend::Backend,
    buffer::{Buffer, Cell},
    geometry::Rect,
};

/// A headless [`Backend`] that accumulates draw calls into an in-memory buffer.
///
/// Useful for snapshot tests: build a frame with [`crate::Terminal`], then call
/// [`TestBackend::to_string`] to get a plain-text grid for golden assertions.
pub struct TestBackend {
    buffer: Buffer,
}

impl TestBackend {
    pub fn new(width: u16, height: u16) -> Self {
        Self { buffer: Buffer::empty(Rect::new(0, 0, width, height)) }
    }

    /// Read access to the current buffer state.
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Resize the headless surface (content is discarded).
    pub fn resize(&mut self, width: u16, height: u16) {
        self.buffer.resize(Rect::new(0, 0, width, height));
    }

    /// Render the buffer as plain text, one line per row.
    ///
    /// Continuation cells (right halves of wide graphemes) contribute no
    /// character — the wide grapheme's left half already accounts for 2 columns.
    pub fn render(&self) -> String {
        let area = self.buffer.area;
        let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
        for row in area.y..area.bottom() {
            for col in area.x..area.right() {
                let cell = self.buffer.get(col, row);
                if !cell.is_continuation() {
                    out.push_str(&cell.symbol);
                }
            }
            if row + 1 < area.bottom() {
                out.push('\n');
            }
        }
        out
    }
}

impl Backend for TestBackend {
    fn size(&self) -> io::Result<Rect> {
        Ok(self.buffer.area)
    }

    fn draw<'a>(
        &mut self,
        changes: impl Iterator<Item = (u16, u16, &'a Cell)>,
    ) -> io::Result<()> {
        for (x, y, cell) in changes {
            // Use set_grapheme so wide-char continuation cells are handled correctly.
            self.buffer.set_grapheme(x, y, &cell.symbol, cell.style);
        }
        Ok(())
    }

    fn begin_frame(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn end_frame(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.buffer.reset();
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn enter(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn leave(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Assert that a [`TestBackend`]'s rendered output equals an expected golden string.
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
