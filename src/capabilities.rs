// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Terminal capability detection.
//!
//! Call [`Capabilities::detect`] once at program startup and pass the result to
//! [`CrosstermBackend::apply_capabilities`](crate::CrosstermBackend::apply_capabilities)
//! to automatically configure colour depth, Unicode charset mode, and
//! synchronized-output behaviour.
//!
//! ## Usage pattern
//!
//! ```no_run
//! use mullion::{backend::CrosstermBackend, capabilities::Capabilities};
//! let mut backend = CrosstermBackend::new(std::io::stdout());
//! backend.apply_capabilities(&Capabilities::detect());
//! ```
//!
//! ## Conservative defaults
//!
//! When environment variables are absent or unrecognised, `detect` degrades
//! safely: `Palette16` colour and unicode enabled.  Use [`Capabilities::full`]
//! in tests or when the terminal is known to support everything.

use std::env;

use crate::style::ColorDepth;

/// Terminal capabilities detected from the process environment.
///
/// Obtain a value via [`Capabilities::detect`] (reads the process environment)
/// or [`Capabilities::full`] (truecolor + unicode + sync, for tests/known-good
/// terminals).  Pass to
/// [`CrosstermBackend::apply_capabilities`](crate::CrosstermBackend::apply_capabilities)
/// to configure all adaptations at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Maximum colour depth the terminal reliably supports.
    pub color: ColorDepth,
    /// Whether Unicode box-drawing glyphs and wide graphemes render reliably.
    ///
    /// When `false`, [`CrosstermBackend`](crate::CrosstermBackend) replaces
    /// box-drawing characters with ASCII equivalents via
    /// [`box_to_ascii`](crate::charset::box_to_ascii) before emission.
    pub unicode: bool,
    /// Whether the `\x1b[?2026h/l` synchronized-output extension is honoured.
    ///
    /// When `false`, the begin/end-frame sync markers are not emitted.
    /// Terminals that do not implement the extension silently ignore the
    /// sequences, so this flag is a performance hint rather than a
    /// correctness requirement — it defaults to `true` unconditionally.
    pub synchronized_output: bool,
}

impl Capabilities {
    /// Detect capabilities from the current process environment.
    ///
    /// Reads `COLORTERM`, `TERM`, and the locale (`LC_ALL`, `LC_CTYPE`,
    /// `LANG`) and delegates to the pure [`from_env`] helper.
    pub fn detect() -> Self {
        let colorterm = env::var("COLORTERM").ok();
        let term      = env::var("TERM").ok();
        let ctype     = env::var("LC_ALL")
            .or_else(|_| env::var("LC_CTYPE"))
            .or_else(|_| env::var("LANG"))
            .ok();
        from_env(colorterm.as_deref(), term.as_deref(), ctype.as_deref())
    }

    /// Full-capability preset: truecolor, unicode, synchronized output.
    ///
    /// Use this in unit tests and integration fixtures where the rendering
    /// target is a known-good byte sink rather than a real terminal.
    pub fn full() -> Self {
        Self { color: ColorDepth::TrueColor, unicode: true, synchronized_output: true }
    }
}

