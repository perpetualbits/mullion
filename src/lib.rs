//! # tile-engine
//!
//! A general-purpose, reusable terminal UI tiling engine.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────┐  render_fn  ┌──────────┐   diff    ┌──────────┐
//! │ Widget / │ ──────────► │  Buffer  │ ─────────► │ Terminal │ ──► Backend
//! │ user code│             │ (back)   │            │          │
//! └──────────┘             └──────────┘            └──────────┘
//! ```
//!
//! - **[`Buffer`]** — a 2-D grid of [`Cell`]s that widgets write into.  Every
//!   frame the caller receives a cleared *back* buffer and fills it in.
//! - **[`Terminal`]** — diffs the new back buffer against the previously rendered
//!   *front* buffer, sends only the changed cells to the [`Backend`], and then
//!   swaps the two buffers.
//! - **[`Backend`]** — abstracts over the output target.  [`CrosstermBackend`]
//!   drives a real terminal; [`TestBackend`] is a headless surface for tests.
//!
//! ## Wide-grapheme rule
//!
//! When a 2-column-wide grapheme (e.g. `世`, or a full-width emoji) is written
//! at column `x`, column `x+1` becomes a **continuation cell** (empty `symbol`
//! string, skipped by the renderer).  Overwriting either half of a wide pair
//! automatically blanks its partner so no half-glyph is ever visible.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::io;
//! use tile_engine::{Terminal, backend::CrosstermBackend, style::Style};
//!
//! let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
//! term.enter()?;
//! term.draw(|buf| {
//!     buf.set_string(0, 0, "Hello, terminal!", Style::default());
//! })?;
//! term.leave()?;
//! # Ok::<(), io::Error>(())
//! ```

pub mod backend;
pub mod buffer;
pub mod geometry;
pub mod style;
pub mod terminal;

pub use backend::{Backend, CrosstermBackend, TestBackend};
pub use buffer::{Buffer, Cell};
pub use geometry::Rect;
pub use style::{Color, Modifier, Style};
pub use terminal::{poll_event, read_event, Terminal};
