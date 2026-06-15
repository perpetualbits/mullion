/// A terminal color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// Reset to the terminal's default color.
    #[default]
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    Gray,
    /// An 8-bit indexed color (0–255).
    Indexed(u8),
    /// A 24-bit RGB color.
    Rgb(u8, u8, u8),
}

bitflags::bitflags! {
    /// Text attribute modifiers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Modifier: u16 {
        const BOLD      = 0b0000_0001;
        const DIM       = 0b0000_0010;
        const ITALIC    = 0b0000_0100;
        const UNDERLINE = 0b0000_1000;
        const REVERSE   = 0b0001_0000;
    }
}

/// Combined foreground color, background color, and text modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub mods: Modifier,
}

impl Style {
    /// Return a new `Style` with fields from `other` overriding non-`Reset` / non-empty values.
    ///
    /// `Reset` fg/bg in `other` is treated as "keep mine"; fully empty mods keep mine.
    pub fn patch(self, other: Style) -> Style {
        Style {
            fg: if other.fg == Color::Reset { self.fg } else { other.fg },
            bg: if other.bg == Color::Reset { self.bg } else { other.bg },
            mods: if other.mods.is_empty() { self.mods } else { other.mods },
        }
    }

    pub fn fg(mut self, color: Color) -> Self {
        self.fg = color;
        self
    }

    pub fn bg(mut self, color: Color) -> Self {
        self.bg = color;
        self
    }

    pub fn add_modifier(mut self, m: Modifier) -> Self {
        self.mods |= m;
        self
    }
}
