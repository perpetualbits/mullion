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
//! ## Rendering
//!
//! The list is content-agnostic: it owns geometry and data, not formatting. Pull
//! the on-screen rows with [`visible`](VirtualList::visible) and draw them through
//! a [`ColumnGrid`](crate::table::ColumnGrid) — see `examples/records.rs`.

use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::record::RecordSource;
use crate::style::Style;

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
}

impl<S: RecordSource> VirtualList<S> {
    /// Create a list showing `viewport` rows, refilling `batch` rows at a time.
    ///
    /// The window capacity is `max(viewport + 2 * batch, …)` so that after a
    /// refill there is always slack outside the viewport to trim, keeping the
    /// window bounded without ever dropping a visible row. `viewport` and `batch`
    /// are floored at 1.
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
        };
        // A source may hand back fewer than asked without reaching the end; make
        // sure the viewport is filled if more rows exist.
        list.fill_below();
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

// ── Scrollbar rendering ──────────────────────────────────────────────────────

/// Draw a vertical scrollbar for `metrics` into the (1-column-wide) `rect`.
///
/// The track is drawn faintly; the thumb sits at `metrics.position` and spans
/// `metrics.extent` of the track (at least one cell). When `metrics.exact` is
/// `false` the thumb uses a **lighter shade glyph** (`▒` instead of `█`) so an
/// estimated position is visibly distinct from a true one (§6.2). `style` colors
/// the thumb; the track is drawn in the same color but dimmed via the track glyph.
///
/// Does nothing for a zero-height rect.
pub fn render_scrollbar(buf: &mut Buffer, rect: Rect, metrics: ScrollMetrics, style: Style) {
    if rect.height == 0 || rect.width == 0 {
        return;
    }
    let h = rect.height as usize;
    let track_glyph = "│";
    // Exact thumbs are solid; estimated thumbs use a shade so the eye reads them
    // as approximate rather than precise.
    let thumb_glyph = if metrics.exact { "█" } else { "▒" };

    // Thumb length in cells: at least 1, at most the whole track.
    let thumb_len = ((metrics.extent * h as f32).round() as usize).clamp(1, h);
    // Top of the thumb: position scaled over the free travel of the track.
    let travel = h - thumb_len;
    let thumb_top = (metrics.position * travel as f32).round() as usize;
    let thumb_top = thumb_top.min(travel);

    for i in 0..h {
        let y = rect.y + i as u16;
        let in_thumb = i >= thumb_top && i < thumb_top + thumb_len;
        let (glyph, st) = if in_thumb {
            (thumb_glyph, style)
        } else {
            (track_glyph, style)
        };
        buf.set_grapheme(rect.x, y, glyph, st);
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
