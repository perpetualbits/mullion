// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
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
    /// Bright white (ANSI 15, brighter than `Gray`).
    White,
    /// Bright black / dark grey (ANSI 8).
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    /// Standard white / light grey (ANSI 7).
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

// ── ColorDepth ────────────────────────────────────────────────────────────────

/// Controls how [`Color::Rgb`] (and [`Color::Indexed`] at the 16-colour step)
/// are downsampled before being emitted to the terminal.
///
/// Use [`Color::downsample`] to apply a depth to a single colour, or
/// set [`CrosstermBackend::set_color_depth`](crate::CrosstermBackend::set_color_depth)
/// to apply it automatically to every rendered cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDepth {
    /// Emit 24-bit truecolor (`38;2;r;g;b` / `48;2;r;g;b`) sequences.
    ///
    /// Identity transform: every [`Color`] variant passes through unchanged.
    #[default]
    TrueColor,
    /// Map [`Color::Rgb`] to the nearest xterm 256-colour palette index.
    ///
    /// Both the 6×6×6 RGB cube (indices 16–231) and the 24-step grayscale ramp
    /// (indices 232–255) are evaluated; whichever has a smaller squared RGB
    /// distance wins.  Named colours, [`Color::Indexed`], and [`Color::Reset`]
    /// pass through unchanged.
    Palette256,
    /// Map [`Color::Rgb`] and [`Color::Indexed`] to the nearest of the 16 ANSI
    /// named colour variants (`Black`, `Red`, …, `White`).
    ///
    /// Named colours and [`Color::Reset`] pass through unchanged.
    /// [`Color::Indexed`] is first expanded to its RGB components (using the
    /// standard xterm cube / grayscale formulae) and then matched against the
    /// ANSI 16 table.
    Palette16,
}

// ── Downsampling helpers ──────────────────────────────────────────────────────

/// xterm-256 cube level values for each axis step (0–5).
///
/// Step 0 is black (0); steps 1–5 are 95, 135, 175, 215, 255.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Standard RGB values for ANSI colour indices 0–15 (the 16 named variants).
///
/// The table order is: Black, Red, Green, Yellow, Blue, Magenta, Cyan,
/// Gray (ANSI 7 = standard white), DarkGray (ANSI 8 = bright black),
/// LightRed … LightCyan, White (ANSI 15 = bright white).
const ANSI16_RGB: [(u8, u8, u8); 16] = [
    (  0,   0,   0), // 0  Black
    (128,   0,   0), // 1  Red      (dark)
    (  0, 128,   0), // 2  Green    (dark)
    (128, 128,   0), // 3  Yellow   (dark)
    (  0,   0, 128), // 4  Blue     (dark)
    (128,   0, 128), // 5  Magenta  (dark)
    (  0, 128, 128), // 6  Cyan     (dark)
    (192, 192, 192), // 7  Gray     (standard white  / ANSI 7)
    (128, 128, 128), // 8  DarkGray (bright black    / ANSI 8)
    (255,   0,   0), // 9  LightRed
    (  0, 255,   0), // 10 LightGreen
    (255, 255,   0), // 11 LightYellow
    (  0,   0, 255), // 12 LightBlue
    (255,   0, 255), // 13 LightMagenta
    (  0, 255, 255), // 14 LightCyan
    (255, 255, 255), // 15 White    (bright white    / ANSI 15)
];

/// Squared Euclidean distance between two 8-bit channel values.
fn sq_dist(a: u8, b: u8) -> u32 {
    let d = a as i32 - b as i32;
    (d * d) as u32
}

/// Return the xterm cube level index (0–5) whose value is closest to `v`.
fn nearest_cube_level(v: u8) -> usize {
    CUBE_LEVELS
        .iter()
        .enumerate()
        .min_by_key(|&(_, &l)| sq_dist(v, l))
        .map(|(i, _)| i)
        .unwrap() // non-empty slice
}

/// Return the grayscale ramp sub-index `n` (0–23) whose entry is closest
/// to the point `(r, g, b)`.
///
/// Gray ramp values are `8 + 10*n`; the optimal continuous target is the
/// channel mean `(r+g+b)/3`, and the nearest ramp entry is found by iterating
/// all 24 values (negligible cost).
fn nearest_gray_n(r: u8, g: u8, b: u8) -> u8 {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    (0u8..24)
        .min_by_key(|&n| {
            let v = 8 + 10 * n as i32;
            (r - v) * (r - v) + (g - v) * (g - v) + (b - v) * (b - v)
        })
        .unwrap() // non-empty range
}

