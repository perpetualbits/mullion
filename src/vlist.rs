// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Row virtualization: scroll a windowed view over a [`RecordSource`] without
//! materializing the whole dataset (design note §4.1).
//!
//! ## The window
//!
//! A [`VirtualList`] keeps only a bounded **window** of rows materialized — a
//! contiguous run from the underlying sequence. As the viewport scrolls past the
//! window's edge it fetches the next batch via
//! [`fetch_after`](crate::record::RecordSource::fetch_after) /
//! [`fetch_before`](crate::record::RecordSource::fetch_before) (keyset/seek shape,
//! never an integer offset) and trims the far end so the window never exceeds its
//! capacity. Fetching follows the scroll direction, so a single end-to-end pass
//! materializes each row exactly once and never re-fetches.
//!
//! ## Two scrollbar truths (§6.2)
//!
//! [`scroll_metrics`](VirtualList::scroll_metrics) reports an [`exact`](ScrollMetrics::exact)
//! flag: `true` when the source's
//! [`exact_len`](crate::record::RecordSource::exact_len) is `Some` (the thumb is a
//! true ordinal and known size), `false` over a remote cursor whose length is
//! unknown (the position is an *estimate* from
//! [`approx_position`](crate::record::RecordSource::approx_position)).
//! [`render_scrollbar`] honors that flag by drawing the estimate with a visibly
//! different glyph, so the approximation is shown deliberately rather than faked.
//!
//! ## Selection
//!
//! A `VirtualList` also carries an optional **selection cursor** for the common
//! selection-driven (not wheel-scrolled) admin list. It is anchored to the selected
//! row's **key**, not a window index — the window trims and refills, so an index
//! would not survive. [`select_next`](VirtualList::select_next) /
//! [`select_prev`](VirtualList::select_prev) move it, pulling the adjacent window in
//! and keeping it in the viewport; [`selected`](VirtualList::selected) /
//! [`selected_visible_row`](VirtualList::selected_visible_row) read it for rendering.
//!
//! ## Rendering
//!
//! The list is content-agnostic: it owns geometry and data, not formatting. Pull
//! the on-screen rows with [`visible`](VirtualList::visible) and draw them through
//! a [`ColumnGrid`](crate::table::ColumnGrid) — see `examples/records.rs`.

use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::record::RecordSource;
use crate::style::{Modifier, Style};

// ── VirtualList ──────────────────────────────────────────────────────────────

/// A scrollable window over a [`RecordSource`].
///
/// Construct with [`new`](VirtualList::new); move with [`scroll_by`](VirtualList::scroll_by);
/// read the on-screen slice with [`visible`](VirtualList::visible). The window is
/// kept within [`capacity`](VirtualList::capacity) rows at all times.
pub struct VirtualList<S: RecordSource> {
    /// The backing data source.
    source: S,
    /// The materialized window, in ascending key order.
    rows: Vec<S::Row>,
    /// Index within `rows` of the topmost visible row.
    view_top: usize,
    /// Number of rows the viewport shows.
    viewport: usize,
    /// Maximum rows kept materialized (`>= viewport + 2 * batch`).
    capacity: usize,
    /// Rows fetched per refill.
    batch: usize,
    /// `true` when `rows[0]` is the first row of the source (cannot page up).
    at_top: bool,
    /// `true` when the last row of `rows` is the source's last (cannot page down).
    at_bottom: bool,
    /// The selected row's key — a cursor anchored to a **key**, not a window index,
    /// so it survives the window's trim/refill. `None` only for an empty source.
    selected_key: Option<S::Key>,
}

