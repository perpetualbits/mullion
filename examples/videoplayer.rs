// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! A full terminal video player built on the `Video` widget and the temporal-overlay
//! text compositor. See docs/superpowers/specs/2026-07-08-videoplayer-design.md.

use std::path::PathBuf;

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

fn main() {
    // Placeholder main — replaced in Task 7. For now, parse and print the playlist.
    let spec = std::env::args().skip(1).collect::<Vec<_>>().windows(2)
        .find(|w| w[0] == "--file").map(|w| w[1].clone());
    match spec.as_deref().map(parse_tracks) {
        Some(tracks) if !tracks.is_empty() => {
            for t in &tracks {
                println!("video={:?} sub={:?} audio={:?}", t.video, t.subtitle, t.audio);
            }
        }
        _ => eprintln!("videoplayer: pass --file a.mp4,b.mp4 (see the module docs)"),
    }
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
}
