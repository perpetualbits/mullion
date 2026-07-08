# Video player example — design

**Date:** 2026-07-08
**Status:** approved, pre-implementation
**Deliverable:** `examples/videoplayer.rs` (new) + `[[example]]` entry in `Cargo.toml`. Library
(`src/`) is **not** modified — the player is built entirely from existing public API.

## Goal

Turn the `tv` example into a full terminal video player: real footage decoded through ffmpeg,
**with sound**, transport controls (play/pause, fast-forward, fast-backward, next, previous)
drawn as big on-video buttons usable by mouse, touch, and keyboard, an esc-to-hide behaviour that
restores the shell, optional SRT subtitles and optional per-track separate audio, and
`Encoding::LumaChroma` + `Dither::TemporalBayer` as the defaults.

`tv.rs` stays intact; this is a new sibling example that reuses its ffmpeg-frame-pump pattern.

## Reused mullion API (no library changes)

- `mullion::video::{Video, Encoding::LumaChroma, Dither::TemporalBayer, Sampling, Filter, Frame, Rgb}`
  — the render pipeline. `Video::new().encoding(..).dither(..).sampling(..).frame(u32).render_frame(buf, area, frame)`.
- `mullion::curve_map::{temporal_overlay, OverlayCell}` — the "temporal interleaving on characters"
  compositor (`OverlayCell { x, y, glyph, style, duty }`; `temporal_overlay(buf, &cells, phase)`).
  This is the same atom canopy's lasso callout uses.
- `mullion::{Terminal, Buffer, Rect, EventReader}` and `backend::CrosstermBackend`
  (`set_color_depth`, `set_mouse_capture` — mouse capture is on by default).
- Frame source pattern from `tv.rs`: ffmpeg → `rawvideo`/`rgb24` pipe → reader thread →
  `Arc<Mutex<Vec<u8>>>` → snapshot newest → `Frame::from_rgb`.

## CLI & playlist

```
cargo run --example videoplayer -- --file a.mp4,b.mp4,c.mp4
cargo run --example videoplayer -- --file "clip1.mp4:s:clip1.srt:a:song.mp3,clip2.mp4"
```

- `--file <csv>` parses a **comma-separated** list into a `Vec<Track>` playlist.
- We do **not** pass the whole list to a single ffmpeg. A single ffmpeg cannot cleanly support
  per-track next/prev seeking, so the playlist is **Rust-managed**: exactly one video ffmpeg + one
  audio ffplay for the *current* track at a time, respawned on control actions. (This is the
  fallback plan for "ffmpeg can't take multiple files as a seekable playlist".)
- Missing `--file`, empty list, or ffmpeg/ffplay not on PATH → a clear stderr error **before**
  entering the alternate screen (so the message is visible).

### Track entry syntax

Each comma-separated entry is a **compound** describing one track's sources:

```
<video>[:s:<subtitle.srt>][:a:<audio-file>]
```

- `<video>` — the video file (required; the first token, up to the first `:s:`/`:a:` marker).
- `:s:<subtitle.srt>` — optional SRT subtitle file. Missing → no subtitles for that track.
- `:a:<audio-file>` — optional **separate** audio file to play *instead of* the video's own
  audio. Missing → audio comes from the video file itself (as before).
- Markers may appear in either order; the parser scans for the literal `:s:` and `:a:` markers.
  (Paths containing `:s:`/`:a:` are unsupported — acceptable for an example.)

```rust
struct Track { video: PathBuf, subtitle: Option<PathBuf>, audio: Option<PathBuf> }
```

The **audio source file** for a track is `track.audio.clone().unwrap_or(track.video.clone())`.
The video's own embedded audio is never played directly — ffmpeg only pipes rawvideo (no audio);
all sound comes from ffplay, pointed at either the separate audio file or the video file.

## Playback engine — respawn at timestamp

Single source of truth:

- `playlist: Vec<PathBuf>`, `index: usize`
- `media_pos: f64` — current position in seconds within the track
- `speed: f32` — `1.0` normal; `⏩` cycles `1 → 2 → 4 → 1`; `⏪` cycles `1 → -2 → -4 → 1`
- `paused: bool`
- `last_spawn: Instant` — wall time the current children were spawned at, to advance `media_pos`

Each loop, when playing: `media_pos += speed as f64 * dt` (dt = wall time since last tick).

**Any** control (play/pause, speed change, next/prev, resume-from-hide) kills both child processes
and respawns from the new `(track, media_pos, speed)`:

- **Video:** `ffmpeg -loglevel error -readrate <|speed|.max(1)> -ss <media_pos> -i <file>
  -vf scale=W:H -pix_fmt rgb24 -f rawvideo -`
  (`-readrate 1.0` is the modern spelling of the old `-re`; ffmpeg ≥ 5.1. `-ss` before `-i` is a
  fast keyframe seek.)