impl<S: RecordSource> VirtualList<S> {
    /// Create a list showing `viewport` rows, refilling `batch` rows at a time.
    ///
    /// The window capacity is `viewport + 2 * batch` so that after a refill there
    /// is always slack outside the viewport to trim, keeping the window bounded
    /// without ever dropping a visible row. `viewport` and `batch` are each
    /// floored to a minimum of 1. [`set_viewport`](VirtualList::set_viewport) may
    /// later raise the capacity to preserve this margin if the viewport grows.
    ///
    /// The initial window is fetched from the start of the source.
    pub fn new(mut source: S, viewport: usize, batch: usize) -> Self {
        let viewport = viewport.max(1);
        let batch = batch.max(1);
        let capacity = viewport + 2 * batch;

        // Fill the initial window from the beginning, up to capacity.
        let first = source.fetch_after(None, capacity);
        let mut list = Self {
            source,
            rows: first.rows,
            view_top: 0,
            viewport,
            capacity,
            batch,
            at_top: true, // we started at the very beginning
            at_bottom: first.reached_boundary,
            selected_key: None,
        };
        // A source may hand back fewer than asked without reaching the end; make
        // sure the viewport is filled if more rows exist.
        list.fill_below();
        // A fresh list selects its first row — every admin list opens with a cursor.
        list.selected_key = list.rows.first().map(|r| list.source.key_of(r));
        list
    }

    /// The rows currently on screen: at most `viewport` rows from `view_top`.
    pub fn visible(&self) -> &[S::Row] {
        let end = (self.view_top + self.viewport).min(self.rows.len());
        &self.rows[self.view_top..end]
    }

    /// The maximum number of rows the window may hold.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The number of rows the viewport shows.
    pub fn viewport(&self) -> usize {
        self.viewport
    }

    /// `true` when the window's first row is the source's first row.
    pub fn at_top(&self) -> bool {
        self.at_top
    }

    /// `true` when the window's last row is the source's last row.
    pub fn at_bottom(&self) -> bool {
        self.at_bottom
    }

    /// Change the viewport height (e.g. on resize), refilling as needed.
    pub fn set_viewport(&mut self, viewport: usize) {
        self.viewport = viewport.max(1);
        // Capacity must stay ahead of the viewport so trimming has slack.
        self.capacity = self.capacity.max(self.viewport + 2 * self.batch);
        self.fill_below();
        self.clamp_bottom();
    }

    /// Scroll by `delta` rows: positive scrolls **down** (toward later keys),
    /// negative scrolls **up**. Movement is clamped at both ends of the source.
    pub fn scroll_by(&mut self, delta: isize) {
        match delta.cmp(&0) {
            std::cmp::Ordering::Greater => self.scroll_down(delta as usize),
            std::cmp::Ordering::Less => self.scroll_up((-delta) as usize),
            std::cmp::Ordering::Equal => {}
        }
    }

    /// Scroll down by `n` rows, fetching below and trimming the front.
    fn scroll_down(&mut self, n: usize) {
        self.view_top += n;
        self.fill_below();
        self.clamp_bottom();
        self.trim_front();
    }

    /// Scroll up by `n` rows, fetching above and trimming the back.
    fn scroll_up(&mut self, n: usize) {
        // Bring enough rows above `view_top` into the window to move up by `n`.
        while self.view_top < n && !self.at_top {
            let first_key = match self.rows.first() {
                Some(r) => self.source.key_of(r),
                None => break,
            };
            let w = self.source.fetch_before(Some(first_key), self.batch);
            self.at_top = w.reached_boundary;
            let added = w.rows.len();
            if added == 0 {
                self.at_top = true;
                break;
            }
            // Prepend the older rows; everything shifts down by `added`.
            let mut prefixed = w.rows;
            prefixed.append(&mut self.rows);
            self.rows = prefixed;
            self.view_top += added;
        }
        self.view_top = self.view_top.saturating_sub(n);
        self.trim_back();
    }

    /// Fetch forward until the viewport is covered or the source ends.
    fn fill_below(&mut self) {
        while self.view_top + self.viewport > self.rows.len() && !self.at_bottom {
            let last_key = match self.rows.last() {
                Some(r) => self.source.key_of(r),
                None => break, // empty source
            };
            let w = self.source.fetch_after(Some(last_key), self.batch);
            self.at_bottom = w.reached_boundary;
            if w.rows.is_empty() {
                self.at_bottom = true;
                break;
            }
            self.rows.extend(w.rows);
        }
    }

