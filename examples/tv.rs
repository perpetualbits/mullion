// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! A TV for the [`Video`] widget. With no argument it plays a synthesised colour
//! signal (colour bars, a reference strip, a drifting luma ramp, a bouncing
//! highlight). Pass a **video file** and it plays real footage:
//!
//! ```text
//! cargo run --example tv -- /path/to/clip.mp4
//! ```
//!
//! That spawns `ffmpeg … -pix_fmt rgb24 -f rawvideo -` (needs `ffmpeg` on PATH) and
//! feeds its frames to the widget — mullion never decodes video itself.
//!
//! Keys
//!   e              encoding: braille → half-block → luma-chroma → sextant
//!   d              dither: Bayer (ordered) ↔ Floyd–Steinberg (error diffusion)
//!   n              sampling: bilinear ↔ nearest
//!   c              colour depth: truecolor → 256 → 16 (fewer output bytes, lower fidelity)
//!   1..6           toggle scanlines / vignette / phosphor / gamma / saturation / grey
//!   space          pause / resume
//!   q              quit

use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::{thread, time::{Duration, Instant}};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    style::{Color, ColorDepth, Modifier, Style},
    video::{Dither, Encoding, Filter, Frame, Rgb, Sampling, Video},
    EventReader,
    Buffer, Rect, Terminal,
};

const SYNTH_W: usize = 192;
const SYNTH_H: usize = 144;
const FF_W: usize = 320;
const FF_H: usize = 180;

const FILTERS: [Filter; 6] = [
    Filter::Scanlines(0.4),
    Filter::Vignette(0.6),
    Filter::Phosphor { hue: 40.0, sat: 0.7 },
    Filter::Gamma(1.8),
    Filter::Saturation(1.8),
    Filter::Grayscale,
];
const FILTER_NAMES: [&str; 6] = ["scanlines", "vignette", "phosphor", "gamma", "saturation", "greyscale"];

struct State {
    t: f32,
    encoding: Encoding,
    dither: Dither,
    sampling: Sampling,
    depth: ColorDepth,
    filters: [bool; 6],
    paused: bool,
}

// ── Frame sources ─────────────────────────────────────────────────────────────────

/// Where frames come from: a procedural test signal, or decoded footage.
enum Source {
    Synth,
    Ffmpeg(FfmpegSource),
}

impl Source {
    /// The current frame (advances footage / the synth clock).
    fn frame(&mut self, t: f32) -> Frame {
        match self {
            Source::Synth => synth_frame(t),
            Source::Ffmpeg(f) => {
                f.tick();
                f.frame()
            }
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Source::Synth => "synth",
            Source::Ffmpeg(_) => "ffmpeg",
        }
    }
}

/// Real footage: `ffmpeg … -pix_fmt rgb24 -f rawvideo -` decoded by a reader thread
/// into a shared buffer; [`tick`](FfmpegSource::tick) snapshots the newest frame. The
/// child is killed on drop.
struct FfmpegSource {
    w: usize,
    h: usize,
    pixels: Vec<Rgb>,
    shared: Arc<Mutex<Vec<u8>>>,
    child: Child,
}

impl FfmpegSource {
    fn new(path: &str, w: usize, h: usize) -> io::Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-loglevel", "error",
                "-re", // pace at native frame rate
                "-stream_loop", "-1", // loop forever
                "-i", path,
                "-vf", &format!("scale={w}:{h}"),
                "-pix_fmt", "rgb24",
                "-f", "rawvideo",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut out = child.stdout.take().ok_or_else(|| io::Error::other("ffmpeg produced no stdout"))?;
        let shared = Arc::new(Mutex::new(vec![0u8; w * h * 3]));
        let writer = Arc::clone(&shared);
        let frame_len = w * h * 3;
        thread::spawn(move || {
            let mut buf = vec![0u8; frame_len];
            while out.read_exact(&mut buf).is_ok() {
                if let Ok(mut g) = writer.lock() {
                    g.copy_from_slice(&buf);
                }
            }
        });
        Ok(Self { w, h, pixels: vec![(0, 0, 0); w * h], shared, child })
    }

    fn tick(&mut self) {
        if let Ok(g) = self.shared.lock() {
            for (i, px) in self.pixels.iter_mut().enumerate() {
                *px = (g[i * 3], g[i * 3 + 1], g[i * 3 + 2]);
            }
        }
    }

    fn frame(&self) -> Frame {
        Frame::from_rgb(self.w, self.h, self.pixels.clone())
    }
}

impl Drop for FfmpegSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The synthesised test signal at time `t`: SMPTE-ish colour bars over a reference
/// strip and a drifting luma ramp, with a bright disc bouncing across.
fn synth_frame(t: f32) -> Frame {
    const BARS: [Rgb; 7] = [
        (255, 255, 255), (255, 255, 0), (0, 255, 255), (0, 255, 0), (255, 0, 255), (255, 0, 0), (0, 0, 255),
    ];
    let (cx, cy) = (0.5 + 0.34 * (t * 0.7).sin(), 0.33 + 0.16 * (t * 1.1).cos());
    let pixels: Vec<Rgb> = (0..SYNTH_H)
        .flat_map(|y| (0..SYNTH_W).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (u, v) = ((x as f32 + 0.5) / SYNTH_W as f32, (y as f32 + 0.5) / SYNTH_H as f32);
            let base = if v < 0.66 {
                BARS[((u * 7.0) as usize).min(6)]
            } else if v < 0.74 {
                let g = if (u * 12.0) as usize % 2 == 0 { 210 } else { 20 };
                (g, g, g)
            } else {
                let g = ((u + t * 0.05).fract() * 255.0) as u8;
                (g, g, g)
            };
            if ((u - cx).powi(2) + (v - cy).powi(2) * 2.25).sqrt() < 0.07 {
                (255, 255, 255)
            } else {
                base
            }
        })
        .collect();
    Frame::from_rgb(SYNTH_W, SYNTH_H, pixels)
}

