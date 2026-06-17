# mullion — Programming Manual

> A terminal UI **tiling engine** in Rust. You describe a layout as a tree;
> mullion resolves it into one rectangle per tile; you paint into those
> rectangles. A double-buffered `Terminal` diffs and flushes only what changed.
>
> **Status:** Phases 0–7 complete — rendering substrate, layout solver, borders +
> junctions, focus, input, smooth virtualized carousels, zoom, border labels,
> mouse, directional navigation, theming, color downsampling, and degraded-terminal
> fallback. Apptop integration / the `TileContent` trait (Phase 8) is still ahead
> and is flagged **(upcoming)** below.

---

## 1. The mental model

One core idea: **a tree of nodes whose leaves are tiles, resolved against a
terminal size into one `Rect` per tile.** You draw into each tile's rectangle; the
engine never learns what your content *is*.

A **`TileId`** (a `u64` you assign) is the stable identity of a logical pane.
Content, focus, scroll position, and zoom all attach to the `TileId`, so a tile
keeps its state as the tree is restructured, grown, or pruned at runtime. Derive
`TileId`s from durable domain identity (a VM id, a cgroup path), never from
position — that is what makes a churning, runtime-discovered layout stable.

Three node kinds:

- `Node::Tile(TileId)` — a leaf you paint.
- `Node::Split { orientation, children: Vec<(Constraint, Node)> }` — divides its
  rect among children that fit the available space.
- `Node::Carousel { id, orientation, scroll, children: Vec<(u16, Node)> }` — a
  scrollable strip whose children may exceed the viewport; only the visible window
  is solved and rendered (virtualization).

---

## 2. Getting started

Build a tree, solve it, frame each tile, paint the interior.

```rust
use mullion::{Buffer, Node, Constraint, Size, Orientation};
use mullion::layout::solve;
use mullion::border::{frame_tiles, Borders, BorderStyle, LineWeight, CornerStyle};
use mullion::style::Style;

const HEADER: u64 = 1;
const SIDEBAR: u64 = 2;
const MAIN: u64 = 3;

fn build() -> Node {
    Node::Split {
        orientation: Orientation::Vertical,
        children: vec![
            (Constraint::new(Size::Fixed(3)), Node::Tile(HEADER)),
            (Constraint::new(Size::Fill(1)), Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fixed(20)), Node::Tile(SIDEBAR)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(MAIN)),
                ],
            }),
        ],
    }
}

fn draw(buf: &mut Buffer, root: &mut Node) {
    let style = BorderStyle { weight: LineWeight::Light, corners: CornerStyle::Square, style: Style::default() };
    let rects = solve(root, buf.area);                            // tree → [(TileId, Rect)]
    let content = frame_tiles(buf, &rects, Borders::ALL, &style); // draw borders, get interiors
    for (id, area) in content {
        match id {
            HEADER  => { buf.set_string(area.x, area.y, "mullion", Style::default()); }
            SIDEBAR => { /* paint the sidebar into `area` */ }
            MAIN    => { /* paint the main pane into `area` */ }
            _ => {}
        }
    }
}
```

Drive it with a `Terminal<B: Backend>`: `term.draw(|buf| draw(buf, &mut root))`
clears the back buffer, runs your closure, diffs against the front buffer, and
flushes only the changed cells inside synchronized-output markers. Backends:
`CrosstermBackend` (real terminal) and `TestBackend` (headless).

The snippet above compiles and runs as `examples/quickstart.rs` — see §5.
A fully featured program lives in `examples/showcase.rs` — it exercises the
smooth carousel, labels, focus, zoom, and animation together.

---

## 3. Concepts

### 3.1 Nodes and constraints

A `Split` child carries a `Constraint`: `Size::Fixed(n)`, `Size::Percent(p)`, or
`Size::Fill(weight)` (shares leftover space by weight), with optional
`.with_min(n)` / `.with_max(n)`; `Constraint::default()` is `Fill(1)`. A split
tiles its rect exactly when at least one `Fill` child can absorb the remainder.

`Orientation` is `Horizontal`, `Vertical`, or `Adaptive { margin_pct, last }`.
**Adaptive** resolves from the rect's aspect ratio each solve (lay out along the
longer dimension) with a hysteresis dead-zone so it doesn't flicker near square —
set a split (or carousel) to `Adaptive` and it reorganizes when the terminal goes
wide↔tall.

