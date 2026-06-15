use std::io::{self, Write};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute, queue,
    style::{
        Attribute, Color as CtColor, Colors, Print, ResetColor, SetAttribute,
        SetColors,
    },
    terminal::{
        self, disable_raw_mode, enable_raw_mode, size, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};

use crate::{
    backend::Backend,
    buffer::Cell,
    geometry::Rect,
    style::{Color, Modifier, Style},
};

// Synchronized-output DEC private mode sequences.
const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
const END_SYNC: &[u8] = b"\x1b[?2026l";

/// A [`Backend`] that drives a real terminal via `crossterm`.
pub struct CrosstermBackend<W: Write> {
    writer: W,
    /// Last style emitted to the terminal; used to suppress redundant SGR sequences.
    last_style: Option<Style>,
}

impl<W: Write> CrosstermBackend<W> {
    pub fn new(writer: W) -> Self {
        Self { writer, last_style: None }
    }
}

fn to_ct_color(c: Color) -> CtColor {
    match c {
        Color::Reset => CtColor::Reset,
        Color::Black => CtColor::Black,
        Color::Red => CtColor::DarkRed,
        Color::Green => CtColor::DarkGreen,
        Color::Yellow => CtColor::DarkYellow,
        Color::Blue => CtColor::DarkBlue,
        Color::Magenta => CtColor::DarkMagenta,
        Color::Cyan => CtColor::DarkCyan,
        Color::White => CtColor::Grey,
        Color::DarkGray => CtColor::DarkGrey,
        Color::LightRed => CtColor::Red,
        Color::LightGreen => CtColor::Green,
        Color::LightYellow => CtColor::Yellow,
        Color::LightBlue => CtColor::Blue,
        Color::LightMagenta => CtColor::Magenta,
        Color::LightCyan => CtColor::Cyan,
        Color::Gray => CtColor::White,
        Color::Indexed(i) => CtColor::AnsiValue(i),
        Color::Rgb(r, g, b) => CtColor::Rgb { r, g, b },
    }
}

fn emit_style<W: Write>(w: &mut W, style: Style) -> io::Result<()> {
    // Reset all attributes first, then set the desired ones.
    queue!(w, ResetColor, SetAttribute(Attribute::Reset))?;

    queue!(
        w,
        SetColors(Colors::new(to_ct_color(style.fg), to_ct_color(style.bg)))
    )?;

    if style.mods.contains(Modifier::BOLD) {
        queue!(w, SetAttribute(Attribute::Bold))?;
    }
    if style.mods.contains(Modifier::DIM) {
        queue!(w, SetAttribute(Attribute::Dim))?;
    }
    if style.mods.contains(Modifier::ITALIC) {
        queue!(w, SetAttribute(Attribute::Italic))?;
    }
    if style.mods.contains(Modifier::UNDERLINE) {
        queue!(w, SetAttribute(Attribute::Underlined))?;
    }
    if style.mods.contains(Modifier::REVERSE) {
        queue!(w, SetAttribute(Attribute::Reverse))?;
    }
    Ok(())
}

impl<W: Write> Backend for CrosstermBackend<W> {
    fn size(&self) -> io::Result<Rect> {
        let (w, h) = size()?;
        Ok(Rect::new(0, 0, w, h))
    }

    fn draw<'a>(
        &mut self,
        changes: impl Iterator<Item = (u16, u16, &'a Cell)>,
    ) -> io::Result<()> {
        for (x, y, cell) in changes {
            queue!(self.writer, MoveTo(x, y))?;

            let style = cell.style;
            if self.last_style != Some(style) {
                emit_style(&mut self.writer, style)?;
                self.last_style = Some(style);
            }

            queue!(self.writer, Print(&cell.symbol))?;
        }
        Ok(())
    }

    fn begin_frame(&mut self) -> io::Result<()> {
        self.writer.write_all(BEGIN_SYNC)?;
        Ok(())
    }

    fn end_frame(&mut self) -> io::Result<()> {
        self.writer.write_all(END_SYNC)?;
        self.flush()
    }

    fn clear(&mut self) -> io::Result<()> {
        execute!(
            self.writer,
            terminal::Clear(terminal::ClearType::All),
            MoveTo(0, 0)
        )
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    fn enter(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(self.writer, EnterAlternateScreen, Hide)
    }

    fn leave(&mut self) -> io::Result<()> {
        // Best-effort: attempt each step even if a previous one failed.
        let r1 = execute!(self.writer, LeaveAlternateScreen, Show);
        let r2 = disable_raw_mode();
        r1.and(r2)
    }
}
