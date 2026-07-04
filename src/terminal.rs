// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

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

    /// Clear the physical screen and discard the cached model of it, so the next
    /// [`draw`](Terminal::draw) repaints every cell from scratch.
    ///
    /// Why it exists: `draw` only sends cells that differ from `front`, its record of
    /// what the screen currently shows. If something *outside* mullion writes to the
    /// terminal — suspending to run an interactive subprocess (a passphrase prompt),
    /// resuming after `enter`/`leave`, or returning from SIGTSTP — that record is stale
    /// and the diff would wrongly skip cells it thinks are already correct. Clearing the
    /// screen and blanking `front` makes the next diff emit the whole frame. (This is
    /// the same all-blank-front trick [`check_resize`](Terminal::check_resize) relies on
    /// after a resize, exposed for callers to trigger deliberately.)
    ///
    /// # Errors
    /// Propagates errors from the backend's frame/clear calls.
    pub fn clear(&mut self) -> io::Result<()> {
        self.backend.begin_frame()?;
        self.backend.clear()?;
        self.backend.end_frame()?;
        self.front.reset(); // blank model → next diff re-emits every non-blank cell
        Ok(())
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

/// A background **event reader** that decouples input *capture* from rendering, so a
/// keypress never waits on a slow frame.
///
/// The classic `draw(); poll_event(timeout)` loop has three responsiveness traps under
/// load: input is only checked *after* a slow draw; only one event is handled per
/// frame (a burst drains slowly); and a high-frequency `Mouse`/`Resize` stream starves
/// the keyboard, because each frame's single `poll_event` may return a non-key event.
///
/// `EventReader` fixes all three. A dedicated thread blocks on the terminal's event
/// source and forwards every event over a channel the instant it arrives — so capture
/// is independent of how long `draw` takes. The main loop then [`drain`](Self::drain)s
/// **all** pending events each frame:
///
/// ```no_run
/// use std::time::{Duration, Instant};
/// use mullion::{EventReader, Terminal, backend::CrosstermBackend};
/// use crossterm::event::{Event, KeyCode};
/// # fn demo(term: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> std::io::Result<()> {
/// let input = EventReader::new();
/// let frame = Duration::from_millis(16);
/// loop {
///     let start = Instant::now();
///     for ev in input.drain() {                 // handle EVERY queued event this frame
///         if let Event::Key(k) = ev {
///             if k.code == KeyCode::Char('q') { return Ok(()); }
///         }
///     }
///     term.draw(|buf| { /* ... */ })?;          // even a slow draw can't delay capture
///     std::thread::sleep(frame.saturating_sub(start.elapsed())); // pace the frame
/// }
/// # }
/// ```
///
/// While an `EventReader` exists, read input **only** through it — do not also call
/// [`poll_event`]/[`read_event`], or the two will race for the same events. Dropping it
/// stops and joins the reader thread.
pub struct EventReader {
    rx: Receiver<Event>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EventReader {
    /// Spawn the reader thread.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            // Poll with a timeout so the thread wakes periodically to observe `stop`;
            // an event arriving mid-wait is still returned (and forwarded) at once.
            while !thread_stop.load(Ordering::Relaxed) {
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => match event::read() {
                        Ok(ev) => {
                            if tx.send(ev).is_err() {
                                break; // the receiver was dropped
                            }
                        }
                        Err(_) => break,
                    },
                    Ok(false) => {}  // timed out; loop to re-check `stop`
                    Err(_) => break, // event source closed/unavailable
                }
            }
        });
        Self { rx, stop, handle: Some(handle) }
    }

    /// The next captured event, or `None` if none are queued — never blocks.
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Block up to `timeout` for the next event (e.g. to pace a frame that should
    /// sleep until input), or `None` if it elapses first.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Drain **all** currently-queued events. Handling every one each frame is what
    /// keeps input snappy: a burst is consumed in one frame, and high-frequency mouse
    /// events never starve the keyboard.
    pub fn drain(&self) -> impl Iterator<Item = Event> + '_ {
        std::iter::from_fn(|| self.rx.try_recv().ok())
    }
}

/// The default reader — equivalent to `EventReader::new()`.
impl Default for EventReader {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EventReader {
    /// Signal the reader thread to stop and join it (within one poll interval).
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_reader_starts_idle_and_stops_cleanly() {
        // No synthetic events reach the global source in a headless test, so the queue
        // is empty; the point is that construction, draining, and Drop (which joins the
        // thread) never block or panic.
        let reader = EventReader::new();
        assert!(reader.try_recv().is_none());
        assert_eq!(reader.drain().count(), 0);
    }

    #[test]
    fn clear_forces_a_full_repaint_after_external_corruption() {
        use crate::backend::TestBackend;
        use crate::style::Style;

        let mut term = Terminal::new(TestBackend::new(4, 1)).unwrap();
        let paint = |b: &mut Buffer| {
            b.set_string(0, 0, "HI", Style::default());
        };

        term.draw(paint).unwrap();
        assert_eq!(term.backend().render(), "HI  ");

        // Simulate a subprocess wiping the screen behind mullion's back: the physical
        // surface is now blank, but `front` still models "HI".
        term.backend_mut().clear().unwrap();
        assert_eq!(term.backend().render(), "    ");

        // A plain redraw of identical content cannot restore it — the diff sees no change.
        term.draw(paint).unwrap();
        assert_eq!(term.backend().render(), "    ", "diff wrongly skips unchanged-but-lost cells");

        // clear() discards the stale model, so the next draw repaints every cell.
        term.clear().unwrap();
        term.draw(paint).unwrap();
        assert_eq!(term.backend().render(), "HI  ");
    }
}