### 3.2 The buffer and Terminal

A `Buffer` is a grid of styled `Cell`s (a grapheme + `Style`), width-aware (a
double-width grapheme occupies two cells). `set_string`/`set_grapheme` write into
it; `Buffer::blit(&src, src_rect, dst_x, dst_y)` copies a sub-rectangle (used by
smooth scrolling; blanks wide graphemes split at the copy boundary).
`Terminal::draw` does the diff-and-flush.

### 3.3 Borders

Two modes:

- **Per-tile** — `frame_tiles(buf, &rects, borders, &style)` boxes each tile and
  returns its interior content rect (adjacent tiles show a doubled gutter, by
  design). `draw_box` is the primitive.
- **Shared** — `render_shared(buf, &mut root, area, weight, &style, &overrides)`
  draws one outer frame and single-line dividers with correct `├ ┤ ┬ ┴ ┼`
  junctions (including mixed light/heavy), returning content rects.

`LineWeight` is `Light`/`Heavy`/`Double`; `CornerStyle` is `Square`/`Rounded`
(rounded is light-only). `overrides: &[(TileId, LineWeight)]` draws chosen tiles
heavier — used for the focus highlight (§3.4). `focus_override(&tree, weight)`
builds that slice from the current focus.

### 3.4 Focus and input

`Tree` owns the root plus focus, scroll, and zoom state:

```rust
use mullion::tree::{Tree, Dir, Direction};
let mut tree = Tree::new(build());     // focus starts on the first leaf
tree.focus_set(MAIN);
tree.flip_focused_parent();            // flip the focused tile's parent H↔V
tree.swap_focused(Dir::Next);          // swap with the next sibling
```

Focus follows the **`TileId`**, not a position — adding/removing/reordering *other*
leaves never disturbs it; `ensure_focus_valid()` re-resolves only if the focused
leaf disappears. While zoomed (§3.7), focus traversal is scoped to the zoomed
subtree.

**Directional navigation** (the default scheme):

- `tree.focus_dir(Direction)` — **plain arrows**. Moves between tiles *within* the
  focused tile's enclosing carousel along its axis, **wrapping** at both ends; a
  wrong-axis key or a non-carousel tile is a no-op.
- `tree.focus_dir_cross(Direction, area)` — **Shift+arrows**. Moves to the
  geometrically nearest tile in that direction across the whole (effective)
  layout, no wrap, crossing carousel/split boundaries.

**Input routing.** `InputRouter` maps keys to actions, but it's a *convenience* —
the app may call the `Tree` methods directly and own interaction mode itself.

```rust
use mullion::input::{InputRouter, KeyOutcome};
let mut router = InputRouter::new();    // prefixless arrow keymap by default
match router.handle(key, &mut tree) {
    KeyOutcome::Nav(_cmd)  => { /* focus/zoom/flip already applied to the tree */ }
    KeyOutcome::Consumed   => { /* a prefix/no-op was absorbed */ }
    KeyOutcome::Forward(k) => { /* deliver `k` to the focused tile's content */ }
}
```

The **default `Keymap`** is prefix-free: plain arrows → `focus_dir`, `Shift`+arrows
→ `focus_dir_cross`, `Enter` → zoom in (enter), `Esc` → zoom out (leave),
everything else → `Forward`. The keymap is a replaceable field (`with_keymap`); the
old `Ctrl-w`-prefix scheme is preserved as `Keymap::vim_prefix()`. The prefix is
now `Option<KeyEvent>` — `None` gives the flat (modeless) mapping.

### 3.5 Dynamic trees (grow, prune, reconcile)

The tree is plain owned data — grow with `Vec::push`, prune with `retain`,
rearrange by swapping subtrees, then re-solve. Keep a churning layout stable by
(1) deriving `TileId`s from durable identity and (2) **reconciling** rather than
rebuilding: diff a fresh snapshot into the children, reusing surviving subtrees so
their focus/scroll/history persist.

Two built-in reconcile helpers cover the two container kinds:

