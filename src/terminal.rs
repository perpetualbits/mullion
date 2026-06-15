use std::io;

use crossterm::event::{self, Event};

use crate::{
    backend::Backend,
    buffer::Buffer,
};

/// Double-buffered terminal driver.
///
/// `Terminal` owns the two cell buffers (front = last rendered, back = in-progress)
/// and a [`Backend`]. Call [`draw`](Terminal::draw) to produce a frame:
///
/// 1. The closure receives the cleared back buffer to paint into.
/// 2. The back buffer is diffed against the front buffer.
/// 3. Only changed cells are sent to the backend.
/// 4. Buffers are swapped — the former back becomes the new front.
///
/// On a size change, both buffers are reallocated and the next frame is a full
/// repaint.
pub struct Terminal<B: Backend> {
    backend: B,
    /// Most recently displayed buffer.
    front: Buffer,
    /// Scratch buffer that widgets draw into.
    back: Buffer,
}

impl<B: Backend> Terminal<B> {
    /// Create a new terminal, querying the backend for its current size.
    pub fn new(backend: B) -> io::Result<Self> {
        let area = backend.size()?;
        Ok(Self {
            front: Buffer::empty(area),
            back: Buffer::empty(area),
            backend,
        })
    }

    /// Read access to the backend (useful for [`TestBackend::render`](crate::backend::TestBackend::render)).
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Mutable access to the backend.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Enter interactive mode (alternate screen, raw mode, hidden cursor).
    pub fn enter(&mut self) -> io::Result<()> {
        self.backend.enter()
    }

    /// Leave interactive mode.
    pub fn leave(&mut self) -> io::Result<()> {
        self.backend.leave()
    }

    /// Check for a terminal resize and reallocate buffers if needed.
    ///
    /// Returns `true` if a resize occurred.
    pub fn check_resize(&mut self) -> io::Result<bool> {
        let new_area = self.backend.size()?;
        if new_area != self.front.area {
            self.front.resize(new_area);
            self.back.resize(new_area);
            return Ok(true);
        }
        Ok(false)
    }

    /// Draw a frame.
    ///
    /// `render_fn` receives a cleared back buffer; the diff against the
    /// previously rendered front buffer is sent to the backend.
    pub fn draw<F>(&mut self, render_fn: F) -> io::Result<()>
    where
        F: FnOnce(&mut Buffer),
    {
        // Check for resize; on resize both buffers are already cleared.
        self.check_resize()?;

        self.back.reset();
        render_fn(&mut self.back);

        let diff = self.back.diff(&self.front);

        self.backend.begin_frame()?;
        self.backend.draw(diff.into_iter())?;
        self.backend.end_frame()?;

        std::mem::swap(&mut self.front, &mut self.back);
        Ok(())
    }

}

/// Read a single crossterm [`Event`], blocking until one arrives.
pub fn read_event() -> io::Result<Event> {
    event::read()
}

/// Poll for a crossterm event with the given timeout.
///
/// Returns `None` if the timeout elapsed without an event.
pub fn poll_event(timeout: std::time::Duration) -> io::Result<Option<Event>> {
    if event::poll(timeout)? {
        Ok(Some(event::read()?))
    } else {
        Ok(None)
    }
}
