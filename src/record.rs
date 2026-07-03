// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The seek-shaped data-provider trait for row virtualization (design note §4.1).
//!
//! This module defines the *contract* a backing store must satisfy to be scrolled
//! through without materializing all of its rows.  The trait and its [`Window`]
//! type were defined **with the floating-tile foundation**, rather than
//! retrofitted alongside the consumer, so the rest of the engine could be built
//! against a stable shape from the start.  It also ships [`VecRecordSource`], an
//! in-memory reference implementation for tests and demos; real seek/keyset and
//! LDAP VLV implementations live in the consuming applications.
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
/// advances or mutates, so the fetch and query methods take `&mut self` rather
/// than pretending the source is pure. [`key_of`](RecordSource::key_of) is the
/// exception: it only reads a row the caller already holds, so it takes `&self`.
pub trait RecordSource {
    /// The ordering key that anchors a fetch (e.g. a primary key or LDAP DN).
    type Key;
    /// One materialized row handed back to the consumer.
    type Row;

    /// The ordering key of a materialized row.
    ///
    /// Keyset paging is anchored by key, so the consumer must be able to recover
    /// the key of a row it already holds in order to fetch the next or previous
    /// window. A row therefore carries (or can derive) its own key — the SQL key
    /// column, the LDAP DN, and so on.
    fn key_of(&self, row: &Self::Row) -> Self::Key;

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

// ── VecRecordSource ──────────────────────────────────────────────────────────

/// An in-memory [`RecordSource`] over a sorted vector of `(key, value)` rows.
///
/// This is the reference implementation: it materializes everything up front, so
/// it is meant for tests and demos rather than large data. It still honors the
/// seek-shaped contract — every fetch is resolved by binary search on the key,
/// never by integer offset — so it exercises the same windowing code paths a real
/// keyset or VLV backend would.
///
/// The [`Row`](RecordSource::Row) is the whole `(K, V)` pair, so
/// [`key_of`](RecordSource::key_of) simply returns the key component.
///
/// By default [`exact_len`](RecordSource::exact_len) returns `Some` (an exact
/// scrollbar). Call [`estimated`](VecRecordSource::estimated) to model a remote
/// cursor whose length is unknown: `exact_len` then returns `None`, forcing the
/// honest-estimate scrollbar path (§6.2).
#[derive(Debug, Clone)]
pub struct VecRecordSource<K, V> {
    /// Rows sorted ascending by key; keys are assumed unique.
    rows: Vec<(K, V)>,
    /// Whether the source admits to knowing its exact length.
    knows_len: bool,
}

impl<K: Ord + Clone, V: Clone> VecRecordSource<K, V> {
    /// Build a source from `(key, value)` rows, sorting them by key.
    pub fn new(mut rows: Vec<(K, V)>) -> Self {
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Self { rows, knows_len: true }
    }

    /// Model an unknown-length remote cursor: `exact_len` returns `None`, so a
    /// consumer must fall back to the estimated scrollbar.
    pub fn estimated(mut self) -> Self {
        self.knows_len = false;
        self
    }

