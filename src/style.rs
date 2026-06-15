/// A terminal color.
///
/// The sixteen named variants follow the standard ANSI 8+8 palette.  The
/// naming convention used here (and by ratatui) is:
///
/// | Variant | ANSI index | Typical appearance |
/// |---|---|---|
/// | `Black` | 0 | Black |
/// | `Red`…`Cyan` | 1–6 | Dark (dim) versions |
/// | `Gray` | 7 | Standard white / light grey |
/// | `DarkGray` | 8 | Bright black / dark grey |
/// | `LightRed`…`LightCyan` | 9–14 | Bright (vivid) versions |
/// | `White` | 15 | Bright white |
///
/// `Reset` instructs the terminal to fall back to its own default color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// Reset to the terminal's own default foreground or background color.
    #[default]
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    /// Standard white (ANSI 7, appears as light grey on most terminals).
    White,
    /// Bright black / dark grey (ANSI 8).
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    /// Bright white (ANSI 15, brighter than `White`).
    Gray,
    /// An 8-bit indexed color (0–255).
    Indexed(u8),
    /// A 24-bit RGB color.
    Rgb(u8, u8, u8),
}

bitflags::bitflags! {
    /// Text attribute modifiers, stored as a bitmask.
    ///
    /// Combine flags with `|`; test membership with `contains`.
    /// The `Default` impl yields an empty set (no attributes active).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Modifier: u16 {
        const BOLD      = 0b0000_0001;
        const DIM       = 0b0000_0010;
        const ITALIC    = 0b0000_0100;
        const UNDERLINE = 0b0000_1000;
        const REVERSE   = 0b0001_0000;
    }
}

/// Combined foreground color, background color, and text modifiers for a cell.
///
/// The `Default` impl produces `Style { fg: Color::Reset, bg: Color::Reset,
/// mods: Modifier::empty() }`, which defers all rendering choices to the
/// terminal's own defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    /// Foreground (text) color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Active text-attribute flags (bold, italic, …).
    pub mods: Modifier,
}

impl Style {
    /// Return a new `Style` with fields from `other` overlaid on `self`.
    ///
    /// The merge rule treats `Color::Reset` in `other` as "keep the base value"
    /// and an empty `Modifier` set in `other` as "keep the base modifiers".
    /// Any non-Reset color or non-empty modifier set in `other` wins outright.
    ///
    /// This is intentional: callers use `Reset` / empty to mean "I have no
    /// opinion on this field", not "please reset this field to the terminal
    /// default".  A caller that wants to explicitly clear a field should build
    /// a `Style` carrying `Reset` and apply it to a `Style::default()` base.
    pub fn patch(self, other: Style) -> Style {
        Style {
            // A Reset fg in `other` means "no opinion" — fall back to self.
            fg: if other.fg == Color::Reset { self.fg } else { other.fg },
            bg: if other.bg == Color::Reset { self.bg } else { other.bg },
            // An empty modifier set in `other` means "no opinion" — fall back to self.
            mods: if other.mods.is_empty() { self.mods } else { other.mods },
        }
    }

    /// Set the foreground color, consuming and returning `self` for chaining.
    pub fn fg(mut self, color: Color) -> Self {
        self.fg = color;
        self
    }

    /// Set the background color, consuming and returning `self` for chaining.
    pub fn bg(mut self, color: Color) -> Self {
        self.bg = color;
        self
    }

    /// Add a modifier flag, consuming and returning `self` for chaining.
    pub fn add_modifier(mut self, m: Modifier) -> Self {
        self.mods |= m;
        self
    }
}
