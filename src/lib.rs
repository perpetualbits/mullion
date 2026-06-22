// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! # mullion
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
//! use mullion::{Terminal, backend::CrosstermBackend, style::Style};
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
pub mod border;
pub mod table;
pub mod buffer;
pub mod capabilities;
pub mod charset;
pub mod ease;
pub mod float;
pub mod geometry;
pub mod input;
pub mod junction;
pub mod label;
pub mod layout;
pub mod mouse;
pub mod record;
pub mod render;
pub mod style;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod tree;

pub use backend::{Backend, CrosstermBackend, TestBackend};
pub use label::{draw_label, label_period, Align, Label, Side};
pub use border::{draw_box, frame_tiles, render_shared, BorderGap, BorderStyle, Borders, CornerStyle, LineWeight};
pub use buffer::{Buffer, Cell};
pub use geometry::Rect;
pub use input::{InputRouter, Keymap, KeyCode, KeyEvent, KeyModifiers, KeyOutcome, MouseButton, MouseEvent, MouseEventKind, MouseOutcome, NavCommand};
pub use mouse::{carousel_at, tile_at};
pub use layout::{carousel_visible_range, region_of, Constraint, Node, Orientation, Size, TileId};
pub use capabilities::Capabilities;
pub use charset::box_to_ascii;
pub use ease::{gaussian, lerp, smoothstep};
pub use float::{
    free_cells_in_window, free_intervals_in_rows, FloatChild, FloatLayer, FloatRect, FreeInterval,
};
pub use record::{RecordSource, Window};
pub use table::{ColumnDef, ColumnGrid, ColumnKind, Table};
pub use text::{
    render_line, render_wrapped, shape_line, wrap, BaseDirection, CursorMap, VisualCell,
    VisualLine, WrappedText,
};
pub use style::{Color, ColorDepth, Modifier, Style};
pub use theme::Theme;
pub use terminal::{poll_event, read_event, Terminal};
pub use render::render_carousel;
pub use tree::{
    focus_override, focus_path, id_from_key, leaves, node_by_id, node_by_id_mut, node_id,
    reconcile_carousel, reconcile_split, tile_id_of, Dir, Direction, Tree,
};