    /// Total number of rows held (independent of the `knows_len` pretence).
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` when the source holds no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

impl<K: Ord + Clone, V: Clone> RecordSource for VecRecordSource<K, V> {
    type Key = K;
    type Row = (K, V);

    /// The key component of the `(key, value)` row.
    fn key_of(&self, row: &(K, V)) -> K {
        row.0.clone()
    }

    /// Rows with key strictly greater than `key` (or from the start), by binary
    /// search — the in-memory analogue of `WHERE key > ? ORDER BY key LIMIT n`.
    fn fetch_after(&mut self, key: Option<K>, n: usize) -> Window<(K, V)> {
        let start = match key {
            None => 0,
            // First index whose key is > `k` (all keys ≤ k are skipped).
            Some(k) => self.rows.partition_point(|(rk, _)| *rk <= k),
        };
        let end = (start + n).min(self.rows.len());
        Window::new(self.rows[start..end].to_vec(), end == self.rows.len())
    }

    /// Rows with key strictly less than `key` (or the last `n`), returned in
    /// ascending key order so windows stitch without re-sorting.
    fn fetch_before(&mut self, key: Option<K>, n: usize) -> Window<(K, V)> {
        let end = match key {
            None => self.rows.len(),
            // First index whose key is ≥ `k`; everything before it is < k.
            Some(k) => self.rows.partition_point(|(rk, _)| *rk < k),
        };
        let start = end.saturating_sub(n);
        Window::new(self.rows[start..end].to_vec(), start == 0)
    }

    /// Exact fractional position (rows strictly before `key` over the total).
    fn approx_position(&mut self, key: &K) -> Option<f32> {
        if self.rows.is_empty() {
            return None;
        }
        let idx = self.rows.partition_point(|(rk, _)| *rk < *key);
        Some(idx as f32 / self.rows.len() as f32)
    }

    /// `Some(len)` unless the source was put into [`estimated`](VecRecordSource::estimated)
    /// mode.
    fn exact_len(&mut self) -> Option<u64> {
        self.knows_len.then_some(self.rows.len() as u64)
    }
}

// ── RangeSource ────────────────────────────────────────────────────────────────

/// A [`RecordSource`] over a computed range of `len` items, where each item is
/// built **on demand** from its index by a closure.
///
/// Use this for large spaces whose rows are cheap to compute from an ordinal rather
/// than stored — an IP range (`index → address`), row numbers, a synthetic list. A
/// [`VirtualList`](crate::VirtualList) over a `RangeSource` materializes only the
/// visible window, so a `/8` of 16,777,216 addresses costs the same to browse as a
/// `/24`: nothing but the rows on screen is ever built.
///
/// The [`Key`](RecordSource::Key) is the item's `u64` **index**, and the
/// [`Row`](RecordSource::Row) is `(index, value)` — so the value need not carry its
/// own key. Length is known, so the scrollbar is exact.
pub struct RangeSource<T, F> {
    len: u64,
    build: F,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T, F: Fn(u64) -> T> RangeSource<T, F> {
    /// A source of `len` items, each built by `build(index)` when fetched.
    pub fn new(len: u64, build: F) -> Self {
        Self { len, build, _marker: std::marker::PhantomData }
    }

    /// The number of items in the range.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// `true` when the range is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<T, F: Fn(u64) -> T> RecordSource for RangeSource<T, F> {
    type Key = u64;
    type Row = (u64, T);

    /// The index component of the `(index, value)` row.
    fn key_of(&self, row: &(u64, T)) -> u64 {
        row.0
    }

    /// Build the indices immediately after `key` (or from index 0), up to `n`.
    fn fetch_after(&mut self, key: Option<u64>, n: usize) -> Window<(u64, T)> {
        let start = match key {
            None => 0,
            Some(k) => k.saturating_add(1),
        };
        let start = start.min(self.len);
        let end = start.saturating_add(n as u64).min(self.len);
        let rows = (start..end).map(|i| (i, (self.build)(i))).collect();
        Window::new(rows, end >= self.len)
    }

    /// Build up to `n` indices immediately before `key` (or the last `n`), in
    /// ascending order.
    fn fetch_before(&mut self, key: Option<u64>, n: usize) -> Window<(u64, T)> {
        let end = key.unwrap_or(self.len).min(self.len);
        let start = end.saturating_sub(n as u64);
        let rows = (start..end).map(|i| (i, (self.build)(i))).collect();
        Window::new(rows, start == 0)
    }

    /// Exact fractional position: `key / len` (there are `key` indices before it).
    fn approx_position(&mut self, key: &u64) -> Option<f32> {
        if self.len == 0 {
            None
        } else {
            Some((*key).min(self.len) as f32 / self.len as f32)
        }
    }

    /// Always exact — a computed range knows its length.
    fn exact_len(&mut self) -> Option<u64> {
        Some(self.len)
    }
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

        fn key_of(&self, row: &u64) -> u64 {
            *row
        }

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
        assert_eq!(s.key_of(&30), 30);
    }

    #[test]
    fn vec_record_source_sorts_and_keys() {
        // Unsorted input is sorted by key; Row is the (key, value) pair.
        let mut s = VecRecordSource::new(vec![(3, "c"), (1, "a"), (2, "b")]);
        assert_eq!(s.fetch_after(None, 2).rows, vec![(1, "a"), (2, "b")]);
        assert_eq!(s.key_of(&(2, "b")), 2);
        assert_eq!(s.fetch_before(Some(3), 1).rows, vec![(2, "b")]);
        assert_eq!(s.exact_len(), Some(3));
    }

    #[test]
    fn vec_record_source_estimated_hides_len() {
        let mut s = VecRecordSource::new(vec![(1, ()), (2, ())]).estimated();
        // Length is hidden, but position estimation still works.
        assert_eq!(s.exact_len(), None);
        assert_eq!(s.approx_position(&2), Some(0.5));
    }

    #[test]
    fn range_source_len_and_emptiness() {
        let s = RangeSource::new(10, |i| i * 2);
        assert_eq!(s.len(), 10);
        assert!(!s.is_empty());
        assert!(RangeSource::new(0, |i| i).is_empty());
    }

    #[test]
    fn range_source_fetches_windows_lazily() {
        // Value = index * 10, so we can check exactly which indices were built.
        let mut s = RangeSource::new(10, |i| i * 10);

        // From the start.
        let w = s.fetch_after(None, 3);
        assert_eq!(w.rows, vec![(0, 0), (1, 10), (2, 20)]);
        assert!(!w.reached_boundary);

        // After a key, hitting the end (only 8,9 remain of len 10).
        let w = s.fetch_after(Some(7), 5);
        assert_eq!(w.rows, vec![(8, 80), (9, 90)]);
        assert!(w.reached_boundary);

        // The last n rows.
        let w = s.fetch_before(None, 2);
        assert_eq!(w.rows, vec![(8, 80), (9, 90)]);
        assert!(!w.reached_boundary);

        // Before a key, hitting the start.
        let w = s.fetch_before(Some(3), 9);
        assert_eq!(w.rows, vec![(0, 0), (1, 10), (2, 20)]);
        assert!(w.reached_boundary);
    }

    #[test]
    fn range_source_position_and_len() {
        let mut s = RangeSource::new(200, |i| i);
        assert_eq!(s.exact_len(), Some(200));
        assert_eq!(s.approx_position(&100), Some(0.5));
        assert_eq!(s.approx_position(&0), Some(0.0));
    }

    #[test]
    fn range_source_over_a_slash_8_is_instant() {
        // 10.0.0.0/8 = 16,777,216 addresses. Fetching a window near the middle must
        // build only the requested rows — materializing all of it would hang/OOM.
        let mut s = RangeSource::new(16_777_216, |i| i);
        assert_eq!(s.exact_len(), Some(16_777_216));
        let w = s.fetch_after(Some(8_000_000), 4);
        assert_eq!(w.rows, vec![(8_000_001, 8_000_001), (8_000_002, 8_000_002), (8_000_003, 8_000_003), (8_000_004, 8_000_004)]);
        assert!(!w.reached_boundary);
    }

    #[test]
    fn range_source_drives_a_virtual_list() {
        use crate::VirtualList;
        // A million-row range, windowed by a VirtualList that only ever holds a few.
        let source = RangeSource::new(1_000_000, |i| i);
        let mut list = VirtualList::new(source, 5, 8);
        assert!(list.at_top());
        assert!(!list.visible().is_empty());
        assert_eq!(list.visible()[0].0, 0); // first visible key is index 0
        list.scroll_by(3); // scroll down toward later keys
        assert_eq!(list.visible()[0].0, 3); // the window moved; key is stable under trimming
    }
}