    /// Pin the viewport to the last full screen once the bottom is reached, so
    /// the final row never scrolls past the bottom edge.
    fn clamp_bottom(&mut self) {
        if self.view_top + self.viewport > self.rows.len() {
            self.view_top = self.rows.len().saturating_sub(self.viewport);
        }
    }

    /// Drop rows above the viewport to honor capacity (used after scrolling down).
    /// Never removes a visible row: at most `view_top` rows are eligible.
    fn trim_front(&mut self) {
        let excess = self.rows.len().saturating_sub(self.capacity);
        let drop = excess.min(self.view_top);
        if drop > 0 {
            self.rows.drain(0..drop);
            self.view_top -= drop;
            self.at_top = false; // we discarded the former head
        }
    }

    /// Drop rows below the viewport to honor capacity (used after scrolling up).
    /// Never removes a visible row: only rows past `view_top + viewport` go.
    fn trim_back(&mut self) {
        let excess = self.rows.len().saturating_sub(self.capacity);
        let keep_end = self.view_top + self.viewport;
        let droppable = self.rows.len().saturating_sub(keep_end);
        let drop = excess.min(droppable);
        if drop > 0 {
            self.rows.truncate(self.rows.len() - drop);
            self.at_bottom = false; // we discarded the former tail
        }
    }

    /// Compute the scrollbar position and size, and whether it is exact (§6.2).
    ///
    /// The position is the fraction of the source above the top visible row,
    /// from [`approx_position`](crate::record::RecordSource::approx_position). The
    /// extent (thumb length fraction) is `viewport / total` when the source knows
    /// its length, and a best-effort proxy otherwise. `exact` mirrors whether
    /// [`exact_len`](crate::record::RecordSource::exact_len) returned `Some`.
    pub fn scroll_metrics(&mut self) -> ScrollMetrics {
        let total = self.source.exact_len();
        let position = match self.rows.get(self.view_top) {
            Some(r) => {
                let k = self.source.key_of(r);
                self.source.approx_position(&k).unwrap_or(0.0)
            }
            None => 0.0,
        };
        let extent = match total {
            Some(t) if t > 0 => (self.viewport as f32 / t as f32).clamp(0.0, 1.0),
            // Unknown length: if both ends are in view the whole set is shown;
            // otherwise fall back to a small marker the renderer styles as an
            // estimate.
            _ if self.at_top && self.at_bottom => 1.0,
            _ => 0.0,
        };
        ScrollMetrics { position: position.clamp(0.0, 1.0), extent, exact: total.is_some() }
    }

    /// Borrow the underlying source.
    pub fn source(&self) -> &S {
        &self.source
    }
}

// ── Selection (key-anchored cursor) ────────────────────────────────────────────

