//! # tile-engine
//!
//! A general-purpose terminal UI tiling engine.
//!
//! ## Core concepts
//!
//! ```text
//! ┌──────────┐   draw()    ┌──────────┐   diff    ┌──────────┐
//! │ Widget / │ ──────────► │  Buffer  │ ─────────► │ Terminal │ ──► Backend
//! │ user code│             │ (back)   │            │          │
//! └──────────┘             └──────────┘            └──────────┘
//! ```
//!
//! - **[`Buffer`]** is a 2-D grid of [`Cell`]s.  Widgets write into the *back*
//!   buffer.
//! - **[`Terminal`]** diffs back against front, sends only the changed cells to
//!   the [`Backend`], then swaps buffers.
//! - **[`Backend`]** abstracts over the real terminal
//!   ([`CrosstermBackend`]) or a headless surface ([`TestBackend`]) for tests.
//!
//! ## Wide-grapheme rule
//!
//! When a 2-column-wide grapheme (e.g. `世`, or a full-width emoji) is written
//! at column `x`, column `x+1` becomes a **continuation cell** (empty symbol,
//! skipped on render).  Overwriting either half of a wide pair automatically
//! blanks its partner so no half-glyph is ever visible.

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
