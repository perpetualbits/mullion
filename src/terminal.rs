use std::io;

use crossterm::event::{self, Event};

use crate::{
    backend::Backend,
    buffer::Buffer,
};

/// Double-buffered terminal driver.
///
/// `Terminal` owns two [`Buffer`]s (*front* and *back*) and a [`Backend`].
/// The front buffer holds what was last rendered to the screen.  The back
/// buffer is the scratch surface that the caller's render closure draws into.
///
/// ## Frame lifecycle
///
/// Each [`draw`](Terminal::draw) call:
/// 1. Detects and handles any terminal resize.
/// 2. Clears the back buffer and hands it to the caller's closure.
/// 3. Diffs the back buffer against the front buffer.
/// 4. Sends only the changed cells to the backend (minimal redraws).
/// 5. Swaps buffers: front ← back, back ← former front.
///
/// After the swap, `front` reflects exactly what is on screen, ready for the
/// next diff.
pub struct Terminal<B: Backend> {
    backend: B,
    /// The buffer whose content is currently displayed on the physical terminal.
    front: Buffer,
    /// The scratch buffer that the render closure writes into each frame.
    back: Buffer,
}

impl<B: Backend> Terminal<B> {
    /// Create a new `Terminal`, querying the backend for its initial size.
    ///
    /// Both buffers are initialized to that size, filled with blank (space) cells.
    ///
    /// # Errors
    /// Propagates any error from [`Backend::size`].
    pub fn new(backend: B) -> io::Result<Self> {
        let area = backend.size()?;
        Ok(Self {
            front: Buffer::empty(area),
            back: Buffer::empty(area),
            backend,
        })
    }

    /// Return a shared reference to the backend.
    ///
    /// Useful for reading [`TestBackend`](crate::backend::TestBackend) state
    /// (e.g. [`render`](crate::backend::TestBackend::render)) after a draw call.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Return a mutable reference to the backend.
    ///
    /// Used in tests to call [`TestBackend::resize`](crate::backend::TestBackend::resize)
    /// to simulate a terminal resize.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Forward to [`Backend::enter`]: alternate screen, raw mode, hidden cursor.
    pub fn enter(&mut self) -> io::Result<()> {
        self.backend.enter()
    }

    /// Forward to [`Backend::leave`]: restore cursor, normal screen, cooked mode.
    pub fn leave(&mut self) -> io::Result<()> {
        self.backend.leave()
    }

    /// Check whether the terminal dimensions changed and reallocate buffers if so.
    ///
    /// Queries [`Backend::size`] and compares it to the current front-buffer area.
    /// On a mismatch, both buffers are reallocated (resized to the new area and
    /// filled with blank cells).  The next [`draw`](Terminal::draw) call will
    /// then diff a fully-drawn back against a blank front, producing a full
    /// repaint — which is correct because the physical screen also needs to be
    /// redrawn from scratch after a resize.
    ///
    /// # Returns
    /// `true` if the dimensions changed, `false` if they are the same.
    pub fn check_resize(&mut self) -> io::Result<bool> {
        let new_area = self.backend.size()?;
        if new_area != self.front.area {
            // Both buffers must be the same size for diff to work correctly.
            // Resizing front to the new area implicitly makes it all-blank, which
            // forces the next diff to re-emit every non-blank cell.
            self.front.resize(new_area);
            self.back.resize(new_area);
            return Ok(true);
        }
        Ok(false)
    }

    /// Render one frame through the double-buffer pipeline.
    ///
    /// Execution order:
    /// 1. **Resize check** — if the terminal grew or shrank, both buffers are
    ///    reallocated and `resized` is set to `true`.
    /// 2. **Clear back buffer** — so the closure sees a blank canvas.
    /// 3. **Render closure** — `render_fn` draws into the back buffer.
    /// 4. **Diff** — compare back (desired) against front (current screen).
    /// 5. **Begin frame** — emit synchronized-output start marker.
    /// 6. **Clear screen** (only on resize) — erase stale physical content that
    ///    lies outside the new dimensions.  The front buffer is all-blank after a
    ///    resize, so the diff in step 4 will send every non-blank cell anyway;
    ///    the clear only removes content the diff cannot reach.
    /// 7. **Draw diff** — send only changed cells to the backend.
    /// 8. **End frame** — emit synchronized-output end marker and flush.
    /// 9. **Swap buffers** — the just-rendered back becomes the new front,
    ///    and the old front (stale content) becomes the next back scratch buffer.
    ///
    /// # Errors
    /// Propagates errors from any backend call.
    pub fn draw<F>(&mut self, render_fn: F) -> io::Result<()>
    where
        F: FnOnce(&mut Buffer),
    {
        let resized = self.check_resize()?;

        self.back.reset();
        render_fn(&mut self.back);

        let diff = self.back.diff(&self.front);

        self.backend.begin_frame()?;
        if resized {
            // The front buffer is now all-spaces (reallocated). Clear the
            // physical screen so stale content outside the new dimensions is
            // erased; the diff below will repaint everything non-blank.
            self.backend.clear()?;
        }
        self.backend.draw(diff.into_iter())?;
        self.backend.end_frame()?;

        // Swap front and back: front now reflects what the screen shows, and
        // back is free to be cleared and reused next frame.
        std::mem::swap(&mut self.front, &mut self.back);
        Ok(())
    }
}

/// Read a single crossterm [`Event`], blocking until one arrives.
pub fn read_event() -> io::Result<Event> {
    event::read()
}

/// Poll for a crossterm event, returning `None` if the timeout elapses first.
pub fn poll_event(timeout: std::time::Duration) -> io::Result<Option<Event>> {
    if event::poll(timeout)? {
        Ok(Some(event::read()?))
    } else {
        Ok(None)
    }
}