/// A key-anchored **selection cursor** on top of the scroll window.
///
/// Selection-driven admin lists (an LDAP user list, an IP table) move a highlighted
/// row with `j`/`k` and act on it with Enter, rather than wheel-scrolling. The
/// selection cannot be a window index because the window trims and refills as it
/// scrolls, so it is anchored to the row's **key** and stays valid across refills.
/// `select_next`/`select_prev` move it, pulling the adjacent window in with the same
/// `fetch_after`/`fetch_before` the scroll path uses so the cursor never leaves the
/// materialized window, and keep it inside the viewport (the
/// [`visible_window`](crate::visible_window) policy applied to the key-anchored
/// cursor). Requires the key to be comparable and cloneable.
impl<S: RecordSource> VirtualList<S>
where
    S::Key: Clone + PartialEq,
{
    /// The selected row's key, if any. Selection is by **key** (stable across window
    /// trim/refill), never a window index.
    pub fn selected_key(&self) -> Option<&S::Key> {
        self.selected_key.as_ref()
    }

    /// The window index of the selected row, if it is currently materialized.
    fn selected_index(&self) -> Option<usize> {
        let key = self.selected_key.as_ref()?;
        self.rows.iter().position(|r| self.source.key_of(r) == *key)
    }

    /// The selected row itself, if it is in the materialized window.
    pub fn selected(&self) -> Option<&S::Row> {
        self.selected_index().map(|i| &self.rows[i])
    }

    /// The selected row's offset within [`visible`](VirtualList::visible), for
    /// drawing the highlight — `None` when the selection is outside the viewport.
    pub fn selected_visible_row(&self) -> Option<usize> {
        let i = self.selected_index()?;
        (i >= self.view_top && i < self.view_top + self.viewport).then_some(i - self.view_top)
    }

    /// Move the selection one row **down**, fetching across the window's bottom edge
    /// as needed and keeping the selected row in the viewport.
    ///
    /// Returns `false` at the source's last row (so a form can move focus onward),
    /// mirroring `line_edit`/`textarea_edit`.
    pub fn select_next(&mut self) -> bool {
        let Some(idx) = self.selected_index() else {
            return self.anchor_selection_top();
        };
        // Need a row after `idx`. If it is the window's tail, force one batch in.
        if idx + 1 >= self.rows.len() {
            if self.at_bottom {
                return false;
            }
            self.force_fetch_below();
            if idx + 1 >= self.rows.len() {
                return false; // source ended exactly here
            }
        }
        self.selected_key = Some(self.source.key_of(&self.rows[idx + 1]));
        self.keep_selection_in_view();
        true
    }

    /// Move the selection one row **up** — symmetric to [`select_next`](VirtualList::select_next).
    pub fn select_prev(&mut self) -> bool {
        let Some(idx) = self.selected_index() else {
            return self.anchor_selection_top();
        };
        if idx == 0 {
            if self.at_top {
                return false;
            }
            self.force_fetch_above(); // prepends; the old row 0 shifts down
            let Some(idx) = self.selected_index() else { return false };
            if idx == 0 {
                return false; // nothing actually came in
            }
            self.selected_key = Some(self.source.key_of(&self.rows[idx - 1]));
        } else {
            self.selected_key = Some(self.source.key_of(&self.rows[idx - 1]));
        }
        self.keep_selection_in_view();
        true
    }

    /// Move the selection by `delta` rows (`+` down, `−` up), clamped at the source
    /// boundary. Returns `false` when it could not move at all (already at the end).
    pub fn select_page(&mut self, delta: isize) -> bool {
        if delta == 0 {
            return false;
        }
        let mut moved = false;
        for _ in 0..delta.unsigned_abs() {
            let stepped = if delta > 0 { self.select_next() } else { self.select_prev() };
            if stepped {
                moved = true;
            } else {
                break; // hit the boundary
            }
        }
        moved
    }

    /// Seek so `key` is selected and visible (jump-to). No-op if `key` is absent.
    ///
    /// How: fetch one row *before* `key` to find its predecessor, then fetch forward
    /// from that predecessor — that window begins with `key` itself when it exists.
    /// If it does not (the first fetched key differs), nothing changes.
    pub fn select_key(&mut self, key: &S::Key) {
        let before = self.source.fetch_before(Some(key.clone()), 1);
        let window = match before.rows.last() {
            Some(r) => {
                let pk = self.source.key_of(r);
                self.source.fetch_after(Some(pk), self.capacity)
            }
            None => self.source.fetch_after(None, self.capacity),
        };
        if window.rows.first().map(|r| self.source.key_of(r)).as_ref() != Some(key) {
            return; // `key` is not in the source
        }
        self.at_top = before.rows.is_empty(); // no predecessor ⇒ it is the first row
        self.at_bottom = window.reached_boundary;
        self.rows = window.rows;
        self.view_top = 0;
        self.selected_key = Some(key.clone());
        self.fill_below();
        self.clamp_bottom();
    }

    /// Select the window's top row — used to recover a selection that scrolling has
    /// pushed out of the materialized window.
    fn anchor_selection_top(&mut self) -> bool {
        match self.rows.first() {
            Some(r) => {
                self.selected_key = Some(self.source.key_of(r));
                self.keep_selection_in_view();
                true
            }
            None => false,
        }
    }

    /// Slide the viewport the minimum needed to show the selected row, then keep the
    /// window filled and within capacity. Trims from whichever end has slack; never
    /// drops a visible or selected row (both lie inside the viewport afterwards).
    fn keep_selection_in_view(&mut self) {
        if let Some(idx) = self.selected_index() {
            crate::geometry::visible_window(idx, &mut self.view_top, self.rows.len(), self.viewport);
        }
        self.fill_below();
        self.clamp_bottom();
        self.trim_front();
        self.trim_back();
    }

    /// Force one batch of rows in from below even if the viewport is already covered
    /// — used to step the selection past the window's materialized tail.
    fn force_fetch_below(&mut self) {
        let Some(r) = self.rows.last() else { return };
        let last = self.source.key_of(r);
        let w = self.source.fetch_after(Some(last), self.batch);
        self.at_bottom = w.reached_boundary;
        if w.rows.is_empty() {
            self.at_bottom = true;
        } else {
            self.rows.extend(w.rows);
        }
    }

    /// Force one batch of rows in from above (prepending), shifting `view_top` down.
    fn force_fetch_above(&mut self) {
        let Some(r) = self.rows.first() else { return };
        let first = self.source.key_of(r);
        let w = self.source.fetch_before(Some(first), self.batch);
        self.at_top = w.reached_boundary;
        let added = w.rows.len();
        if added == 0 {
            self.at_top = true;
        } else {
            let mut prefixed = w.rows;
            prefixed.append(&mut self.rows);
            self.rows = prefixed;
            self.view_top += added;
        }
    }
}

