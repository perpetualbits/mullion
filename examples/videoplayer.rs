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
}