- **Audio:** `ffplay -loglevel error -nodisp -vn -autoexit -ss <media_pos> -af atempo=<speed>
  <audio-source-file>` — spawned only when audio should sound (see the reverse and
  audio-exhaustion rules). `<audio-source-file>` is the track's separate `:a:` file if present,
  else the video file.

ffmpeg is **not** looped (`tv.rs`'s `-stream_loop -1` is dropped): each track's video plays once
so we can detect its end. Reader thread + newest-frame snapshot are otherwise identical to
`tv.rs`, generalised so the target `(w, h)` and both child handles live in a small `Engine` struct
whose `Drop` kills both children.

### End of video → auto-advance

Each loop, poll the video child with `try_wait()`. If it exited **on its own** (not because we
killed it for a respawn) and the pipe has drained, the track finished → auto-advance to the next
track (wrap), `media_pos = 0`, `speed = 1`, respawn. Because advancing kills the current ffplay,
**separate audio always stops at the video's end and never carries into the next track** (the
"video shorter than audio" rule).

### Separate-audio exhaustion (audio shorter than video)

Per track, track an `audio_exhausted: bool` (reset on track change). Each loop poll the audio child
with `try_wait()`; if it exited on its own (reached its end, or `-ss` seeked past its end), set
`audio_exhausted = true`. While `audio_exhausted`, respawns skip ffplay entirely — so a short audio
file **plays once and stops, never loops**, while the longer video keeps playing silently (the
"video longer than audio" rule). `we_killed` flag distinguishes our respawn-kills from self-exit.

### Forward fast-forward (speed 2, 4)

`-readrate <speed>` paces ffmpeg's input at `speed ×` realtime, so the rawvideo pipe emits frames
`speed ×` faster; the render loop keeps sampling newest at ~30 fps → smooth fast motion. Audio
plays through `ffplay -af atempo=<speed>` (pitch-corrected). `media_pos` advances at `speed ×`.

### Fast-backward / rewind (speed -2, -4)

Streaming reverse is not feasible with ffmpeg, so rewind is **fast-rewind by periodic backward
re-seek**: `media_pos += speed * dt` (speed negative → position moves backward), and ffmpeg is
respawned every ~250 ms at the new earlier `media_pos`. **Audio is muted while rewinding**
(no ffplay spawned). Reads as a normal fast-rewind. If `media_pos` reaches 0, clamp it to 0,
reset `speed` to 1, and resume normal play from the start of the track.

### Next / previous

`index` moves ±1 with wraparound over the playlist; `media_pos` resets to 0, `speed` to 1, then
respawn. (Simple wrap keeps a short playlist looping for demos.)

## Control bar — big buttons via `temporal_overlay`

Five big buttons across the **middle 50 %** of the terminal width (≈ ¼ margin each side):

```
        ┌──────┐┌──────┐┌──────┐┌──────┐┌──────┐
        │  ⏮  ││  ⏪  ││ ⏯  ││  ⏩  ││  ⏭  │   (each ~5 rows tall, block-art glyph)
        └──────┘└──────┘└──────┘└──────┘└──────┘
```

- Each button is a rounded box (~5 rows tall, width = bar_width / 5) with a **block-art transport
  glyph** stamped in its centre (drawn from block/box characters so it reads "big", not a single
  small codepoint).
- Composited straight over the already-rendered video frame with `temporal_overlay`:
  box **border** cells `duty ≈ 0.85`, box **fill** cells `duty ≈ 0.5` (video breathes through),
  **glyph** cells `duty 1.0` (never flicker). `phase` advances each frame from the same clock as
  the video dither, so the chrome "breathes" like the lasso callout.
- The ⏯ (centre) glyph shows ▶ when paused, ❚❚ when playing.
- A one-line status readout (track name, `mm:ss / mm:ss`-ish position, current speed, encoding/
  dither) sits with the bar and hides/shows with it.

### Auto-hide

- `last_activity: Instant`, updated on any key / mouse-move / mouse-down / touch.
- Bar + status are drawn only while `now - last_activity < 3s`.
- Any input reveals them. A **click while the bar is hidden only reveals** (does not trigger a
  button); a click while visible hit-tests the buttons and triggers.

## Subtitles (SRT, parsed + crisp overlay)

When a track has a `:s:` subtitle file, parse it once (on track load) into a `Vec<Cue>`:

```rust
struct Cue { start: f64, end: f64, lines: Vec<String> } // seconds
```

- **Parser:** minimal SRT — blocks separated by blank lines; a `HH:MM:SS,mmm --> HH:MM:SS,mmm`
  timing line; one or more text lines; the numeric index line is ignored. Basic `<i>/<b>`-style
  tags are stripped. Parse failures for a block are skipped (best-effort), never fatal.
- **Selection:** each frame, the active cue is the one with `cue.start <= media_pos < cue.end`
  (linear scan; playlists are short). Because timing is keyed on our own `media_pos`, subtitles
  stay correct through seek / FF / rewind automatically.
- **Shorter/longer rules fall out for free:** a subtitle track shorter than the video simply has no
  cue past its last `end`, so nothing shows after it; cues beyond the video's end are never reached
  because the video ends (and the track advances) first. No looping, no carry-over.