// ── ScrollMetrics ────────────────────────────────────────────────────────────

/// The scrollbar geometry derived from a [`VirtualList`], plus its honesty flag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollMetrics {
    /// Fraction of the source above the top visible row, in `[0, 1]`.
    pub position: f32,
    /// Thumb length as a fraction of the track, in `[0, 1]`. `0` means "unknown
    /// size — draw a position-only marker" (only ever the case when `!exact`).
    pub extent: f32,
    /// `true` when the source reported an exact length: the thumb is a true
    /// ordinal. `false` means position is an estimate and must be shown as one.
    pub exact: bool,
}

impl ScrollMetrics {
    /// Metrics for a plain, fully in-memory list of `total` rows showing `viewport`
    /// rows from `offset` — the common non-virtualized case (a `ListCursor` +
    /// [`visible_window`](crate::visible_window) screen), as a one-liner.
    ///
    /// Always [`exact`](ScrollMetrics::exact): an in-memory list knows its length.
    /// `position = offset / total`, `extent = viewport / total`, both clamped to
    /// `[0, 1]`; an empty list yields a full thumb at the top.
    #[must_use]
    pub fn from_window(offset: usize, viewport: usize, total: usize) -> ScrollMetrics {
        if total == 0 {
            return ScrollMetrics { position: 0.0, extent: 1.0, exact: true };
        }
        ScrollMetrics {
            position: (offset as f32 / total as f32).clamp(0.0, 1.0),
            extent: (viewport as f32 / total as f32).clamp(0.0, 1.0),
            exact: true,
        }
    }
}

// ── Scrollbar rendering ──────────────────────────────────────────────────────

/// Which gutter a vertical scrollbar occupies for a base direction (§round-2 A5):
/// [`Left`](crate::label::Side::Left) under RTL, [`Right`](crate::label::Side::Right)
/// otherwise — so the bar sits on the trailing edge of the reading direction.
pub fn scrollbar_side(base: crate::text::BaseDirection) -> crate::label::Side {
    match base {
        crate::text::BaseDirection::Rtl => crate::label::Side::Left,
        _ => crate::label::Side::Right,
    }
}