/// Expand a palette index to its RGB components.
///
/// Covers all three xterm-256 regions:
/// - 0–15: the 16 ANSI colours (see `ANSI16_RGB`).
/// - 16–231: 6×6×6 RGB cube using `CUBE_LEVELS`.
/// - 232–255: 24-step grayscale ramp (`8 + 10*(i − 232)`).
fn indexed_to_rgb(i: u8) -> (u8, u8, u8) {
    if i < 16 {
        ANSI16_RGB[i as usize]
    } else if i < 232 {
        let i = i - 16;
        (
            CUBE_LEVELS[(i / 36) as usize],
            CUBE_LEVELS[((i % 36) / 6) as usize],
            CUBE_LEVELS[(i % 6) as usize],
        )
    } else {
        // Grayscale ramp: values 8, 18, 28 … 238.
        let v = 8 + 10 * (i - 232);
        (v, v, v)
    }
}

/// Map `(r, g, b)` to the nearest of the 16 ANSI named [`Color`] variants.
///
/// Uses the squared Euclidean distance in RGB space; ties are broken by the
/// lower ANSI index (the array iteration order).
fn nearest_ansi16(r: u8, g: u8, b: u8) -> Color {
    const VARIANTS: [Color; 16] = [
        Color::Black, Color::Red,          Color::Green,        Color::Yellow,
        Color::Blue,  Color::Magenta,      Color::Cyan,         Color::Gray,
        Color::DarkGray, Color::LightRed,  Color::LightGreen,   Color::LightYellow,
        Color::LightBlue, Color::LightMagenta, Color::LightCyan, Color::White,
    ];
    VARIANTS
        .iter()
        .zip(ANSI16_RGB.iter())
        .min_by_key(|&(_, &(cr, cg, cb))| {
            sq_dist(r, cr) + sq_dist(g, cg) + sq_dist(b, cb)
        })
        .map(|(&c, _)| c)
        .unwrap() // non-empty slice
}

