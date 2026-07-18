// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! A full terminal video player built on the `Video` widget and the temporal-overlay
//! text compositor. See docs/superpowers/specs/2026-07-08-videoplayer-design.md.

use std::io;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::{
    cursor::{Hide, Show},
    event::{Event, KeyCode, KeyEvent, MouseButton, MouseEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use mullion::video::{Frame, Rgb};
use mullion::{
    backend::{Backend, CrosstermBackend},
    style::{Color, ColorDepth, Modifier, Style},
    curve_map::{temporal_overlay, OverlayCell},
    video::{Dither, Encoding, Sampling, Video},
    Buffer, EventReader, Rect, Terminal,
};

/// One playlist entry: a video, plus optional separate subtitle (SRT) and audio files.
/// Syntax per comma-separated entry: `<video>[:s:<subs.srt>][:a:<audio>]` (markers in any order).
struct Track {
    video: PathBuf,
    subtitle: Option<PathBuf>,
    audio: Option<PathBuf>,
}

/// Split a `--file` value on commas into one `Track` per entry.
fn parse_tracks(spec: &str) -> Vec<Track> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_entry)
        .collect()
}

/// Parse one entry: the video is everything up to the first `:s:`/`:a:` marker; each marker
/// introduces the corresponding optional file. Markers may appear in either order.
fn parse_entry(entry: &str) -> Track {
    // Find the earliest marker to end the video path.
    let s_pos = entry.find(":s:");
    let a_pos = entry.find(":a:");
    let video_end = [s_pos, a_pos].into_iter().flatten().min().unwrap_or(entry.len());
    let video = PathBuf::from(entry[..video_end].trim());
    let field = |marker: &str, pos: Option<usize>| -> Option<PathBuf> {
        let start = pos? + marker.len();
        // The field runs until the next marker after it, or end of string.
        let rest = &entry[start..];
        let next = [rest.find(":s:"), rest.find(":a:")].into_iter().flatten().min().unwrap_or(rest.len());
        let val = rest[..next].trim();
        (!val.is_empty()).then(|| PathBuf::from(val))
    };
    Track { video, subtitle: field(":s:", s_pos), audio: field(":a:", a_pos) }
}

/// A subtitle cue with times in seconds.
#[derive(Debug, Clone, PartialEq)]
struct Cue {
    start: f64,
    end: f64,
    lines: Vec<String>,
}

/// Parse `HH:MM:SS,mmm` (or `.mmm`) into seconds.
fn parse_ts(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let (hms, frac) = s.split_once('.').unwrap_or((s.as_str(), "0"));
    let parts: Vec<&str> = hms.split(':').collect();
    let [h, m, sec] = parts.as_slice() else { return None };
    let secs = h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?;
    let ms = format!("0.{frac}").parse::<f64>().ok()?;
    Some(secs + ms)
}

/// Minimal SRT parser: blank-line-separated blocks, each with an optional index line, a
/// `start --> end` timing line, and one or more text lines. Blocks without a valid timing
/// line are skipped (best-effort, never panics). Basic `<...>` tags are stripped.
fn parse_srt(text: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    for block in text.split("\n\n").flat_map(|b| b.split("\r\n\r\n")) {
        let mut lines = block.lines().filter(|l| !l.trim().is_empty());
        // Skip a leading numeric index line if present.
        let mut first = match lines.next() {
            Some(l) => l,
            None => continue,
        };
        if first.trim().parse::<u32>().is_ok() {
            first = match lines.next() {
                Some(l) => l,
                None => continue,
            };
        }
        let Some((a, b)) = first.split_once("-->") else { continue };
        let (Some(start), Some(end)) = (parse_ts(a), parse_ts(b)) else { continue };
        let text_lines: Vec<String> = lines.map(strip_tags).collect();
        if !text_lines.is_empty() {
            cues.push(Cue { start, end, lines: text_lines });
        }
    }
    cues
}

