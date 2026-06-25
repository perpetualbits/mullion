// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Named style roles so apps can use semantic colour names instead of literal
//! [`Color`] values.
//!
//! A [`Theme`] collects one [`Style`] per UI role.  Swapping the `Theme`
//! atomically recolors the whole interface.  Two built-in palettes are provided:
//!
//! | Constructor | Palette |
//! |---|---|
//! | [`Theme::default`] | Dark (terminal-default background, cyan accent) |
//! | [`Theme::light`]   | Light (black text, blue accent) |

use crate::border::{BorderStyle, CornerStyle, LineWeight};
use crate::style::{Color, Modifier, Style};

/// A collection of [`Style`]s covering common terminal-UI roles.
///
/// Apps reference roles (e.g. `theme.accent`) rather than hardcoded colours.
/// Passing a `Theme` reference through the widget tree lets a single palette
/// change re-render the entire interface consistently.
///
/// See [`Theme::border_style`] to obtain a ready-to-use [`BorderStyle`] for a
/// focused or unfocused tile.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Border colour/attributes for unfocused tiles.
    pub border: Style,
    /// Border colour/attributes for the focused tile.
    pub border_focused: Style,
    /// Primary content text (foreground only; leave bg as `Reset` to inherit).
    pub text: Style,
    /// Secondary or dimmed text (labels, status hints, captions).
    pub text_dim: Style,
    /// Accent colour for gauges, marquees, and selected controls.
    pub accent: Style,
    /// Background highlight for selected items.
    pub selection: Style,
    /// Emphasised text for headings/titles, distinct from body [`text`](Theme::text).
    pub heading: Style,
    /// Success / healthy / "OK" status (bind succeeded, write committed).
    pub ok: Style,
    /// Warning / caution status (nearing a limit, unsaved changes).
    pub warn: Style,
    /// Error / failure status (write failed, validation error, mismatch).
    pub error: Style,
}

impl Theme {
    /// Return a [`BorderStyle`] appropriate for a tile's focus state.
    ///
    /// Focused tiles get a `Heavy` border using [`border_focused`](Theme::border_focused);
    /// all other tiles get a `Light` border using [`border`](Theme::border).
    /// Corners are always `Square` (rounded-corner support is Phase 7d).
    pub fn border_style(&self, focused: bool) -> BorderStyle {
        if focused {
            BorderStyle {
                weight:  LineWeight::Heavy,
                corners: CornerStyle::Square,
                style:   self.border_focused,
            }
        } else {
            BorderStyle {
                weight:  LineWeight::Light,
                corners: CornerStyle::Square,
                style:   self.border,
            }
        }
    }

    /// Light colour scheme (black text, blue accent, white/gray borders).
    ///
    /// Use this on terminals configured with a light background.
    pub fn light() -> Self {
        Self {
            border:         Style::default().fg(Color::Gray),
            border_focused: Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
            text:           Style::default().fg(Color::Black),
            text_dim:       Style::default().fg(Color::DarkGray),
            accent:         Style::default().fg(Color::Blue),
            selection:      Style::default().fg(Color::White).bg(Color::Blue),
            heading:        Style::default().fg(Color::Black).add_modifier(Modifier::BOLD),
            // On a light background, light-* variants wash out: use the saturated
            // base colours (and a dark goldenrod for warn) so status stays legible.
            ok:             Style::default().fg(Color::Green),
            warn:           Style::default().fg(Color::Rgb(0xB8, 0x86, 0x0B)),
            error:          Style::default().fg(Color::Red),
        }
    }
}

impl Default for Theme {
    /// Dark colour scheme (terminal-default background, cyan accent).
    ///
    /// `text` is entirely unstyled so it inherits the terminal's default
    /// foreground; only structural roles carry explicit colours.
    fn default() -> Self {
        Self {
            border:         Style::default().fg(Color::DarkGray),
            border_focused: Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD),
            text:           Style::default(),
            text_dim:       Style::default().fg(Color::DarkGray),
            accent:         Style::default().fg(Color::LightCyan),
            selection:      Style::default().fg(Color::Black).bg(Color::LightCyan),
            heading:        Style::default().add_modifier(Modifier::BOLD),
            // On a dark background the bright light-* variants read best for status.
            ok:             Style::default().fg(Color::LightGreen),
            warn:           Style::default().fg(Color::LightYellow),
            error:          Style::default().fg(Color::LightRed),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_theme_unfocused_border_is_light_weight() {
        let theme = Theme::default();
        let bs = theme.border_style(false);
        assert_eq!(bs.weight, LineWeight::Light);
        assert_eq!(bs.style, theme.border);
    }

    #[test]
    fn dark_theme_focused_border_is_heavy_weight() {
        let theme = Theme::default();
        let bs = theme.border_style(true);
        assert_eq!(bs.weight, LineWeight::Heavy);
        assert_eq!(bs.style, theme.border_focused);
    }

    #[test]
    fn light_theme_border_style_follows_focus() {
        let theme = Theme::light();
        assert_eq!(theme.border_style(false).weight, LineWeight::Light);
        assert_eq!(theme.border_style(true).weight, LineWeight::Heavy);
    }

    #[test]
    fn dark_and_light_accent_colours_differ() {
        // Ensure the built-in palettes are genuinely distinct.
        assert_ne!(Theme::default().accent, Theme::light().accent);
    }

    #[test]
    fn focused_and_unfocused_border_styles_differ() {
        let theme = Theme::default();
        assert_ne!(theme.border_style(false).style, theme.border_style(true).style);
    }

    #[test]
    fn status_roles_are_distinct_in_both_palettes() {
        for theme in [Theme::default(), Theme::light()] {
            // The three status roles must be mutually distinct so success/warning/
            // error are never confusable, and a heading must read apart from body.
            assert_ne!(theme.ok, theme.error);
            assert_ne!(theme.ok, theme.warn);
            assert_ne!(theme.warn, theme.error);
            assert_ne!(theme.heading, theme.text);
        }
    }
}
