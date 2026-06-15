// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use tile_engine::backend::{Backend, CrosstermBackend};

const LEAVE_ALT: &str = "\x1b[?1049l";
const SHOW_CURSOR: &str = "\x1b[?25h";

// `mark_entered` sets the `entered` flag without calling `enable_raw_mode`,
// so the restore path can be exercised against a Vec<u8> sink.

#[test]
fn leave_writes_alt_screen_and_cursor_restore() {
    let mut buf = Vec::<u8>::new();
    {
        let mut backend = CrosstermBackend::new(&mut buf);
        backend.mark_entered();
        backend.leave().unwrap();
    } // drop backend to release &mut buf borrow before reading
    let out = String::from_utf8_lossy(&buf);
    assert!(out.contains(LEAVE_ALT), "leave() must emit leave-alt-screen; got: {out:?}");
    assert!(out.contains(SHOW_CURSOR), "leave() must emit show-cursor; got: {out:?}");
}

#[test]
fn drop_writes_restore_when_entered() {
    let mut buf = Vec::<u8>::new();
    {
        let mut backend = CrosstermBackend::new(&mut buf);
        backend.mark_entered();
        // Drop fires here, should call leave() best-effort.
    }
    let out = String::from_utf8_lossy(&buf);
    assert!(out.contains(LEAVE_ALT), "Drop must emit leave-alt-screen; got: {out:?}");
    assert!(out.contains(SHOW_CURSOR), "Drop must emit show-cursor; got: {out:?}");
}

#[test]
fn drop_does_nothing_when_not_entered() {
    let mut buf = Vec::<u8>::new();
    {
        let _backend = CrosstermBackend::new(&mut buf);
        // entered is false; Drop should be a no-op.
    }
    assert!(buf.is_empty(), "Drop must not write anything when not entered");
}