/// Remove `<...>` markup from a subtitle line.
fn strip_tags(line: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in line.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// The cue active at media time `t` (seconds), if any: `start <= t < end`.
fn active_cue(cues: &[Cue], t: f64) -> Option<&Cue> {
    cues.iter().find(|c| c.start <= t && t < c.end)
}

/// Fast-forward speed cycle: anything ≤1 (incl. rewind) → 2, 2 → 4, 4 → back to 1.
fn ff_speed(s: f32) -> f32 {
    if s >= 4.0 { 1.0 } else if s >= 2.0 { 4.0 } else { 2.0 }
}

/// Rewind speed cycle: anything ≥1 (incl. forward FF) → -2, -2 → -4, -4 → back to 1.
fn rw_speed(s: f32) -> f32 {
    if s <= -4.0 { 1.0 } else if s <= -2.0 { -4.0 } else { -2.0 }
}

/// Format a duration in seconds as `m:ss` (or `h:mm:ss` past an hour).
fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{sec:02}") } else { format!("{m}:{sec:02}") }
}

/// The five transport buttons, left to right.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Button { Prev, Rewind, PlayPause, Forward, Next }

const BUTTONS: [Button; 5] = [Button::Prev, Button::Rewind, Button::PlayPause, Button::Forward, Button::Next];

/// The control-bar rect: middle 50% of the width (¼ margin each side), 5 rows tall, a couple
/// rows above the bottom edge.
fn bar_area(area: Rect) -> Rect {
    let w = area.width / 2;
    let x = area.width / 4;
    let h = 5.min(area.height);
    let y = area.height.saturating_sub(h + 2);
    Rect::new(x, y, w, h)
}

/// Split the bar into five equal button columns (the last absorbs any remainder).
fn button_rects(bar: Rect) -> Vec<(Button, Rect)> {
    let bw = bar.width / 5;
    BUTTONS
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let x = bar.x + i as u16 * bw;
            let w = if i == 4 { bar.width - 4 * bw } else { bw };
            (*b, Rect::new(x, bar.y, w, bar.height))
        })
        .collect()
}

/// Point-in-rect (half-open on the far edges).
fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Which button (if any) covers cell `(x, y)`.
fn hit_test(rects: &[(Button, Rect)], x: u16, y: u16) -> Option<Button> {
    rects.iter().find(|(_, r)| contains(*r, x, y)).map(|(b, _)| *b)
}

const FF_W: usize = 320;
const FF_H: usize = 180;

