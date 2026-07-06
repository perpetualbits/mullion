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
pub mod buffer;
pub mod capabilities;
pub mod charset;
pub mod colorfield;
pub mod curve_map;
pub mod diff;
pub mod docview;
pub mod ease;
pub mod edit;
pub mod field;
pub mod float;
pub mod form;
pub mod geometry;
pub mod graph;
pub mod input;
pub mod junction;
pub mod label;
pub mod layout;
pub mod mouse;
pub mod outline;
pub mod panel;
pub mod record;
pub mod refine;
pub mod render;
pub mod route;
pub mod runaround;
pub mod socket;
pub mod spacefill;
pub mod style;
pub mod sugiyama;
pub mod table;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod tree;
pub mod video;
pub mod vlist;
pub mod zoom;

pub use backend::{Backend, CrosstermBackend, TestBackend};
pub use border::{
    draw_box, frame_tiles, render_rim, render_shared, BorderGap, BorderStyle, Borders, CornerStyle,
    LineWeight,
};
pub use buffer::{Buffer, Cell};
pub use capabilities::Capabilities;
pub use charset::box_to_ascii;
pub use colorfield::{Flame, Palette, Reaction, Wave};
pub use diff::{diff_lines, render_diff_unified, DiffOp};
pub use docview::{render_doc, DocView};
pub use ease::{gaussian, lerp, smoothstep};
pub use edit::{line_edit, render_field, render_textarea, textarea_edit, FieldRender};
pub use field::{Field, ASCII_RAMP, BLOCK_RAMP};
pub use float::{
    free_cells_in_window, free_intervals_in_rows, FloatChild, FloatLayer, FloatRect, FreeInterval,
};
pub use form::{focus_step, render_validity, FormLayout, FormRow, Validity};
pub use geometry::{mirror_rects_in, visible_window, Rect};
pub use graph::{GraphCanvas, Viewport};
pub use input::{
    InputRouter, KeyCode, KeyEvent, KeyModifiers, KeyOutcome, Keymap, MouseButton, MouseEvent,
    MouseEventKind, MouseOutcome, NavCommand,
};
pub use label::{draw_label, label_period, Align, Anchor, Label, Side};
pub use layout::{carousel_visible_range, region_of, Constraint, Node, Orientation, Size, TileId};
pub use mouse::{carousel_at, tile_at};
pub use outline::{render_more_row, render_tree_row, tree_prefix};
pub use panel::{draw_panel, render_keyhints, Panel};
pub use record::{RangeSource, RecordSource, VecRecordSource, Window};
pub use refine::{
    anneal, learn_weights, refine, score, AnnealParams, LayoutScore, Preference, ScoreWeights,
};
pub use render::render_carousel;
pub use route::{render as render_connectors, route, route_all, Connector, RouteRequest};
pub use runaround::{flow, render_flow, slots_in, PlacedLine, Slot};
pub use socket::{bookends, draw_socket, Flow, FlowStyle, Socket};
pub use style::{Color, ColorDepth, Modifier, Style};
pub use sugiyama::{assign_layers, auto_layout, crossings, order_layers, LayerDir, SugiyamaParams};
pub use table::{ColumnDef, ColumnGrid, ColumnKind, Table};
pub use terminal::{poll_event, read_event, EventReader, Terminal};
pub use text::{
    caret_from_visual_col, caret_visual_col, elide, render_line, render_line_selected,
    render_wrapped, selection_step, shape_digits, shape_line, visual_step, wrap, wrap_into_slots,
    BaseDirection, CursorMap, DigitShaping, TextCtx, VisualCell, VisualLine, WrappedText,
};
pub use theme::Theme;
pub use tree::{
    focus_override, focus_path, id_from_key, leaves, node_by_id, node_by_id_mut, node_id,
    reconcile_carousel, reconcile_split, tile_id_of, Dir, Direction, Tree,
};
pub use video::{Dither, Encoding, Filter, Frame, Rgb, Sampling, Video};
pub use vlist::{render_scrollbar, scrollbar_side, ScrollMetrics, VirtualList};
pub use zoom::{lerp_rect, FocusTarget, Lod, LodScale, Zoom};