- **Rendering:** the active cue is drawn **bottom-centre** of the video area (above the control
  bar), via `temporal_overlay`: a see-through dark band (`duty ≈ 0.5`, video breathes through)
  with **opaque** white glyphs (`duty 1.0`, never flicker), lines centred and wrapped to the video
  width. Subtitles are **content, not chrome** — they show regardless of the auto-hide state of the
  control bar.

## Input — mouse, touch, keyboard

- **Mouse & touch:** `Event::Mouse` with `MouseEventKind::Down(MouseButton::Left)` → hit-test
  `(column, row)` against the five button rects (own `rect.contains`-style check). Touchscreen taps
  arrive as the same mouse-down events on terminals that support the SGR mouse protocol, so mouse
  and touch share one code path. Mouse capture is enabled (default; `set_mouse_capture(true)`).
- **Keyboard:**
  - `Space` — play / pause (also un-hides + resumes from esc-hidden mode; see below)
  - `←` / `→` — ⏪ / ⏩ (cycle speed as above)
  - `,` / `.` — previous / next track
  - `Esc` — hide / exit (see below)
  - retained hidden power keys from `tv`: `e` encoding, `d` dither, `n` sampling,
    `c` colour depth, `1`–`6` filters
- **Defaults on start:** `Encoding::LumaChroma`, `Dither::TemporalBayer`.

## Esc-hide + 15-second timer

Two modes: **Active** (normal, on the alternate screen) and **Hidden** (paused, alternate screen
left so the prior shell content shows).

- **Esc while Active:** pause + kill both children; emit **only** `LeaveAlternateScreen`
  (raw mode and the background `EventReader` stay alive so keys still register); set
  `hide_deadline = now + 15s`; the loop stops calling `term.draw` (so it does not corrupt the
  shell). Cursor shown.
- **While Hidden:**
  - `Esc` again → exit the program.
  - `hide_deadline` elapses (15 s) → exit the program.
  - `Space` → `EnterAlternateScreen` + `term.clear()` (force full repaint), respawn children at
    `media_pos`, resume playing; clears `hide_deadline`; back to Active.
- Implementation note: we drive `LeaveAlternateScreen` / `EnterAlternateScreen` via
  `crossterm::execute!` on `io::stdout()` directly rather than `Terminal::leave()`, because
  `leave()` also disables raw mode (which would stop single-key input during the hidden window).
  Full `Terminal::leave()` (restore raw mode, cursor, screen) runs once at real program exit.

## Render loop shape (pseudocode)

```
term.enter()                         // alt screen, raw mode, mouse capture, hide cursor
engine.spawn()                       // ffmpeg + ffplay for track 0 at pos 0, speed 1
loop {
    dt = time since last tick
    for ev in input.drain() { handle key / mouse; update last_activity, engine, mode }
    if mode == Exit { break }
    engine.poll_children()               // video self-exit → auto-advance; audio self-exit → exhausted
    if playing { media_pos += speed * dt; maybe respawn (rewind re-seek cadence / speed change) }
    if mode == Active {
        frame = engine.newest_frame()
        term.draw(|buf| {
            Video::new().encoding(enc).dither(dith).sampling(samp).frame(n).render_frame(buf, video_area, &frame)
            if let Some(cue) = active_cue(&track.cues, media_pos) { temporal_overlay(buf, &subtitle_cells(cue, video_area, phase), phase) }
            if controls_visible { temporal_overlay(buf, &button_cells(&engine, phase), phase); draw_status(buf) }
        })
        n = n.wrapping_add(1); phase += phase_step
    } else { /* Hidden: no draw; check esc-again / 15s deadline / space */ }
    sleep(budget - elapsed)          // ~33 ms base
}
term.leave()                         // full restore on real exit
```

## Testing / verification

- No unit tests (example, external processes). Verify by running with a real clip and a
  multi-file playlist; drive each control and confirm behaviour end-to-end (`/verify`-style):
  play/pause, ⏩ 2×/4× with sound speeding, ⏪ fast-rewind (muted), next/prev switching tracks,
  mouse-click and keyboard on each button, auto-hide after 3 s and reveal, esc → shell reappears,
  esc-again exits, 15 s exits, space resumes from hidden. Also verify the compound `--file`
  entries: a `:s:` SRT shows/clears at the right times and survives seeks; a `:a:` separate audio
  plays over the video; audio shorter than video stops without looping; video shorter than audio
  cuts the audio at the video's end (no carry into the next track).
- `cargo build --example videoplayer` must succeed (additive-only gate: the crate still compiles).

## Non-goals (YAGNI)

- No seek bar scrubbing by mouse drag (progress readout only).
- Subtitles: **SRT only** (no `.ass`/`.vtt`), no styling/positioning tags, no subtitle-track
  selection within a container.
- No on-disk config, no volume control.
- No custom audio decoding — ffplay owns audio; mullion owns video pixels only.