/// Load and parse a track's SRT file into cues (empty if none / unreadable).
fn load_cues(track: &Track) -> Vec<Cue> {
    match &track.subtitle {
        Some(p) => std::fs::read_to_string(p).map(|s| parse_srt(&s)).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Owns the child processes and the playback clock. One ffmpeg (video → rawvideo pipe) and one
/// ffplay (audio) for the current track, respawned from `media_pos` on every control action.
struct Engine {
    playlist: Vec<Track>,
    index: usize,
    media_pos: f64,
    speed: f32,
    paused: bool,
    w: usize,
    h: usize,
    shared: Arc<Mutex<Vec<u8>>>,
    video: Option<Child>,
    audio: Option<Child>,
    audio_exhausted: bool,
    last_tick: Instant,
    last_seek: Instant,
    cues: Vec<Cue>,
}

impl Engine {
    fn new(playlist: Vec<Track>, w: usize, h: usize) -> Self {
        let cues = load_cues(&playlist[0]);
        let mut e = Engine {
            playlist,
            index: 0,
            media_pos: 0.0,
            speed: 1.0,
            paused: false,
            w,
            h,
            shared: Arc::new(Mutex::new(vec![0u8; w * h * 3])),
            video: None,
            audio: None,
            audio_exhausted: false,
            last_tick: Instant::now(),
            last_seek: Instant::now(),
            cues,
        };
        e.spawn_current();
        e
    }

    /// Spawn ffmpeg (always) and ffplay (only when audio should sound) from `media_pos`.
    fn spawn_current(&mut self) {
        let frame_len = self.w * self.h * 3;
        self.shared = Arc::new(Mutex::new(vec![0u8; frame_len])); // fresh: no stale frames
        let track = &self.playlist[self.index];
        let rate = self.speed.abs().max(1.0);
        let pos = self.media_pos.max(0.0);
        let mut vchild = Command::new("ffmpeg")
            .args([
                "-loglevel", "error",
                "-readrate", &format!("{rate}"),
                "-ss", &format!("{pos}"),
                "-i", track.video.to_str().unwrap_or_default(),
                "-vf", &format!("scale={}:{}", self.w, self.h),
                "-pix_fmt", "rgb24",
                "-f", "rawvideo",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ffmpeg");
        if let Some(out) = vchild.stdout.take() {
            let shared = Arc::clone(&self.shared);
            thread::spawn(move || {
                let mut out = out;
                let mut buf = vec![0u8; frame_len];
                while out.read_exact(&mut buf).is_ok() {
                    if let Ok(mut g) = shared.lock() {
                        g.copy_from_slice(&buf);
                    }
                }
            });
        }
        self.video = Some(vchild);

        let want_audio = self.speed > 0.0 && !self.paused && !self.audio_exhausted;
        self.audio = if want_audio {
            let src = track.audio.clone().unwrap_or_else(|| track.video.clone());
            // atempo caps at 2.0 per instance on older ffmpeg; chain two for 4×.
            let atempo = if self.speed >= 4.0 {
                "atempo=2.0,atempo=2.0".to_string()
            } else if self.speed > 1.0 {
                format!("atempo={}", self.speed)
            } else {
                "atempo=1.0".to_string()
            };
            Command::new("ffplay")
                .args([
                    "-loglevel", "error",
                    "-nodisp", "-vn", "-autoexit",
                    "-ss", &format!("{pos}"),
                    "-af", &atempo,
                    src.to_str().unwrap_or_default(),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        } else {
            None
        };
        self.last_tick = Instant::now();
        self.last_seek = Instant::now();
    }

    fn kill(&mut self) {
        if let Some(mut c) = self.video.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(mut c) = self.audio.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn respawn(&mut self) {
        self.kill();
        self.spawn_current();
    }

    /// Snapshot the newest decoded frame into a `Frame`.
    fn newest_frame(&self) -> Frame {
        let g = self.shared.lock().unwrap();
        let pixels: Vec<Rgb> = (0..self.w * self.h).map(|i| (g[i * 3], g[i * 3 + 1], g[i * 3 + 2])).collect();
        Frame::from_rgb(self.w, self.h, pixels)
    }

    /// Poll children: a video that exited on its own → the track finished → advance; an audio
    /// that exited on its own → it ran out → mark exhausted (don't respawn it for this track).
    fn poll(&mut self) {
        if let Some(ch) = self.video.as_mut() {
            if matches!(ch.try_wait(), Ok(Some(_))) {
                self.next();
                return;
            }
        }
        if let Some(ch) = self.audio.as_mut() {
            if matches!(ch.try_wait(), Ok(Some(_))) {
                self.audio = None;
                self.audio_exhausted = true;
            }
        }
    }

    /// Advance the media clock; handle the rewind re-seek cadence and the clamp at zero.
    fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64();
        self.last_tick = now;
        if self.paused {
            return;
        }
        self.media_pos += self.speed as f64 * dt;
        if self.media_pos <= 0.0 {
            self.media_pos = 0.0;
            self.speed = 1.0;
            self.respawn();
            return;
        }
        // Rewind can't stream backward; respawn ffmpeg at the new earlier position ~4×/sec.
        if self.speed < 0.0 && now.duration_since(self.last_seek) >= Duration::from_millis(250) {
            self.respawn();
        }
    }

    fn set_speed(&mut self, s: f32) {
        self.speed = s;
        if !self.paused {
            self.respawn();
        }
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if self.paused {
            self.kill();
        } else {
            self.respawn();
        }
    }

    fn goto(&mut self, index: usize) {
        self.index = index % self.playlist.len();
        self.media_pos = 0.0;
        self.speed = 1.0;
        self.audio_exhausted = false;
        self.paused = false;
        self.cues = load_cues(&self.playlist[self.index]);
        self.respawn();
    }

    fn next(&mut self) {
        self.goto(self.index + 1);
    }

    fn prev(&mut self) {
        self.goto(self.index + self.playlist.len() - 1);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.kill();
    }
}

/// View-side (non-playback) state: display modes and the auto-hide clock.
struct View {
    encoding: Encoding,
    dither: Dither,
    sampling: Sampling,
    depth: ColorDepth,
    filters: [bool; 6],
    frame: u32,
    phase: f32,
    last_activity: Instant,
}

impl View {
    fn new() -> Self {
        View {
            encoding: Encoding::LumaChroma,
            dither: Dither::TemporalBayer,
            sampling: Sampling::default(),
            depth: ColorDepth::TrueColor,
            filters: [false; 6],
            frame: 0,
            phase: 0.0,
            last_activity: Instant::now(),
        }
    }
    /// The control bar + status are shown for ~3s after the last input.
    fn controls_visible(&self) -> bool {
        self.last_activity.elapsed() < Duration::from_secs(3)
    }
}

/// Three-row block-art for a button's glyph. PlayPause depends on `playing`.
fn art(b: Button, playing: bool) -> [&'static str; 3] {
    match b {
        Button::Prev => ["▐◀ ", "▐◀◀", "▐◀ "],
        Button::Rewind => ["◀◀ ", "◀◀◀", "◀◀ "],
        Button::PlayPause if playing => ["▐ ▐", "▐ ▐", "▐ ▐"],
        Button::PlayPause => [" ▶ ", " ▶▶", " ▶ "],
        Button::Forward => ["▶▶ ", "▶▶▶", "▶▶ "],
        Button::Next => ["▶▶▐", "▶▶▐", "▶▶▐"],
    }
}

/// Overlay cells for a rounded box: a near-opaque border and a see-through fill.
fn box_cells(r: Rect, style: Style, border_duty: f32, fill_duty: f32) -> Vec<OverlayCell> {
    if r.width < 2 || r.height < 2 {
        return Vec::new();
    }
    let mut cells = Vec::new();
    let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.width - 1, r.y + r.height - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let glyph = match (x, y) {
                _ if x == x0 && y == y0 => '╭',
                _ if x == x1 && y == y0 => '╮',
                _ if x == x0 && y == y1 => '╰',
                _ if x == x1 && y == y1 => '╯',
                _ if y == y0 || y == y1 => '─',
                _ if x == x0 || x == x1 => '│',
                _ => ' ',
            };
            let border = glyph != ' ';
            cells.push(OverlayCell {
                x,
                y,
                glyph,
                style,
                duty: if border { border_duty } else { fill_duty },
            });
        }
    }
    cells
}

/// Overlay cells for all five big buttons composited over the video.
fn button_cells(eng: &Engine, area: Rect) -> Vec<OverlayCell> {
    let bar = bar_area(area);
    let playing = !eng.paused;
    let chrome = Style::default().fg(Color::White);
    let glyph_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let mut cells = Vec::new();
    for (b, r) in button_rects(bar) {
        if r.width < 3 || r.height < 3 {
            continue;
        }
        cells.extend(box_cells(r, chrome, 0.85, 0.5));
        // Stamp the 3-row art centred in the box interior.
        let lines = art(b, playing);
        let art_w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let cx = r.x + r.width.saturating_sub(art_w) / 2;
        let cy = r.y + r.height.saturating_sub(3) / 2;
        for (dy, line) in lines.iter().enumerate() {
            for (dx, ch) in line.chars().enumerate() {
                if ch != ' ' {
                    cells.push(OverlayCell { x: cx + dx as u16, y: cy + dy as u16, glyph: ch, style: glyph_style, duty: 1.0 });
                }
            }
        }
    }
    cells
}

/// Overlay cells for the active subtitle cue: a see-through dark band with opaque white text,
/// centred just above the control bar.
fn subtitle_cells(cue: &Cue, area: Rect) -> Vec<OverlayCell> {
    let bar = bar_area(area);
    let max_w = area.width.saturating_sub(4).max(1) as usize;
    // Wrap each source line to the video width.
    let mut wrapped: Vec<String> = Vec::new();
    for line in &cue.lines {
        let mut cur = String::new();
        for word in line.split_whitespace() {
            if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max_w {
                wrapped.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        if !cur.is_empty() {
            wrapped.push(cur);
        }
    }
    let n = wrapped.len() as u16;
    if n == 0 {
        return Vec::new();
    }
    let top = bar.y.saturating_sub(n + 1);
    let band = Style::default().fg(Color::White).bg(Color::Rgb(10, 10, 10));
    let text = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let mut cells = Vec::new();
    for (i, line) in wrapped.iter().enumerate() {
        let y = top + i as u16;
        let lw = line.chars().count() as u16;
        let start = area.x + area.width.saturating_sub(lw) / 2;
        // See-through band across the text span (+1 cell padding each side).
        for x in start.saturating_sub(1)..(start + lw + 1).min(area.x + area.width) {
            cells.push(OverlayCell { x, y, glyph: ' ', style: band, duty: 0.5 });
        }
        for (dx, ch) in line.chars().enumerate() {
            cells.push(OverlayCell { x: start + dx as u16, y, glyph: ch, style: text, duty: 1.0 });
        }
    }
    cells
}

/// Draw one full frame: video, then subtitles, then (if visible) the control bar + status.
fn render(buf: &mut Buffer, eng: &Engine, view: &View) {
    let area = buf.area;
    if area.height < 6 {
        return;
    }
    let frame = eng.newest_frame();
    let mut video = Video::new()
        .encoding(view.encoding)
        .dither(view.dither)
        .sampling(view.sampling)
        .frame(view.frame);
    // Faithful by default; filters are the hidden power-keys extras.
    let filter_list = [
        mullion::video::Filter::Scanlines(0.4),
        mullion::video::Filter::Vignette(0.6),
        mullion::video::Filter::Phosphor { hue: 40.0, sat: 0.7 },
        mullion::video::Filter::Gamma(1.8),
        mullion::video::Filter::Saturation(1.8),
        mullion::video::Filter::Grayscale,
    ];
    for (i, &on) in view.filters.iter().enumerate() {
        if on {
            video = video.filter(filter_list[i]);
        }
    }
    video.render_frame(buf, area, &frame);

    // Subtitles are content: always shown regardless of auto-hide.
    if let Some(cue) = active_cue(&eng.cues, eng.media_pos) {
        temporal_overlay(buf, &subtitle_cells(cue, area), view.phase);
    }

    if view.controls_visible() {
        temporal_overlay(buf, &button_cells(eng, area), view.phase);
        // Status line at the very bottom.
        let track = &eng.playlist[eng.index];
        let name = track.video.file_name().and_then(|s| s.to_str()).unwrap_or("?");
        let speed = if eng.paused { "paused".to_string() } else { format!("{}x", eng.speed) };
        let status = format!(
            " {}/{}  {}  {}  {}   space play/pause · ←/→ speed · ,/. prev/next · e/d/n/c/1-6 power · esc hide",
            eng.index + 1, eng.playlist.len(), name, fmt_time(eng.media_pos), speed,
        );
        let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
        for x in 0..area.width {
            buf.set_string(x, area.height - 1, " ", sstyle);
        }
        buf.set_string(0, area.height - 1, &status, sstyle);
    }
}

/// Is `bin` runnable (on PATH)? Probes `bin -version`.
fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let spec = args.windows(2).find(|w| w[0] == "--file").map(|w| w[1].clone());
    let playlist = match spec.as_deref().map(parse_tracks) {
        Some(t) if !t.is_empty() => t,
        _ => {
            eprintln!("videoplayer: usage: --file <video[:s:subs.srt][:a:audio]>[,<...>]");
            std::process::exit(2);
        }
    };
    if !have("ffmpeg") || !have("ffplay") {
        eprintln!("videoplayer: needs both `ffmpeg` and `ffplay` on PATH.");
        std::process::exit(1);
    }

    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;
    term.enter()?;
    let eng = Engine::new(playlist, FF_W, FF_H);
    let result = run(&mut term, eng, View::new());
    term.leave()?;
    result
}

fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>, mut eng: Engine, mut view: View) -> io::Result<()> {
    let input = EventReader::new();
    let budget = Duration::from_millis(33); // ~30 fps
    let mut hidden = false;
    let mut hide_deadline: Option<Instant> = None;

    loop {
        let start = Instant::now();

        for ev in input.drain() {
            match ev {
                Event::Key(KeyEvent { code, .. }) => {
                    view.last_activity = Instant::now();
                    match code {
                        KeyCode::Esc => {
                            if hidden {
                                return Ok(()); // second esc → exit
                            }
                            // First esc: pause, leave the alt screen (keep raw mode + input),
                            // start the 15s timer, stop drawing.
                            eng.paused = true;
                            eng.kill();
                            execute!(io::stdout(), LeaveAlternateScreen, Show)?;
                            hidden = true;
                            hide_deadline = Some(Instant::now() + Duration::from_secs(15));
                        }
                        KeyCode::Char(' ') => {
                            if hidden {
                                // Resume from hidden: re-enter alt screen, repaint, play.
                                execute!(io::stdout(), EnterAlternateScreen, Hide)?;
                                term.clear()?;
                                hidden = false;
                                hide_deadline = None;
                                eng.paused = false;
                                eng.respawn();
                            } else {
                                eng.toggle_pause();
                            }
                        }
                        _ if hidden => {} // other keys are inert while hidden
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Left => eng.set_speed(rw_speed(eng.speed)),
                        KeyCode::Right => eng.set_speed(ff_speed(eng.speed)),
                        KeyCode::Char(',') => eng.prev(),
                        KeyCode::Char('.') => eng.next(),
                        // Hidden power keys (defaults stay luma-chroma + temporal).
                        KeyCode::Char('e') => {
                            view.encoding = match view.encoding {
                                Encoding::Braille => Encoding::HalfBlock,
                                Encoding::HalfBlock => Encoding::LumaChroma,
                                Encoding::LumaChroma => Encoding::Sextant,
                                Encoding::Sextant => Encoding::Braille,
                            }
                        }
                        KeyCode::Char('d') => {
                            view.dither = match view.dither {
                                Dither::Bayer => Dither::FloydSteinberg,
                                Dither::FloydSteinberg => Dither::TemporalBayer,
                                Dither::TemporalBayer => Dither::Bayer,
                            }
                        }
                        KeyCode::Char('n') => {
                            view.sampling = match view.sampling {
                                Sampling::Bilinear => Sampling::Nearest,
                                Sampling::Nearest => Sampling::Bilinear,
                            }
                        }
                        KeyCode::Char('c') => {
                            view.depth = match view.depth {
                                ColorDepth::TrueColor => ColorDepth::Palette256,
                                ColorDepth::Palette256 => ColorDepth::Palette16,
                                ColorDepth::Palette16 => ColorDepth::TrueColor,
                            };
                            term.backend_mut().set_color_depth(view.depth);
                        }
                        KeyCode::Char(ch @ '1'..='6') => view.filters[ch as usize - '1' as usize] ^= true,
                        _ => {}
                    }
                }
                Event::Mouse(me) => {
                    // Sample visibility BEFORE resetting the activity clock, so a click that
                    // merely wakes the auto-hidden bar reveals it without also firing a button.
                    let was_visible = view.controls_visible();
                    view.last_activity = Instant::now();
                    if !hidden && was_visible && matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) {
                        let rects = button_rects(bar_area(Rect::new(0, 0, term_w(term)?, term_h(term)?)));
                        if let Some(b) = hit_test(&rects, me.column, me.row) {
                            match b {
                                Button::Prev => eng.prev(),
                                Button::Rewind => eng.set_speed(rw_speed(eng.speed)),
                                Button::PlayPause => eng.toggle_pause(),
                                Button::Forward => eng.set_speed(ff_speed(eng.speed)),
                                Button::Next => eng.next(),
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if hidden {
            if hide_deadline.map(|d| Instant::now() >= d).unwrap_or(false) {
                return Ok(()); // 15s timeout → exit
            }
            thread::sleep(budget.saturating_sub(start.elapsed()));
            continue;
        }

        eng.poll();
        eng.tick();
        term.draw(|buf| render(buf, &eng, &view))?;
        view.frame = view.frame.wrapping_add(1);
        view.phase = (view.phase + 0.08).fract();
        thread::sleep(budget.saturating_sub(start.elapsed()));
    }
}

/// Current terminal width/height via the backend.
fn term_w(term: &Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<u16> {
    Ok(term.backend().size()?.width)
}
fn term_h(term: &Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<u16> {
    Ok(term.backend().size()?.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_list() {
        let t = parse_tracks("a.mp4,b.mp4, c.mp4 ");
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].video, PathBuf::from("a.mp4"));
        assert!(t[0].subtitle.is_none() && t[0].audio.is_none());
        assert_eq!(t[2].video, PathBuf::from("c.mp4"));
    }

    #[test]
    fn parse_subtitle_and_audio_any_order() {
        let t = parse_tracks("clip.mp4:s:clip.srt:a:song.mp3");
        assert_eq!(t[0].video, PathBuf::from("clip.mp4"));
        assert_eq!(t[0].subtitle, Some(PathBuf::from("clip.srt")));
        assert_eq!(t[0].audio, Some(PathBuf::from("song.mp3")));

        let t2 = parse_tracks("clip.mp4:a:song.mp3:s:clip.srt");
        assert_eq!(t2[0].subtitle, Some(PathBuf::from("clip.srt")));
        assert_eq!(t2[0].audio, Some(PathBuf::from("song.mp3")));
    }

    #[test]
    fn parse_only_subtitle() {
        let t = parse_tracks("clip.mp4:s:clip.srt");
        assert_eq!(t[0].subtitle, Some(PathBuf::from("clip.srt")));
        assert!(t[0].audio.is_none());
    }

    const SAMPLE_SRT: &str = "1\n00:00:01,000 --> 00:00:03,500\nHello there\n\n2\n00:00:04,000 --> 00:00:05,000\n<i>Second</i> line\nwrapped\n";

    #[test]
    fn parse_srt_two_cues() {
        let cues = parse_srt(SAMPLE_SRT);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, 1.0);
        assert_eq!(cues[0].end, 3.5);
        assert_eq!(cues[0].lines, vec!["Hello there".to_string()]);
        assert_eq!(cues[1].lines, vec!["Second line".to_string(), "wrapped".to_string()]);
    }

    #[test]
    fn active_cue_selection() {
        let cues = parse_srt(SAMPLE_SRT);
        assert!(active_cue(&cues, 0.5).is_none());
        assert_eq!(active_cue(&cues, 2.0).unwrap().start, 1.0);
        assert!(active_cue(&cues, 3.5).is_none()); // end is exclusive
        assert_eq!(active_cue(&cues, 4.2).unwrap().start, 4.0);
        assert!(active_cue(&cues, 99.0).is_none()); // past the last cue → nothing
    }

    #[test]
    fn speed_cycles() {
        assert_eq!(ff_speed(1.0), 2.0);
        assert_eq!(ff_speed(2.0), 4.0);
        assert_eq!(ff_speed(4.0), 1.0);
        assert_eq!(ff_speed(-2.0), 2.0); // forward from a rewind state
        assert_eq!(rw_speed(1.0), -2.0);
        assert_eq!(rw_speed(-2.0), -4.0);
        assert_eq!(rw_speed(-4.0), 1.0);
        assert_eq!(rw_speed(4.0), -2.0); // rewind from a fast-forward state
    }

    #[test]
    fn time_format() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(65.0), "1:05");
        assert_eq!(fmt_time(3661.0), "1:01:01");
    }

    #[test]
    fn bar_is_middle_half() {
        let bar = bar_area(Rect::new(0, 0, 80, 24));
        assert_eq!(bar.x, 20);
        assert_eq!(bar.width, 40); // middle 50%, 20 cols margin each side
        assert_eq!(bar.height, 5);
    }

    #[test]
    fn hit_test_maps_columns_to_buttons() {
        let bar = bar_area(Rect::new(0, 0, 80, 24));
        let rects = button_rects(bar);
        assert_eq!(rects.len(), 5);
        // Middle of the bar → the middle (PlayPause) button.
        assert_eq!(hit_test(&rects, 40, bar.y + 2), Some(Button::PlayPause));
        // Far left of the bar → Prev.
        assert_eq!(hit_test(&rects, bar.x, bar.y), Some(Button::Prev));
        // Outside the bar → nothing.
        assert_eq!(hit_test(&rects, 0, 0), None);
        assert_eq!(hit_test(&rects, 40, 0), None);
    }
}