/// Draw a scrollbar for `metrics` into `rect`. The **orientation follows the rect
/// shape**: a wider-than-tall rect draws a horizontal bar (track `─`), otherwise a
/// vertical one (track `│`) — so the same widget serves a row scrollbar and either
/// axis of a 2-D graph viewport (§5.7).
///
/// The thumb sits at `metrics.position` and spans `metrics.extent` of the track
/// (at least one cell). When `metrics.exact` is `false` the thumb uses a
/// **lighter shade glyph** (`▒` instead of `█`) so an estimated position is
/// visibly distinct from a true one (§6.2). `style` colors both the thumb and the
/// track; the track is drawn in that color with a [`Modifier::DIM`] so it recedes
/// behind the thumb.
///
/// Does nothing for a zero-size rect.
pub fn render_scrollbar(buf: &mut Buffer, rect: Rect, metrics: ScrollMetrics, style: Style) {
    if rect.height == 0 || rect.width == 0 {
        return;
    }
    let horizontal = rect.width > rect.height;
    let track_len = if horizontal { rect.width } else { rect.height } as usize;
    let track_glyph = if horizontal { "─" } else { "│" };
    // Exact thumbs are solid; estimated thumbs use a shade so the eye reads them
    // as approximate rather than precise.
    let thumb_glyph = if metrics.exact { "█" } else { "▒" };

    // Thumb length in cells: at least 1, at most the whole track.
    let thumb_len = ((metrics.extent * track_len as f32).round() as usize).clamp(1, track_len);
    // Start of the thumb: position scaled over the free travel of the track.
    let travel = track_len - thumb_len;
    let thumb_start = ((metrics.position * travel as f32).round() as usize).min(travel);

    for i in 0..track_len {
        let in_thumb = i >= thumb_start && i < thumb_start + thumb_len;
        let (glyph, st) = if in_thumb {
            (thumb_glyph, style)
        } else {
            // The track recedes behind the thumb: same color, dimmed.
            (track_glyph, style.add_modifier(Modifier::DIM))
        };
        let (x, y) = if horizontal {
            (rect.x + i as u16, rect.y)
        } else {
            (rect.x, rect.y + i as u16)
        };
        buf.set_grapheme(x, y, glyph, st);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::VecRecordSource;

    /// A source of `n` rows keyed `0..n` with a string value, exact length.
    fn source(n: u64) -> VecRecordSource<u64, String> {
        VecRecordSource::new((0..n).map(|k| (k, format!("row{k}"))).collect())
    }

    /// The keys currently visible, for concise assertions.
    fn visible_keys<S: RecordSource<Key = u64, Row = (u64, String)>>(
        list: &VirtualList<S>,
    ) -> Vec<u64> {
        list.visible().iter().map(|(k, _)| *k).collect()
    }

    #[test]
    fn initial_window_starts_at_top() {
        let list = VirtualList::new(source(100), 5, 4);
        assert_eq!(visible_keys(&list), vec![0, 1, 2, 3, 4]);
        assert!(list.at_top());
        assert!(!list.at_bottom());
    }

    #[test]
    fn scroll_down_advances_and_fetches() {
        let mut list = VirtualList::new(source(100), 5, 4);
        list.scroll_by(10);
        assert_eq!(visible_keys(&list), vec![10, 11, 12, 13, 14]);
        assert!(!list.at_top());
    }

    #[test]
    fn scroll_clamps_at_bottom() {
        let mut list = VirtualList::new(source(20), 5, 4);
        list.scroll_by(1000); // far past the end
        assert_eq!(visible_keys(&list), vec![15, 16, 17, 18, 19]);
        assert!(list.at_bottom());
        // Scrolling further down is a no-op.
        list.scroll_by(5);
        assert_eq!(visible_keys(&list), vec![15, 16, 17, 18, 19]);
    }

    #[test]
    fn scroll_up_clamps_at_top() {
        let mut list = VirtualList::new(source(100), 5, 4);
        list.scroll_by(40);
        list.scroll_by(-1000);
        assert_eq!(visible_keys(&list), vec![0, 1, 2, 3, 4]);
        assert!(list.at_top());
    }

    #[test]
    fn round_trip_returns_to_same_rows() {
        let mut list = VirtualList::new(source(200), 6, 5);
        list.scroll_by(120);
        let mid = visible_keys(&list);
        list.scroll_by(-50);
        list.scroll_by(50);
        assert_eq!(visible_keys(&list), mid);
    }

    #[test]
    fn smaller_than_viewport_dataset() {
        let list = VirtualList::new(source(3), 10, 4);
        assert_eq!(visible_keys(&list), vec![0, 1, 2]);
        assert!(list.at_top() && list.at_bottom());
    }

    #[test]
    fn empty_source_is_inert() {
        let mut list = VirtualList::new(source(0), 5, 4);
        assert!(list.visible().is_empty());
        list.scroll_by(10);
        list.scroll_by(-10);
        assert!(list.visible().is_empty());
    }

    #[test]
    fn metrics_exact_vs_estimate() {
        let mut exact = VirtualList::new(source(100), 10, 8);
        let m = exact.scroll_metrics();
        assert!(m.exact);
        assert!((m.position - 0.0).abs() < 1e-6);
        assert!((m.extent - 0.1).abs() < 1e-6); // 10 / 100

        let est_src = VecRecordSource::new((0..100u64).map(|k| (k, ())).collect()).estimated();
        let mut est = VirtualList::new(est_src, 10, 8);
        assert!(!est.scroll_metrics().exact);
    }

    // ── Selection (R1) ────────────────────────────────────────────────────

    #[test]
    fn selection_starts_at_top() {
        let list = VirtualList::new(source(100), 5, 4);
        assert_eq!(list.selected_key(), Some(&0));
        assert_eq!(list.selected().map(|(k, _)| *k), Some(0));
        assert_eq!(list.selected_visible_row(), Some(0));
    }

    #[test]
    fn select_next_moves_and_keeps_selection_in_viewport() {
        let mut list = VirtualList::new(source(100), 5, 4);
        for expected in 1..=6u64 {
            assert!(list.select_next());
            assert_eq!(list.selected_key(), Some(&expected));
            let vr = list.selected_visible_row().expect("selection is visible");
            assert!(vr < list.viewport());
            assert_eq!(list.visible()[vr].0, expected); // that visible row IS the selection
        }
        // The viewport slid to keep the cursor visible (5-row viewport, cursor at 6).
        assert!(list.visible()[0].0 > 0);
    }

    #[test]
    fn select_prev_at_top_returns_false() {
        let mut list = VirtualList::new(source(100), 5, 4);
        assert!(!list.select_prev());
        assert_eq!(list.selected_key(), Some(&0));
    }

    #[test]
    fn select_next_stops_at_bottom() {
        let mut list = VirtualList::new(source(3), 5, 4);
        assert!(list.select_next()); // 0 → 1
        assert!(list.select_next()); // 1 → 2
        assert!(!list.select_next()); // last row: no move
        assert_eq!(list.selected_key(), Some(&2));
    }

    #[test]
    fn select_crosses_the_window_edge_over_a_big_source() {
        // Far larger than capacity: stepping fetches across the edge, keeps the
        // window bounded, and the selection stays materialized throughout.
        let mut list = VirtualList::new(source(1000), 4, 3);
        for _ in 0..500 {
            assert!(list.select_next());
            assert!(list.selected().is_some());
            assert!(list.rows.len() <= list.capacity());
        }
        assert_eq!(list.selected_key(), Some(&500));
        for _ in 0..500 {
            assert!(list.select_prev());
            assert!(list.rows.len() <= list.capacity());
        }
        assert_eq!(list.selected_key(), Some(&0));
        assert!(list.at_top());
    }

    #[test]
    fn select_key_jumps_and_ignores_absent() {
        let mut list = VirtualList::new(source(1000), 5, 4);
        list.select_key(&742);
        assert_eq!(list.selected_key(), Some(&742));
        assert_eq!(list.selected().map(|(k, _)| *k), Some(742));
        assert_eq!(list.selected_visible_row(), Some(0)); // seeked to the top
        list.select_key(&99_999); // absent
        assert_eq!(list.selected_key(), Some(&742)); // unchanged
    }

    #[test]
    fn select_page_moves_and_clamps() {
        let mut list = VirtualList::new(source(100), 5, 4);
        assert!(list.select_page(10));
        assert_eq!(list.selected_key(), Some(&10));
        assert!(list.select_page(-1000)); // clamps to top, but it did move
        assert_eq!(list.selected_key(), Some(&0));
        assert!(!list.select_page(-1)); // already at the top
        assert!(!list.select_page(0));
    }

    // ── ScrollMetrics::from_window (R3) ────────────────────────────────────

    #[test]
    fn scroll_metrics_from_window() {
        let m = ScrollMetrics::from_window(0, 10, 100);
        assert!(m.exact);
        assert!((m.position - 0.0).abs() < 1e-6);
        assert!((m.extent - 0.1).abs() < 1e-6);
        assert!((ScrollMetrics::from_window(90, 10, 100).position - 0.9).abs() < 1e-6);
        // Empty list: a full thumb at the top.
        assert_eq!(ScrollMetrics::from_window(0, 10, 0), ScrollMetrics { position: 0.0, extent: 1.0, exact: true });
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// Stitched windows are contiguous and gap-free: the upper half fetched
        /// after the last key of the lower half joins it with no gap or overlap,
        /// reconstructing the full sorted sequence. This is exactly how
        /// `VirtualList` pages across a boundary.
        #[test]
        fn prop_windows_stitch_contiguously(n in 1u64..200, split in 0u64..200) {
            let mut src = source(n);
            let boundary = split % n; // a valid key in 0..n
            // Lower half: every key strictly below the boundary.
            let before = src.fetch_before(Some(boundary), n as usize).rows;
            let before_keys: Vec<u64> = before.iter().map(|(k, _)| *k).collect();
            // Upper half: fetch after the last lower key (or from the start when
            // the lower half is empty) — the seek the consumer would issue next.
            let after = match before.last() {
                Some((k, _)) => src.fetch_after(Some(*k), n as usize).rows,
                None => src.fetch_after(None, n as usize).rows,
            };
            let after_keys: Vec<u64> = after.iter().map(|(k, _)| *k).collect();

            // No overlap: the halves meet exactly at the boundary.
            prop_assert_eq!(before_keys.len() as u64, boundary);
            prop_assert_eq!(after_keys.first().copied(), Some(boundary));
            // No gap: concatenation is the whole sequence.
            let stitched: Vec<u64> = before_keys.into_iter().chain(after_keys).collect();
            prop_assert_eq!(stitched, (0..n).collect::<Vec<_>>());
        }

        /// A full top→bottom scroll materializes every row exactly once: the set
        /// of all visible keys seen across the pass is exactly `0..n`.
        #[test]
        fn prop_full_scroll_materializes_each_row(n in 1u64..150, vp in 1usize..12) {
            let mut list = VirtualList::new(source(n), vp, 5);
            let mut seen = std::collections::BTreeSet::new();
            // Step one row at a time until the bottom is anchored.
            for _ in 0..(n as usize + vp + 5) {
                for (k, _) in list.visible() {
                    seen.insert(*k);
                }
                if list.at_bottom() && visible_keys(&list).last() == Some(&(n - 1)) {
                    break;
                }
                list.scroll_by(1);
            }
            prop_assert_eq!(seen, (0..n).collect::<std::collections::BTreeSet<_>>());
        }

        /// The materialized window never exceeds the configured capacity, however
        /// far we scroll in either direction.
        #[test]
        fn prop_window_within_capacity(n in 1u64..300, vp in 1usize..10, moves in prop::collection::vec(-30isize..30, 0..40)) {
            let mut list = VirtualList::new(source(n), vp, 6);
            prop_assert!(list.rows.len() <= list.capacity());
            for d in moves {
                list.scroll_by(d);
                prop_assert!(list.rows.len() <= list.capacity(),
                    "window {} exceeded capacity {}", list.rows.len(), list.capacity());
                // The viewport is always backed by real rows (unless dataset < vp).
                prop_assert!(list.view_top + list.viewport <= list.rows.len()
                    || list.rows.len() < list.viewport);
            }
        }
    }
}
