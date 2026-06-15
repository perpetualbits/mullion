use std::io;
use crate::{buffer::Cell, geometry::Rect};

pub mod crossterm_backend;
pub mod test_backend;

pub use crossterm_backend::CrosstermBackend;
pub use test_backend::TestBackend;

/// Abstraction over a terminal (or headless surface) that a [`crate::Terminal`] writes to.
pub trait Backend {
    /// Return the current terminal dimensions.
    fn size(&self) -> io::Result<Rect>;

    /// Apply a sequence of cell changes produced by [`crate::Buffer::diff`].
    fn draw<'a>(&mut self, changes: impl Iterator<Item = (u16, u16, &'a Cell)>) -> io::Result<()>;

    /// Called before the first [`draw`] of a frame (synchronized-output start).
    fn begin_frame(&mut self) -> io::Result<()>;

    /// Called after the last [`draw`] of a frame (synchronized-output end + flush).
    fn end_frame(&mut self) -> io::Result<()>;

    /// Clear the entire visible surface.
    fn clear(&mut self) -> io::Result<()>;

    /// Flush any buffered output.
    fn flush(&mut self) -> io::Result<()>;

    /// Enter interactive mode: alternate screen, raw mode, hidden cursor.
    fn enter(&mut self) -> io::Result<()>;

    /// Leave interactive mode: restore cursor, leave alternate screen, disable raw mode.
    fn leave(&mut self) -> io::Result<()>;
}