// ── Rendering ─────────────────────────────────────────────────────────────────────

fn frame_area(area: Rect) -> Rect {
    Rect::new(0, 1, area.width, area.height.saturating_sub(2))
}

fn render(buf: &mut Buffer, st: &State, frame: &Frame, source: &str) {
    let area = buf.area;
    if area.height < 4 {
        return;
    }
    let mut video = Video::new().encoding(st.encoding).dither(st.dither).sampling(st.sampling);
    for (i, &on) in st.filters.iter().enumerate() {
        if on {
            video = video.filter(FILTERS[i]);
        }
    }
    video.render_frame(buf, frame_area(area), frame);

    buf.set_string(0, 0, "tv — e:encoding  d:dither  n:sampling  c:colour  1-6:filters  space:pause  q:quit",
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let active: Vec<&str> = (0..6).filter(|&i| st.filters[i]).map(|i| FILTER_NAMES[i]).collect();
    let status = format!(" source: {}   encoding: {}   dither: {}   sampling: {}   colour: {}   filters: {}",
        source,
        match st.encoding { Encoding::Braille => "braille", Encoding::HalfBlock => "half-block", Encoding::LumaChroma => "luma-chroma", Encoding::Sextant => "sextant" },
        match st.dither { Dither::Bayer => "bayer", Dither::FloydSteinberg => "floyd-steinberg" },
        match st.sampling { Sampling::Bilinear => "bilinear", Sampling::Nearest => "nearest" },
        match st.depth { ColorDepth::TrueColor => "truecolor", ColorDepth::Palette256 => "256", ColorDepth::Palette16 => "16" },
        if active.is_empty() { "none (faithful)".to_string() } else { active.join(", ") });
    let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, area.height - 1, " ", sstyle);
    }
    buf.set_string(0, area.height - 1, &status, sstyle);
}

fn main() -> io::Result<()> {
    // Build the source before the alternate screen, so an ffmpeg-failure note is seen.
    let source = match std::env::args().nth(1) {
        Some(path) => match FfmpegSource::new(&path, FF_W, FF_H) {
            Ok(s) => Source::Ffmpeg(s),
            Err(e) => {
                eprintln!("tv: could not start ffmpeg for {path} ({e}); using the synth signal");
                Source::Synth
            }
        },
        None => Source::Synth,
    };
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;
    term.enter()?;
    let result = run(&mut term, source);
    term.leave()?;
    result
}

fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>, mut source: Source) -> io::Result<()> {
    // A background reader so a key only acts when you press it — even when a huge frame
    // is slow to draw (otherwise queued events replay in bursts and modes seem to flip
    // on their own).
    let input = EventReader::new();
    let budget = Duration::from_millis(60);
    let mut st = State { t: 0.0, encoding: Encoding::Braille, dither: Dither::default(), sampling: Sampling::default(), depth: ColorDepth::TrueColor, filters: [false; 6], paused: false };
    'frames: loop {
        let start = Instant::now();
        for ev in input.drain() {
            if let Event::Key(KeyEvent { code, .. }) = ev {
                match code {
                    KeyCode::Char('q') => break 'frames,
                    KeyCode::Char('e') => {
                        st.encoding = match st.encoding {
                            Encoding::Braille => Encoding::HalfBlock,
                            Encoding::HalfBlock => Encoding::LumaChroma,
                            Encoding::LumaChroma => Encoding::Sextant,
                            Encoding::Sextant => Encoding::Braille,
                        };
                    }
                    KeyCode::Char('d') => {
                        st.dither = match st.dither {
                            Dither::Bayer => Dither::FloydSteinberg,
                            Dither::FloydSteinberg => Dither::Bayer,
                        };
                    }
                    KeyCode::Char('n') => {
                        st.sampling = match st.sampling {
                            Sampling::Bilinear => Sampling::Nearest,
                            Sampling::Nearest => Sampling::Bilinear,
                        };
                    }
                    KeyCode::Char('c') => {
                        // Fewer SGR bytes per cell at lower depth — the lever for a
                        // huge, terminal-I/O-bound screen (at a cost in colour fidelity).
                        st.depth = match st.depth {
                            ColorDepth::TrueColor => ColorDepth::Palette256,
                            ColorDepth::Palette256 => ColorDepth::Palette16,
                            ColorDepth::Palette16 => ColorDepth::TrueColor,
                        };
                        term.backend_mut().set_color_depth(st.depth);
                    }
                    KeyCode::Char(' ') => st.paused = !st.paused,
                    KeyCode::Char(c @ '1'..='6') => st.filters[c as usize - '1' as usize] ^= true,
                    _ => {}
                }
            }
        }
        let frame = source.frame(st.t);
        let label = source.label();
        term.draw(|buf| render(buf, &st, &frame, label))?;
        if !st.paused {
            st.t += 0.08;
        }
        thread::sleep(budget.saturating_sub(start.elapsed()));
    }
    Ok(())
}
