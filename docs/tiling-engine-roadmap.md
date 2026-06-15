# Tiling Engine — Roadmap

> Crate name: **`mullion`**.
> A general, reusable terminal UI tiling engine in Rust. First consumer: **apptop**.
> Status: design phase. Programming is done by Claude Code; design, idea-shaping,
> and prompt-writing are done jointly.

---

## 1. Vision

A standalone library crate that gives a terminal program a **nested, resizable
tiling layout** of bordered tiles, where:

- the layout is a tree that grows and shrinks with the terminal as a whole;
- groups of tiles can **scroll as a unit** (a column that scrolls vertically, a
  row that scrolls horizontally) while each tile shows its own live content;
- every tile can carry **scrolling text labels in all four borders**;
- the user can **step focus through tiles** with the keyboard and act on the
  focused one;
- the structure can be **zoomed into and out of** as a first-class operation;
- borders can be drawn per-tile, or shared with proper junction glyphs
  (`├ ┤ ┬ ┴ ┼`) between sub-tiles of a group.

The engine owns layout, rendering, focus, input routing, and zoom. The
application supplies only **tile content** and **key handling for the focused
tile**. This split is what makes the engine reusable; apptop is just the first
thing plugged into it.

---

## 2. Goals and non-goals

### Goals
- Reusable across programs — the engine knows nothing about processes, metrics,
  or `/proc`. Clean library boundary.
- Fit-to-space tiling **and** overflow scrolling, modeled as distinct concepts.
- Correct Unicode handling (width, grapheme clusters) everywhere.
- Testable headlessly: render to an in-memory grid and assert on it.
- Degrades gracefully on dumb terminals (ASCII / no-Unicode fallback).
- Predictable performance with many tiles (virtualized scrolling groups).

### Non-goals (at least initially)
- **Not** a retained-mode widget toolkit (buttons, text inputs, focus rings for
  form controls). Tiles draw themselves; the engine arranges and frames them.
- **Not** a PTY multiplexer. Tiles are app-drawn surfaces, not embedded shells.
  (Hosting real subprocess terminals à la tmux is an explicit non-goal; revisit
  only if a future consumer needs it.)
- **Not** floating/overlapping windows. This is *tiling*. A later modal/overlay
  layer (for menus, the apptop manual, dialogs) is a possible stretch, kept
  separate from the tiling core.
- **Not** a CSS/flexbox reimplementation. The constraint model is deliberately
  small (ratios + min/max).

---

## 3. Core model

### 3.1 The layout tree

Three node kinds. Internal nodes arrange; leaves hold content.

- **`Split`** — an orientation (`Horizontal` / `Vertical` / `Adaptive`, see §3.7)
  plus children with ratio/min/max constraints. **Fills its rect exactly.** This
  is the "static layout that grows and shrinks with the terminal."
- **`Carousel`** — an orientation (`Horizontal` / `Vertical` / `Adaptive`) plus
  children that each have a **main-axis extent** (cells along the scroll
  direction; cross axis fills), plus a scroll offset. **Clips and scrolls**;
  natural size may exceed the viewport. This is the "column of tiles you scroll
  up/down" / "row you scroll left/right." Defining the child size as a main-axis
  extent (rather than a fixed width/height) is what lets a flip simply reinterpret
  which screen dimension is "main."
- **`Tile`** (leaf) — a content handle plus four optional border-label slots.