```rust
use mullion::{reconcile_carousel, reconcile_split};
use mullion::tree::node_by_id_mut;

// `node_by_id_mut` locates the container; then reconcile replaces children.
if let Some(carousel) = node_by_id_mut(&mut root, CAROUSEL_ID) {
    reconcile_carousel(carousel, &[
        (id_a, 6),  // (TileId, main-axis extent in cells)
        (id_b, 6),
        (id_c, 6),
    ]);
}
if let Some(sidebar) = node_by_id_mut(&mut root, SIDEBAR_SPLIT_ID) {
    reconcile_split(sidebar, &[
        (id_x, Constraint::new(Size::Fixed(10))),
        (id_y, Constraint::new(Size::Fill(1))),
    ]);
}
// Clean up focus/zoom in case a dropped id was active.
tree.ensure_focus_valid();
tree.ensure_zoom_valid();
```

For each id in `desired`:
- **Survivor:** the existing node is reused (its scroll offset, nested children,
  and other state are preserved); only the extent/constraint is updated.
- **New:** a fresh `Node::Tile(id)` is inserted.
- **Vanished:** dropped.  `Split`-rooted children (no addressable id) are also
  dropped — reconcile-managed children must be `Tile`- or `Carousel`-rooted.

The addressable id of any node (complement to `tile_id_of` which is `Tile`-only):

```rust
use mullion::node_id;
let id = node_id(&node); // Some(id) for Tile or Carousel; None for Split
```

Address a container (a `Carousel`) or tile by id with `node_by_id` /
`node_by_id_mut`. Unbounded, runtime-populated collections belong in a `Carousel`
(§3.6); the fixed skeleton stays a `Split`.

**Carousel viewport rect** — for the `render_shared` ↔ `render_carousel`
composition pattern, use `region_of` to find the rect a carousel (or any node)
was allotted in the layout:

```rust
use mullion::layout::{solve, region_of};
use mullion::render::render_carousel;

let rects = solve(&mut root, area);
// render_shared into `rects` …

if let Some(carousel_rect) = region_of(&mut root, area, CAROUSEL_ID) {
    render_carousel(buf, node_by_id_mut(&mut root, CAROUSEL_ID).unwrap(),
                    carousel_rect, &mut |buf, id, rect| {
        // paint child `id` into `rect`
    });
}
```

`region_of` returns the full viewport rect for a `Carousel` (pre-virtualization)
and the exact clipped rect for a `Tile`.  Returns `None` for a missing id or a
tile that is scrolled off screen.

### 3.6 Carousels — scrollable groups

`Node::Carousel { id, orientation, scroll, children }` holds more tiles than fit.
Each child has a fixed **main-axis extent** (cells along the scroll direction); the
cross axis fills. `solve` virtualizes — only children intersecting the viewport
produce rects, partial ones are clipped — so cost is bounded by the viewport, not
the list length.

**Smooth scrolling.** Render a carousel with `render_carousel`, *not*
`render_shared` — the latter clips tiles to complete-but-shorter boxes, the former
scrolls smoothly with genuine cut-off:

```rust
use mullion::render::render_carousel;
render_carousel(buf, tree.effective_root_mut(), body_rect, &mut |buf, id, rect| {
    // paint one child at full size into `rect`, exactly like any tile
});
```

Internally it lays the visible children into a temp buffer at full size and
**blits** the scrolled window in, so a tile half off an edge shows with its border
genuinely cut.

**Scroll control.** `tree.scroll_by(id, delta)` / `tree.scroll_to(id, offset)`
(located via `node_by_id_mut`; the upper bound is clamped at render time).
`tree.scroll_focus_into_view(area)` nudges each carousel on the focus path the
minimum amount so the focused tile is flush-visible — call it once per frame before
rendering, with the same rect you render the carousel into.

