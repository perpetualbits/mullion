// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The seek-shaped data-provider trait for row virtualization (design note §4.1).
//!
//! This module defines the *contract* a backing store must satisfy to be scrolled
//! through without materializing all of its rows.  It is defined **here, with the
//! floating-tile foundation**, rather than retrofitted alongside the consumer
//! (Phase 3), so the rest of the engine can be built against a stable shape from
//! the start.  No implementations live here yet — only the trait, the [`Window`]
//! it returns, and the reasoning that fixes the shape.
//!
//! ## Why seek, not offset
//!
//! The real backends do **not** offer cheap random access:
//!
//! - In SQL, `OFFSET 750000` is O(n) — the engine still walks the skipped rows.
//! - LDAP has no native offset at all.
//!
//! So the trait is **seek / keyset-shaped**: every fetch is anchored to a *key*
//! and asks for the rows immediately after or before it.  This maps directly onto
//!
//! - SQL **keyset pagination** — `WHERE key > ? ORDER BY key LIMIT n`, and
//! - LDAP's **VLV control** — designed precisely to answer "give me `n` entries
//!   around offset X%".
//!
//! ## Scrollbar honesty
//!
//! Over a remote cursor the total length is usually unknown, so a scrollbar thumb
//! is an *estimate* from [`approx_position`](RecordSource::approx_position), not a
//! true ordinal — unless [`exact_len`](RecordSource::exact_len) cheaply returns
//! `Some`.  The contract makes the distinction explicit (§6.2) so a consumer can
//! render the estimate *as* an estimate rather than fake precision; the future
//! Phase 3 list view is responsible for honoring it.

/// A contiguous run of rows fetched from a [`RecordSource`], plus whether the
/// fetch reached the source's boundary.
///
/// Returned by [`RecordSource::fetch_after`] and
/// [`RecordSource::fetch_before`].  The rows are in the source's key order
/// (ascending), regardless of fetch direction: `fetch_before` returns the rows
/// *preceding* an anchor, still ordered low-to-high, so a consumer can stitch
/// windows together without re-sorting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window<Row> {
    /// The fetched rows, in ascending key order.  May be shorter than the
    /// requested count when the boundary is reached, and empty when the source is
    /// exhausted in the fetch direction.
    pub rows: Vec<Row>,
    /// `true` when this fetch reached the end of the source in its own direction
    /// — for `fetch_after`, the last row of the source; for `fetch_before`, the
    /// first.  Lets a consumer stop paging without a separate length query.
    pub reached_boundary: bool,
}

impl<Row> Window<Row> {
    /// Construct a window from its rows and boundary flag.
    pub fn new(rows: Vec<Row>, reached_boundary: bool) -> Self {
        Self { rows, reached_boundary }
    }

    /// An empty window that has reached the boundary — the natural "no more rows"
    /// result.
    pub fn empty() -> Self {
        Self { rows: Vec::new(), reached_boundary: true }
    }

    /// Number of rows in the window.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` when the window carries no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// A windowed, seek-addressed source of ordered rows.
///
/// Implementors back a virtual list (Phase 3): the view keeps only a small window
/// of rows materialized and calls [`fetch_after`](RecordSource::fetch_after) /
/// [`fetch_before`](RecordSource::fetch_before) as the viewport moves.  All
/// addressing is by **key**, never by integer offset — see the module docs for
/// why offset addressing is rejected.
///
/// # Key ordering
/// Keys are expected to form a total order matching the row order the source
/// returns (ascending).  "Immediately after `key`" therefore means the smallest
/// key strictly greater than `key`; "immediately before" the largest strictly
/// less.  Implementors that cannot guarantee a stable order cannot satisfy the
/// stitching contract the consumer relies on.
///
/// # Why `&mut self`
/// A remote source typically holds a live cursor or connection that a fetch
/// advances or mutates, so every method takes `&mut self` rather than pretending
/// the source is pure.
pub trait RecordSource {
    /// The ordering key that anchors a fetch (e.g. a primary key or LDAP DN).
    type Key;
    /// One materialized row handed back to the consumer.
    type Row;