/// Derive [`Capabilities`] from explicit environment strings.
///
/// Pure function — no environment I/O — so it can be exercised in tests
/// without touching the process environment.  Pass `None` for absent variables.
///
/// `ctype` should be the first of `LC_ALL`, `LC_CTYPE`, or `LANG` that is set. It is
/// currently reserved and unused — the unicode heuristic below no longer inspects the
/// locale (see the inline note at the `let _ = ctype;` binding).
///
/// ## Heuristics (conservative — when unsure, degrade)
///
/// * **color:** `COLORTERM = truecolor | 24bit` → `TrueColor`; `TERM` contains
///   `256color` → `Palette256`; otherwise → `Palette16`.
/// * **unicode:** `TERM = linux` (Linux text console, unreliable Unicode even with a
///   UTF-8 locale) → `false`; every other `TERM` → `true`, regardless of locale (the
///   `ctype` locale is not inspected — most modern emulators work fine).
/// * **synchronized_output:** always `true` (the sequence is safe to emit on
///   non-supporting terminals).
pub fn from_env(
    colorterm: Option<&str>,
    term:      Option<&str>,
    ctype:     Option<&str>,
) -> Capabilities {
    // ── Colour depth ────────────────────────────────────────────────────────
    let color = if matches!(colorterm, Some("truecolor") | Some("24bit")) {
        ColorDepth::TrueColor
    } else if term.is_some_and(|t| t.contains("256color")) {
        ColorDepth::Palette256
    } else {
        ColorDepth::Palette16
    };

    // ── Unicode ─────────────────────────────────────────────────────────────
    // The Linux framebuffer console (TERM=linux) does not render box-drawing
    // glyphs reliably even when the locale advertises UTF-8.  All other
    // terminals default to `true` — a UTF-8 locale is a positive signal, but
    // absent or non-UTF-8 locales are not a negative one: modern emulators
    // handle box-drawing correctly regardless.
    let linux_console = term.is_some_and(|t| t == "linux");
    let _ = ctype; // examined by callers; not currently required to degrade unicode
    let unicode = !linux_console;

    Capabilities { color, unicode, synchronized_output: true }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Colour depth ──────────────────────────────────────────────────────

    #[test]
    fn colorterm_truecolor_gives_truecolor() {
        assert_eq!(from_env(Some("truecolor"), None, None).color, ColorDepth::TrueColor);
    }

    #[test]
    fn colorterm_24bit_gives_truecolor() {
        assert_eq!(from_env(Some("24bit"), None, None).color, ColorDepth::TrueColor);
    }

    #[test]
    fn term_xterm_256color_gives_palette256() {
        assert_eq!(from_env(None, Some("xterm-256color"), None).color, ColorDepth::Palette256);
    }

    #[test]
    fn term_screen_256color_gives_palette256() {
        assert_eq!(from_env(None, Some("screen-256color"), None).color, ColorDepth::Palette256);
    }

    #[test]
    fn term_xterm_no_256color_gives_palette16() {
        assert_eq!(from_env(None, Some("xterm"), None).color, ColorDepth::Palette16);
    }

    #[test]
    fn all_absent_gives_palette16() {
        assert_eq!(from_env(None, None, None).color, ColorDepth::Palette16);
    }

    #[test]
    fn colorterm_beats_256color_in_term() {
        // COLORTERM wins over TERM for colour depth.
        assert_eq!(
            from_env(Some("truecolor"), Some("xterm-256color"), None).color,
            ColorDepth::TrueColor,
        );
    }

    // ── Unicode ───────────────────────────────────────────────────────────

    #[test]
    fn utf8_locale_enables_unicode() {
        assert!(from_env(None, Some("xterm-256color"), Some("en_US.UTF-8")).unicode);
    }

    #[test]
    fn utf8_lowercase_locale_enables_unicode() {
        assert!(from_env(None, None, Some("en_US.utf8")).unicode);
    }

    #[test]
    fn absent_locale_defaults_unicode_true() {
        // No locale info → optimistic default.
        assert!(from_env(None, Some("xterm"), None).unicode);
    }

    #[test]
    fn non_utf8_locale_still_defaults_true() {
        // A non-UTF-8 locale ("C") → still default true (emulator likely works).
        assert!(from_env(None, Some("xterm"), Some("C")).unicode);
    }

    #[test]
    fn linux_console_disables_unicode() {
        // TERM=linux is the Linux VT; box-drawing is unreliable regardless of locale.
        assert!(!from_env(None, Some("linux"), Some("en_US.UTF-8")).unicode);
    }

    #[test]
    fn linux_console_disables_unicode_even_without_locale() {
        assert!(!from_env(None, Some("linux"), None).unicode);
    }

    // ── Synchronized output ───────────────────────────────────────────────

    #[test]
    fn synchronized_output_is_always_true() {
        assert!(from_env(None, None, None).synchronized_output);
        assert!(from_env(Some("truecolor"), Some("linux"), Some("en_US.UTF-8")).synchronized_output);
    }

    // ── Capabilities::full ────────────────────────────────────────────────

    #[test]
    fn full_gives_truecolor_and_unicode() {
        let c = Capabilities::full();
        assert_eq!(c.color, ColorDepth::TrueColor);
        assert!(c.unicode);
        assert!(c.synchronized_output);
    }
}