impl Color {
    /// Construct a 24-bit [`Color::Rgb`] from hue, saturation, and value.
    ///
    /// - `h` — hue in degrees, wrapped automatically to `[0°, 360°)`.
    /// - `s` — saturation, clamped to `[0, 1]`.  `0` gives a grey.
    /// - `v` — value (brightness), clamped to `[0, 1]`.  `0` gives black.
    ///
    /// This is the standard HSV → RGB conversion (identical to "HSB" in most
    /// colour pickers).  It is the most natural space for hue-shifting colour
    /// animations: increment `h` to cycle through the spectrum, hold `s` near
    /// 1 for vivid colours, and modulate `v` for pulsing brightness.
    ///
    /// ```
    /// use mullion::Color;
    /// assert_eq!(Color::from_hsv(0.0,   1.0, 1.0), Color::Rgb(255,   0,   0)); // red
    /// assert_eq!(Color::from_hsv(120.0, 1.0, 1.0), Color::Rgb(  0, 255,   0)); // green
    /// assert_eq!(Color::from_hsv(240.0, 1.0, 1.0), Color::Rgb(  0,   0, 255)); // blue
    /// assert_eq!(Color::from_hsv(0.0,   0.0, 1.0), Color::Rgb(255, 255, 255)); // white (s=0)
    /// assert_eq!(Color::from_hsv(0.0,   1.0, 0.0), Color::Rgb(  0,   0,   0)); // black (v=0)
    /// ```
    pub fn from_hsv(h: f32, s: f32, v: f32) -> Color {
        let s = s.clamp(0.0, 1.0);
        let v = v.clamp(0.0, 1.0);
        let h = h.rem_euclid(360.0);
        let c = v * s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = v - c;
        let (r, g, b) = match (h / 60.0) as u32 {
            0 => (c, x, 0.0_f32),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        Color::Rgb(
            ((r + m) * 255.0).round() as u8,
            ((g + m) * 255.0).round() as u8,
            ((b + m) * 255.0).round() as u8,
        )
    }

    /// Map this colour down to `depth`.
    ///
    /// | Variant        | TrueColor | Palette256                      | Palette16                  |
    /// |----------------|-----------|---------------------------------|----------------------------|
    /// | `Rgb`          | identity  | nearest xterm-256 cube or gray  | nearest ANSI 16 named      |
    /// | `Indexed`      | identity  | identity                        | nearest ANSI 16 (via RGB expansion) |
    /// | named / `Reset`| identity  | identity                        | identity                   |
    ///
    /// For `Palette256`, both the 6×6×6 cube and the grayscale ramp are
    /// evaluated; the one with the smaller squared RGB distance wins.
    pub fn downsample(self, depth: ColorDepth) -> Color {
        match depth {
            ColorDepth::TrueColor => self,

            ColorDepth::Palette256 => {
                let Color::Rgb(r, g, b) = self else { return self; };

                // Cube candidate: find the nearest level index per channel.
                let ri = nearest_cube_level(r);
                let gi = nearest_cube_level(g);
                let bi = nearest_cube_level(b);
                let cube_idx = (16 + 36 * ri + 6 * gi + bi) as u8;
                let cube_dist = sq_dist(r, CUBE_LEVELS[ri])
                    + sq_dist(g, CUBE_LEVELS[gi])
                    + sq_dist(b, CUBE_LEVELS[bi]);

                // Grayscale ramp candidate.
                let gn  = nearest_gray_n(r, g, b);
                let gv  = 8u8 + 10 * gn;
                let gray_dist = sq_dist(r, gv) + sq_dist(g, gv) + sq_dist(b, gv);

                // Prefer the gray ramp when it is strictly closer (or equal, cube wins).
                if gray_dist < cube_dist {
                    Color::Indexed(232 + gn)
                } else {
                    Color::Indexed(cube_idx)
                }
            }

            ColorDepth::Palette16 => match self {
                Color::Rgb(r, g, b) => nearest_ansi16(r, g, b),
                Color::Indexed(i) => {
                    let (r, g, b) = indexed_to_rgb(i);
                    nearest_ansi16(r, g, b)
                }
                // Named colours and Reset are already within the 16-colour set.
                _ => self,
            },
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── TrueColor is identity ─────────────────────────────────────────────

    #[test]
    fn truecolor_is_identity_for_all_variants() {
        for c in [
            Color::Reset, Color::Black, Color::LightRed, Color::White,
            Color::Gray, Color::DarkGray, Color::Indexed(42), Color::Rgb(100, 200, 50),
        ] {
            assert_eq!(c.downsample(ColorDepth::TrueColor), c,
                "{c:?} must pass through TrueColor unchanged");
        }
    }

    // ── Palette256 ────────────────────────────────────────────────────────

    #[test]
    fn palette256_pure_red_maps_to_cube_196() {
        // Cube (5,0,0): 16 + 36×5 + 6×0 + 0 = 196.
        assert_eq!(
            Color::Rgb(255, 0, 0).downsample(ColorDepth::Palette256),
            Color::Indexed(196),
        );
    }

    #[test]
    fn palette256_black_maps_to_indexed_16() {
        // Cube (0,0,0): 16 + 0 + 0 + 0 = 16.
        assert_eq!(
            Color::Rgb(0, 0, 0).downsample(ColorDepth::Palette256),
            Color::Indexed(16),
        );
    }

    #[test]
    fn palette256_midgray_maps_to_gray_ramp() {
        // Rgb(128,128,128): exact gray ramp match at 8+10×12 = 128 → index 244.
        let result = Color::Rgb(128, 128, 128).downsample(ColorDepth::Palette256);
        match result {
            Color::Indexed(i) => assert!(
                (232..=255).contains(&i),
                "midgray must land in the gray ramp (232–255), got {i}"
            ),
            other => panic!("expected Indexed(_), got {other:?}"),
        }
    }

    #[test]
    fn palette256_named_indexed_reset_unchanged() {
        for c in [Color::Red, Color::LightCyan, Color::Indexed(100), Color::Reset] {
            assert_eq!(c.downsample(ColorDepth::Palette256), c,
                "{c:?} must be unchanged in Palette256");
        }
    }

    // ── Palette16 ─────────────────────────────────────────────────────────

    #[test]
    fn palette16_red_rgb_maps_to_light_red() {
        assert_eq!(Color::Rgb(255, 0, 0).downsample(ColorDepth::Palette16), Color::LightRed);
    }

    #[test]
    fn palette16_black_rgb_maps_to_black() {
        assert_eq!(Color::Rgb(0, 0, 0).downsample(ColorDepth::Palette16), Color::Black);
    }

    #[test]
    fn palette16_white_rgb_maps_to_white() {
        assert_eq!(Color::Rgb(255, 255, 255).downsample(ColorDepth::Palette16), Color::White);
    }

    #[test]
    fn palette16_indexed_cube_red_maps_to_red_family() {
        // Indexed(196) = Rgb(255,0,0) in the cube → red variant.
        let result = Color::Indexed(196).downsample(ColorDepth::Palette16);
        assert!(
            matches!(result, Color::LightRed | Color::Red),
            "Indexed(196) must downsample to a red, got {result:?}",
        );
    }

    #[test]
    fn palette16_named_and_reset_unchanged() {
        for c in [
            Color::Black, Color::Red, Color::LightCyan, Color::Gray, Color::White, Color::Reset,
        ] {
            assert_eq!(c.downsample(ColorDepth::Palette16), c,
                "{c:?} must pass through Palette16 unchanged");
        }
    }
}