Composition examples (the user's two scenarios):

```
Split(H, [ Tile(big), Carousel(V, [t1, t2, t3, …]) ])   // big left, scroll column right
Split(V, [ Tile(big), Carousel(H, [t1, t2, t3, …]) ])   // big top,  scroll row below
```

The key design decision captured here: **fit-to-space (Split) and overflow
(Carousel) are separate node types**, not one mechanism with a flag. This keeps
sizing logic clean and makes virtualization a property of Carousel alone.

### 3.7 Reconfiguration: flip, swap, adaptive orientation

Both `Split` and `Carousel` carry an orientation that is one of
`Horizontal | Vertical | Adaptive`:

- **Flip** — a nav-mode key toggles the focused group's orientation
  (`Horizontal ↔ Vertical`). For a `Split` this swaps side-by-side ↔ stacked; for
  a `Carousel` it swaps a left-right scrolling row ↔ an up-down scrolling column.
  A flip mutates one field and re-solves; children and their constraints are
  untouched (this is why carousel children are sized by main-axis extent, §3.1).
- **Swap** — a nav-mode key swaps the focused tile with a chosen neighbor
  (reorder children / swap subtrees). Orthogonal to orientation.
- **Adaptive** — instead of a fixed direction, a node resolves its orientation
  each layout pass from the **aspect ratio of the rect it is handed** (default
  rule: lay out along the longer dimension).

`Adaptive` gives the wide-monitor → tall-monitor behavior **for free and
composably**. Set the root `Split` and the right-hand `Carousel` both to
`Adaptive`: on a wide terminal the root rect is wide → root lays out side-by-side
(left tile | right group); the group's rect is wide-ish → it scrolls left-right.
Move the terminal to a tall monitor → the root rect becomes tall → root stacks
(top tile / bottom group); the bottom rect is now tall-ish → the group scrolls
up-down. Because each node picks from the shape of the rect its parent gave it,
the parent's choice flows into the child's and the whole tree reorganizes from
one resize — no whole-tree special-casing.

**Hysteresis required:** the adaptive choice must use a dead zone (a breakpoint
with margin) near square, or the layout flickers when the terminal is dragged
across the threshold. A manual **flip** sets an explicit orientation and thereby
disables `Adaptive` for that node; a separate key restores `Adaptive`.

The engine owns the *mechanism* (flip/swap ops, adaptive resolution); the
specific adaptive *rule* and breakpoint are configurable, so policy can live in
the app where it belongs (see §8).

### 3.2 The render substrate

A **cell buffer**: a 2-D grid of styled cells (`char`/grapheme + fg + bg +
attrs). All drawing targets the buffer; the buffer is then flushed to the
terminal with a **diff** against the previous frame (only changed cells emit
escapes), wrapped in synchronized-output markers (`ESC[?2026h … l`) to avoid
tearing. This replaces btop's "rebuild one big string" approach, which we reject
because we have many independently-animating regions (border marquees, scrolling
carousels) that want localized partial redraws.

Two backends behind one trait:
- **Terminal backend** — real output (via `crossterm`): raw mode, alt screen,
  cursor hide, mouse, resize events, synchronized output.
- **Headless backend** — renders into a `String`/grid for snapshot tests.

### 3.3 The junction grid (shared borders)

For shared single borders with `├ ┤ ┬ ┴ ┼` dividers we maintain a **border edge
grid**: each cell records which of `{up, down, left, right}` carry a line
segment (and at what weight: light / heavy / double). After all tiles contribute
their edges, each border cell's bitmask is resolved to the correct box-drawing
glyph via a lookup table. This is the single hardest renderer component and
should exist from day one because nesting + shared borders require a global edge
grid anyway.

Two border modes, selectable per group:
- **Per-tile borders** — each tile owns a full box (btop-style). Simple; focus
  highlight is trivial (recolor the tile's own border). Adjacent tiles show
  doubled lines unless deliberately collapsed.
- **Shared borders** — one grid, junctions between sub-tiles. Denser and
  prettier. **Tension:** a shared edge belongs to two tiles, so you can't
  restyle one tile's border in isolation. Resolution: draw shared borders thin
  and normal, then for the focused tile **overlay** a heavy/colored rectangle on
  its four edges (visually "stealing" the shared edges while focused), repainting
  affected edges on focus change.

### 3.4 Focus, input, and the keybinding problem

Focus is a **path into the tree** (which leaf is active). Navigation:
- `Tab` / `Shift-Tab` — next/prev leaf in DFS order (simple, always works).
- Directional focus (`h/j/k/l` or arrows) — geometric neighbor search (nicer,
  optional, second pass).

**Keybinding collision is a design problem, not a detail.** In apptop, arrows and
`j/k` already mean "move cursor / cycle metric." Adding tile-stepping and
carousel-scrolling makes bare `↑/↓` three-ways ambiguous (move focus / scroll
group / move in-tile cursor). The engine therefore routes input through a
**modal layer**:
- A **navigation mode** (entered by a prefix, vim-window style, e.g. `Ctrl-w`
  then a direction) owns focus movement, zoom, and group scrolling.
- In normal mode, unhandled keys are **forwarded to the focused tile's content**,
  so the application's existing bindings keep working unchanged.

The engine defines the routing; the application registers content key handlers
and (optionally) overrides the default nav keymap.

### 3.5 Scroll ↔ focus coupling

Moving focus to a tile that is scrolled out of a Carousel's viewport
**auto-scrolls** the carousel to reveal it. Scrolling a carousel does **not**
move focus. These are independent but coupled in one direction.

### 3.6 Zoom (unified with drill-down)

Zoom = **re-root the view at the focused subtree**. Maintain a `view_root`
pointer plus a **zoom stack** separate from the tree itself:
- *Zoom in* — push the current view root, set view root to the focused node, so
  it fills the terminal.
- *Zoom out* — pop, restoring the previous root **and** the saved focus/scroll
  state of the rest of the tree (so context isn't lost).

**Architectural win:** apptop's existing Enter-to-drill-into-host/VM/pod and
Esc-to-return *is the same abstraction*. We unify drill-down and zoom into one
navigation stack over one tree, so `Esc` means exactly one thing everywhere.

---

## 4. Border labels (all four sides, scrolling)

- **Top / bottom (horizontal):** a marquee living in the border line, replacing a
  run of `─` (like btop's title-in-border). Clip to the available run with a
  scroll offset.
- **Left / right (vertical):** **upright glyphs stacked one per row**, scrolling
  vertically. (Rotated Latin letters do *not* exist as Unicode codepoints — the
  90° sideways form is a renderer layout property, UAX #50, that terminals don't
  implement. Upright-stacked is the only portable option.)
- **Width discipline:** all clipping uses `unicode-width`; multi-cell graphemes
  must not be split mid-character. Never assume one char = one cell.
- **Ownership:** each of a tile's four label slots belongs to exactly one tile.
  On a shared edge, only one tile's label may occupy a given run; the engine
  must define and enforce this.

---

## 5. Cross-cutting concerns

- **Tick architecture:** separate **data cadence** (apptop refreshes every ~2 s)
  from **render cadence** (marquees/scroll animate faster, e.g. 10–20 fps). The
  engine drives rendering; the app pushes data updates when it has them.
- **Resize & minimum sizes:** every node reports a minimum size; when the
  terminal is too small the engine has a defined policy (clip, drop lowest-
  priority tiles, or show a "too small" notice). Decide the policy explicitly.
- **Theming:** a small style system (border weights, focus accent, label colors,
  gradients) owned by the engine and overridable per app — reusable, not
  apptop-specific.
- **Degraded terminals:** an ASCII/no-Unicode fallback (à la btop's `tty_mode`):
  swap box-drawing for `+ - |`, disable vertical labels, simplify focus cues.
- **Mouse (later):** click-to-focus, wheel-to-scroll a carousel, drag a split
  ratio. Optional, behind the same input router.
- **Testing:**
  - Layout solver — property tests (rects tile their parent exactly, no overlap,
    constraints respected).
  - Junction table — exhaustive test over all bitmask combinations.
  - Rendering — **golden-frame snapshot tests** via the headless backend.
  - Input/zoom — scripted key sequences asserting focus path and view root.

---

## 6. Crate / API surface

- **Library crate** (`mullion`) — layout tree, cell buffer, backends,
  junction grid, focus/input/zoom, border labels, theming.
- **apptop** depends on it as the first consumer; apptop supplies content + key
  handlers and builds its layout tree.

The engine/app boundary is a small content trait, roughly:

```rust
/// Implemented by the application for each tile's content.
trait TileContent {
    /// Draw into the given rect of the buffer. Engine has already
    /// drawn borders/labels; content fills the interior.
    fn render(&mut self, area: Rect, buf: &mut Buffer, focused: bool);

    /// Handle a key when this tile is focused and the key was not
    /// consumed by the engine's navigation layer. Returns whether handled.
    fn on_key(&mut self, key: KeyEvent) -> bool { false }

    /// Optional sizing hints for the layout solver.
    fn min_size(&self) -> (u16, u16) { (1, 1) }
}
```

Likely dependencies: `crossterm` (terminal I/O, events), `unicode-width`,
`unicode-segmentation`, and optionally a constraint solver (`cassowary`/`kasuari`)
— though ratio+min/max may be simple enough to hand-roll. We study `ratatui`'s
`Buffer`/`Rect`/`Layout` as prior art but do **not** depend on it, because
carousels, virtualization, shared-border junctions, and zoom are not first-class
there.

---

## 7. Build order (milestones)

Ordered by dependency. Each is a Claude-Code-sized unit or a small cluster of
them. Earlier phases are independently testable without the later ones.

| # | Milestone | Delivers | Depends on |
|---|-----------|----------|------------|
| 0 | **Scaffold + cell buffer + backends** | crate skeleton, `Buffer`, `Cell`, `Rect`, headless backend, crossterm backend, diff flush, synchronized output | — |
| 1 | **Layout tree + solver** | `Split` sizing → exact rects; `Adaptive` orientation resolved from rect aspect ratio (with hysteresis); pure data, no rendering; property tests | 0 |
| 2 | **Borders + junction grid** | edge grid, bitmask→glyph table, per-tile and shared border modes drawn into buffer | 0, 1 |
| 3 | **Focus + input routing** | focus path, `Tab`/`Shift-Tab` DFS, modal nav layer, content key forwarding, focus visual cues, **flip & swap operations** | 1, 2 |
| 4 | **Carousel + virtualization** | overflow scrolling groups, visible-window rendering + overscan, scroll↔focus coupling | 1, 2, 3 |
| 5 | **Zoom stack** | zoom in/out, view-root re-rooting, state save/restore; unify with drill-down | 3, 4 |
| 6 | **Border labels** | horizontal marquees, vertical upright-stacked scrolling, ownership rules, width-safe clipping | 2, 5 |
| 7 | **Theming, mouse, degraded fallback** | style system, ASCII mode, mouse focus/scroll | 2–6 |
| 8 | **apptop integration** | apptop’s views ported onto the engine as the first real consumer | all |

**Recommended starting point: Phase 0.** Everything renders into the buffer, and
the headless backend lets us test every later phase by asserting on frames. It is
the true foundation and is fully testable in isolation.

---

## 8. Open questions / decisions to make

- **Name** of the engine and crate: resolved → `mullion`.
- **Constraint solver:** hand-rolled ratio+min/max vs a `cassowary`-style solver.
  Start simple; upgrade only if the simple model can't express needed layouts.
- **Directional focus** (`hjkl` neighbor search) in v1, or `Tab`-only first?
- **Too-small-terminal policy:** clip, drop, or notice?
- **Border-label ownership** on shared edges: first-come, priority, or split-run?
- **Default navigation keymap:** prefix (`Ctrl-w …`) vs dedicated modifier
  (`Alt/Ctrl-arrows`). Affects every consumer's UX.
- **Style/theme format:** code-only, or a declarative config (TOML) so apps can
  define layouts + themes statically?
- **Adaptive orientation rule & breakpoint:** the exact aspect-ratio threshold and
  hysteresis margin; default "lay out along the longer axis" vs configurable
  breakpoints; and where the responsive policy lives (an engine helper vs the app).
- **Flip/swap keys:** which nav-mode bindings, and whether flip toggles
  `H↔V` only or also cycles back into `Adaptive`.
- **Declarative layouts:** should layouts be buildable from a serialized
  description (enabling user-configurable arrangements), or constructed in code?

---

## 9. Parking lot (ideas to revisit)

- Modal/overlay layer for menus and dialogs (apptop manual, pickers) — separate
  from the tiling core.
- Animated transitions on zoom (terminal-friendly, 1–2 frames).
- Persisted layout/session state across runs.
- Per-tile independent scroll *within* a tile's content (distinct from carousel
  group scroll).
- Split-ratio drag handles (mouse) and keyboard resize of splits.
- Accessibility: a screen-reader-friendly serialization of the current frame.
- **Dynamic-tree reconciliation helper.** A keyed-children utility that diffs a
  desired `[(TileId, Constraint)]` list into a `Split`/`Carousel`, *preserving the
  surviving child subtrees* (and thus their focus, scroll offset, and per-tile
  history) while adding new ids and pruning vanished ones. Every dynamic consumer
  reinvents this ~20-line function (apptop's Proxmox VM/group reconcile is the
  motivating case). Keep it in app-land until a second consumer validates the
  signature, then consider promoting it to core. Depends on stable, identity-
  derived `TileId`s (hash of durable domain keys, never positional indices).
- **Engine accessors for dynamic consumers.** Fetch a node by structural `TileId`
  (`node_mut(id)`) and read a node's id (`tile_id_of(&Node)`). Required by any
  reconcile step; Phase 3 (focus paths) and Phase 4 (carousel) need them anyway,
  so add them there rather than treating them as new scope.

---

## 10. How we work

- **Claude Code** writes the implementation.
- **You + Claude (chat)** shape ideas, make the decisions in §8, and write the
  Claude Code prompts — one focused prompt per milestone (or per sub-unit of a
  milestone), each with clear acceptance criteria and the tests it must pass.
- **Every prompt references `docs/commenting-guidelines.md`** — the project-wide
  standard for comment blocks per function, inline comments on non-trivial lines,
  and the mandatory before/after comment-sync discipline. The aim is code readable
  by both humans and AI, with comments kept rigorously in sync with the code.
- This document is the living source of truth. Append to §8 and §9 as new
  questions and ideas surface.