**Composition with the skeleton.** Render the split skeleton with `render_shared`,
take the carousel's region rect, then `render_carousel` into it. (`examples/
showcase.rs` demonstrates this.)

### 3.7 Zoom

Temporarily re-root the view at an addressable subtree, with a push/pop stack for
nested drill-in. The full tree stays intact; only *which subtree you solve and
render* changes. The API is purely structural (no rendering types), so it doubles
as the seam for an alternate renderer.

```rust
tree.zoom_to(cluster_id);   // re-root at an in-view Tile or Carousel (returns bool)
tree.zoom_focus();          // zoom the focused leaf (tmux-style fullscreen)
tree.zoom_out();            // pop one level;  zoom_reset() pops all
let node = tree.effective_root_mut(); // what to solve/render: the zoomed subtree or root
```

Focus traversal is scoped to the effective subtree while zoomed; carousel scroll
offsets live in the nodes, so both are preserved across zoom automatically. After a
structural edit, call `ensure_zoom_valid()` (alongside `ensure_focus_valid()`) to
pop any zoom level whose target was pruned. apptop's drill-down (host → VM →
process, Esc to return) *is* this zoom stack.

### 3.8 Border labels

Text in a tile's borders, drawn as a **post-pass** over an already-rendered,
junction-resolved border (so it never disturbs corners or junctions).

```rust
use mullion::label::{draw_label, Label, Side, Align, label_period};
draw_label(buf, tile_rect, &Label {
    text: "node-name".into(), side: Side::Top, align: Align::Start, offset: frame,
}, &Style::default());
```

Top/bottom edges take horizontal text that **scrolls as a marquee** when longer
than the edge (`label_period` gives the wrap period; advance `offset` on the render
tick). Left/right edges take **upright-stacked** vertical text — one grapheme per
row, top→bottom (Unicode has no rotated letters; this is deliberate). Wide
graphemes are blanked at a horizontal window edge and skipped on a vertical edge.

### 3.9 Mouse

Hit-testing is the inverse of `solve` — point-in-rect over its output, which
already gives carousel children's on-screen clipped rects, so it works for
smooth-scrolled carousels for free.

```rust
use mullion::mouse::{tile_at, carousel_at};
use mullion::input::MouseOutcome;
let mut router = InputRouter::new();
router.set_hover_focus(true);                 // opt-in: hover highlights a tile
match router.handle_mouse(ev, &mut tree, area) {
    MouseOutcome::Focused(id)  => { /* click/hover focused this tile */ }
    MouseOutcome::Scrolled(id) => { /* wheel scrolled this carousel */ }
    MouseOutcome::Ignored      => {}
}
```

`tile_at(rects, x, y)` and `carousel_at(&mut root, area, x, y)` are the primitives
for apps that compose regions. The backend enables mouse capture on `enter` and
restores it on `leave`/panic (opt out via `set_mouse_capture(false)`). Treating a
click as *enter* is the app's call (it can `zoom_to` after seeing `Focused`).

### 3.10 Theming & degraded terminals

#### Theme

`Theme` groups six named `Style` roles so the whole interface can be recolored by
swapping one value:

| Role | Purpose |
|------|---------|
| `border` | Unfocused tile borders |
| `border_focused` | Focused tile border |
| `text` | Primary content text |
| `text_dim` | Secondary text (labels, hints) |
| `accent` | Gauges, marquees, selected controls |
| `selection` | Selected-item highlight background |

Two built-in palettes: `Theme::default()` (dark, cyan accent) and `Theme::light()`
(black text, blue accent).  `theme.border_style(focused)` returns a ready-to-use
`BorderStyle` with `Heavy` weight for the focused tile, `Light` for others.

#### Color downsampling

`Color::downsample(depth: ColorDepth)` maps an `Rgb` colour to the nearest
palette entry.  `ColorDepth` variants:

| Variant | Behaviour |
|---------|-----------|
| `TrueColor` | Identity (default) |
| `Palette256` | Nearest xterm-256 cube or 24-step grayscale ramp |
| `Palette16` | Nearest ANSI 16 named colour |

`CrosstermBackend::set_color_depth` applies downsampling to every cell before
emission.

#### Capability detection and ASCII fallback

`Capabilities::detect()` reads `COLORTERM`, `TERM`, and the locale to infer what
the terminal supports, then `apply_capabilities` wires everything at once:

```rust
use mullion::{backend::CrosstermBackend, capabilities::Capabilities};
let mut backend = CrosstermBackend::new(std::io::stdout());
backend.apply_capabilities(&Capabilities::detect());
```

When `Capabilities::unicode` is `false` (e.g. `TERM=linux`), the backend
replaces every box-drawing glyph with `box_to_ascii` before emission:
horizontals → `'-'`, verticals → `'|'`, corners/tees/crosses → `'+'`.
Content text passes through unchanged.

---

## 4. API reference by module

| Module | Key items |
|--------|-----------|
| `geometry` | `Rect` (`intersection`, `contains`, `right`, `bottom`, `area`) |
| `style` | `Style`, `Color`, `ColorDepth`, `Modifier` |
| `theme` | `Theme` (`default`, `light`, `border_style`) |
| `capabilities` | `Capabilities` (`detect`, `full`, `from_env`) |
| `charset` | `box_to_ascii` |
| `buffer` | `Buffer` (`set_string`, `set_grapheme`, `blit`), `Cell` |
| `backend` | `Backend`, `CrosstermBackend` (`apply_capabilities`, `set_color_depth`, `set_unicode`), `TestBackend` |
| `terminal` | `Terminal` |
| `layout` | `solve`, `Node`, `Constraint`, `Size`, `Orientation`, `Axis` |
| `tree` | `Tree`, `Dir`, `Direction`, `tile_id_of`, `node_id`, `leaves`, `focus_path`, `focus_override`, `node_by_id`/`node_by_id_mut`, `reconcile_carousel`, `reconcile_split` |
| `layout` (module) | `solve`, `region_of`, `min_size` |
| `render` | `render_carousel` |
| `border` | `draw_box`, `frame_tiles`, `render_shared`, `BorderStyle`, `Borders`, `LineWeight`, `CornerStyle` |
| `junction` | `EdgeGrid`, `EdgeCell`, `resolve` |
| `label` | `draw_label`, `label_period`, `Label`, `Side`, `Align` |
| `input` | `InputRouter`, `KeyOutcome`, `NavCommand`, `Keymap`, `MouseOutcome` (+ re-exported `KeyEvent`/`KeyCode`/`KeyModifiers`, `MouseEvent`/`MouseEventKind`/`MouseButton`) |
| `mouse` | `tile_at`, `carousel_at` |

`Tree` methods worth knowing: `focus_set`/`focus_next`/`focus_prev`/`focus_first`/
`focus_last`/`ensure_focus_valid`, `focus_dir`/`focus_dir_cross`,
`flip_focused_parent`/`swap_focused`, `scroll_by`/`scroll_to`/
`scroll_focus_into_view`, `zoom_to`/`zoom_focus`/`zoom_out`/`zoom_reset`/
`is_zoomed`/`zoom_depth`/`ensure_zoom_valid`, `effective_root`/`effective_root_mut`.

Common re-exports at the crate root: `Buffer`, `Cell`, `Node`, `Constraint`,
`Size`, `Orientation`, `LineWeight`, `Theme`, `Capabilities`, `box_to_ascii`,
`Color`, `ColorDepth`, `Style`, `node_id`, `reconcile_carousel`,
`reconcile_split`, `region_of`.  Module-scoped: `Axis`, `region_of`, `solve`
(`layout`); `Dir`/`Direction` (`tree`).

---

## 5. Examples

**`examples/quickstart.rs`** — the compilable version of the §2 getting-started
snippet.  Uses `Buffer::empty` and an in-memory render (no real terminal needed):

```text
cargo run --example quickstart
```

**`examples/showcase.rs`** — a full runnable monitor: `render_shared` header strip,
vertical smooth-scrolling `Carousel` with `render_carousel`, marquee top-border
labels, upright units labels, arrow-key focus, Heavy-border focus highlight,
Enter/Esc zoom, virtualization, and render-tick animation:

```text
cargo run --example showcase
```

`showcase.rs` is the reference for the `render_carousel` ↔ `render_shared` composition.

---

## 6. Status & roadmap

**Complete (Phases 0–7):**

| Phase | What landed |
|-------|-------------|
| 0–2   | Buffer, Terminal, Backend, layout solver, per-tile and shared borders, junction resolver |
| 3     | Focus model, `Tree`, DFS traversal, zoom |
| 4     | Input routing, `InputRouter`, `Keymap` |
| 5     | Smooth virtualized `Carousel`, `render_carousel`, `blit` |
| 6     | Border labels, marquee scrolling, upright vertical text |
| 7a    | Mouse: hit-test, click-focus, wheel scroll, hover-focus |
| 7b    | Directional focus (`focus_dir`, `focus_dir_cross`), arrow keymap, `Keymap::vim_prefix` |
| 7c    | `Theme` (named style roles), `ColorDepth`, `Color::downsample` |
| 7d    | `Capabilities::detect`, `box_to_ascii`, `apply_capabilities`, quickstart example |
| 8a    | `node_id`, `reconcile_carousel`, `reconcile_split`, `region_of`; §3.5 manual |

**Upcoming:** Phase 8b — apptop integration.

See `docs/tiling-engine-roadmap.md` for the full plan and open design questions.
This manual tracks the public API as each phase merges.
