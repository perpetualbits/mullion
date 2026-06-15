use std::io;
use crate::{buffer::Cell, geometry::Rect};

pub mod crossterm_backend;
pub mod test_backend;

pub use crossterm_backend::CrosstermBackend;
pub use test_backend::TestBackend;

/// Abstraction over a terminal (or headless surface) that a [`crate::Terminal`] writes to.
///
/// [`Terminal`](crate::Terminal) is generic over `B: Backend`, so swapping
/// [`CrosstermBackend`] for [`TestBackend`] in tests requires no changes to
/// rendering code.
///
/// # Object safety
///
/// `draw` uses `impl Trait` and is therefore **not** object-safe
/// (`Box<dyn Backend>` is not supported).  `Terminal<B>` is generic over `B`
/// so this is fine for now.  If runtime backend selection is ever needed,
/// change `draw` to accept `&mut dyn Iterator<Item = …>` or a `&[(…)]` slice.
pub trait Backend {
    /// Return the current terminal dimensions as a `Rect` anchored at `(0, 0)`.
    fn size(&self) -> io::Result<Rect>;

    /// Apply a sequence of cell changes produced by [`crate::Buffer::diff`].
    ///
    /// The iterator yields `(col, row, &Cell)` triples in scan order.
    /// Implementations must position the cursor at each `(col, row)` before
    /// writing the cell's symbol; they may not assume a particular prior cursor
    /// position.
    fn draw<'a>(&mut self, changes: impl Iterator<Item = (u16, u16, &'a Cell)>) -> io::Result<()>;

    /// Signal the start of a frame (synchronized-output begin marker).
    ///
    /// Called once before any [`draw`](Backend::draw) calls in a frame.
    /// Implementations that do not support synchronized output may no-op this.
    fn begin_frame(&mut self) -> io::Result<()>;

    /// Signal the end of a frame (synchronized-output end marker + flush).
    ///
    /// Called once after all [`draw`](Backend::draw) calls in a frame.
    fn end_frame(&mut self) -> io::Result<()>;

    /// Erase the entire visible surface, repositioning the cursor at `(0, 0)`.
    fn clear(&mut self) -> io::Result<()>;

    /// Flush any buffered output to the underlying sink.
    fn flush(&mut self) -> io::Result<()>;

    /// Enter interactive mode: alternate screen, raw mode, hidden cursor.
    ///
    /// Must be matched by a call to [`leave`](Backend::leave) before the process
    /// exits, to restore the terminal.
    fn enter(&mut self) -> io::Result<()>;

    /// Leave interactive mode: show cursor, leave alternate screen, disable raw mode.
    fn leave(&mut self) -> io::Result<()>;
}