    /// Fetch up to `n` rows whose key is immediately **after** `key`, in
    /// ascending key order.
    ///
    /// Passing `key = None` starts from the beginning of the source.  The
    /// returned [`Window`] may hold fewer than `n` rows (and sets
    /// `reached_boundary`) when the end is reached.
    fn fetch_after(&mut self, key: Option<Self::Key>, n: usize) -> Window<Self::Row>;

    /// Fetch up to `n` rows whose key is immediately **before** `key`, returned
    /// in ascending key order (not reversed).
    ///
    /// Passing `key = None` fetches the last `n` rows of the source.  The window
    /// sets `reached_boundary` when the first row of the source is included.
    fn fetch_before(&mut self, key: Option<Self::Key>, n: usize) -> Window<Self::Row>;

    /// Approximate the fractional position of `key` within the source, in
    /// `[0.0, 1.0]`, for driving an **estimated** scrollbar thumb.
    ///
    /// Returns `None` when the source cannot even estimate (e.g. an opaque cursor
    /// with no statistics).  This is deliberately approximate — see the
    /// scrollbar-honesty note in the module docs; consumers must not treat it as
    /// an exact ordinal.
    fn approx_position(&mut self, key: &Self::Key) -> Option<f32>;

    /// The exact total row count, **only if** the source knows it cheaply.
    ///
    /// Returns `None` for remote cursor sources where counting is O(n) or
    /// unavailable; in that case the scrollbar must fall back to
    /// [`approx_position`](RecordSource::approx_position).  Returning `Some`
    /// promises an exact thumb.
    fn exact_len(&mut self) -> Option<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_constructors() {
        let w = Window::new(vec![1, 2, 3], false);
        assert_eq!(w.len(), 3);
        assert!(!w.is_empty());
        assert!(!w.reached_boundary);

        let e: Window<i32> = Window::empty();
        assert!(e.is_empty());
        assert!(e.reached_boundary);
    }

    /// A tiny in-test implementation proving the trait is object-shaped and the
    /// associated types resolve.  (Real implementations land in Phase 3.)
    struct VecSource {
        keys: Vec<u64>,
    }

    impl RecordSource for VecSource {
        type Key = u64;
        type Row = u64;

        fn fetch_after(&mut self, key: Option<u64>, n: usize) -> Window<u64> {
            let start = match key {
                None => 0,
                Some(k) => self.keys.partition_point(|&x| x <= k),
            };
            let end = (start + n).min(self.keys.len());
            Window::new(self.keys[start..end].to_vec(), end == self.keys.len())
        }

        fn fetch_before(&mut self, key: Option<u64>, n: usize) -> Window<u64> {
            let end = match key {
                None => self.keys.len(),
                Some(k) => self.keys.partition_point(|&x| x < k),
            };
            let start = end.saturating_sub(n);
            Window::new(self.keys[start..end].to_vec(), start == 0)
        }

        fn approx_position(&mut self, key: &u64) -> Option<f32> {
            if self.keys.is_empty() {
                return None;
            }
            let idx = self.keys.partition_point(|&x| x < *key);
            Some(idx as f32 / self.keys.len() as f32)
        }

        fn exact_len(&mut self) -> Option<u64> {
            Some(self.keys.len() as u64)
        }
    }

    #[test]
    fn vec_source_round_trips() {
        let mut s = VecSource { keys: vec![10, 20, 30, 40, 50] };
        let after = s.fetch_after(Some(20), 2);
        assert_eq!(after.rows, vec![30, 40]);
        assert!(!after.reached_boundary);

        let before = s.fetch_before(Some(40), 2);
        assert_eq!(before.rows, vec![20, 30]);

        assert_eq!(s.exact_len(), Some(5));
        assert_eq!(s.approx_position(&30), Some(2.0 / 5.0));
    }
}
