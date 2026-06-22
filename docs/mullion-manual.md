# mullion — Programming Manual

> A terminal UI **tiling engine** in Rust. You describe a layout as a tree;
> mullion resolves it into one rectangle per tile; you paint into those
> rectangles. A double-buffered `Terminal` diffs and flushes only what changed.
>
> **Status:** Phases 0–8e complete — rendering substrate, layout solver, borders +
> junctions, focus, input, smooth virtualized carousels, zoom, border labels,
> mouse, directional navigation, theming, color downsampling, degraded-terminal
> fallback, dynamic-tree reconcile, consumer ergonomics helpers, animation
> helpers, gap-aware rim animation (`BorderGap`), declarative column layout
> (`mullion::table`), and the `Table` widget (header + carousel body + footer).

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

**Stable ids from domain keys.** Turn any hashable domain label into a `TileId`
without an explicit allocation table:

```rust
use mullion::id_from_key;

// Derive ids from durable domain identity — stable across reconcile cycles.
let ids: Vec<u64> = vm_ids.iter().map(|&vmid| id_from_key(vmid)).collect();

// ids pair naturally with reconcile_carousel:
if let Some(carousel) = node_by_id_mut(&mut root, CAROUSEL_ID) {
    let desired: Vec<(u64, u16)> = vm_ids.iter()
        .map(|&vmid| (id_from_key(vmid), ROW_HEIGHT))
        .collect();
    reconcile_carousel(carousel, &desired);
}
```

`id_from_key` is deterministic within a build; equal keys → equal ids every
call.  Not guaranteed stable across Rust compiler versions — use it for
frame-to-frame stability, not cross-run persistence.

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

**Data virtualization.** Use `carousel_visible_range` to restrict data sampling or
computation to only the rows that will actually be rendered:

```rust
use mullion::layout::carousel_visible_range;

// carousel_node is a &Node::Carousel; area is the viewport rect.
let range = carousel_visible_range(&carousel_node, area);
// sample data only for visible indices — O(visible) not O(total):
for row in &data[range] {
    // compute metrics, format text, etc.
}
```

The function uses the same internal math as `solve` and `render_carousel`, so the
range always matches what is on screen (including partially-visible edge tiles).
An out-of-range stored scroll is clamped the same way `solve` clamps it.

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

**Effective-root dispatch.** `effective_root_id()` returns the id when the root is
a `Tile` or `Carousel`, `None` for a `Split`.  The recommended dispatch pattern
avoids the typed-enum path (not provided — it adds no expressiveness over a plain
`match`):

```rust
// Branch on what the user is currently viewing.
match tree.effective_root() {
    Node::Tile(id)            => render_detail_view(*id),
    Node::Carousel { id, .. } => render_carousel_overview(*id),
    Node::Split { .. }        => render_split_skeleton(),
}
// Or, when only the id matters:
if let Some(id) = tree.effective_root_id() { /* Tile or Carousel */ }
```

**Closure accessor.** `with_effective_root_mut` scopes the mutable borrow to the
tree, so a render closure can disjointly borrow other struct fields without an
`Option<Tree>` take/restore dance:

```rust
let rects = tree.with_effective_root_mut(|root, focus| solve(root, area));
```

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

### 3.11 Animation helpers

The `ease` module and a pair of geometry methods on `Rect` cover the recurring
needs of animated TUI code without tying mullion to an animation runtime.

#### `mullion::ease`

| Function | Signature | Description |
|----------|-----------|-------------|
| `smoothstep` | `(t: f32) -> f32` | 3t²−2t³ easing, clamped to [0,1]. Zero slope at both ends — smooth start and stop. |
| `lerp` | `(a, b, t: f32) -> f32` | Linear interpolation; extrapolates when `t` is outside [0,1]. |
| `gaussian` | `(x, sigma: f32) -> f32` | Normalised Gaussian kernel exp(−x²/2σ²), peaking at 1.0 when x=0. |

All three are also re-exported at the crate root (`mullion::smoothstep`, etc.).

**Animated zoom pattern** — the most common use of `smoothstep` is easing a
`Fill` weight to grow one tile smoothly without `Tree::zoom_to` (which is
discrete).  Keep a `t: f32` in `[0, 1]` and update the weight every frame:

```rust
use mullion::{ease::smoothstep, layout::{Constraint, Size}};

// Advance t toward 1 when zooming in, toward 0 when zooming out:
// t += dt / 0.3;  // 300 ms ease
let weight = (1.0 + smoothstep(t) * 399.0) as u16; // 1 → 400
// Pass weight as Size::Fill(weight) to the focused tile's Constraint.
```

The layout solver then grows that tile continuously each frame — no jump.

#### `Rect::border_pos` and `Rect::border_len`

```rust
let r = Rect::new(x, y, w, h);
let s: f32 = r.border_pos(cx, cy); // 0.0 .. <1.0, clockwise from top-left
let p: u32 = r.border_len();        // total border cells = 2*(w+h)-4
```

`border_pos` maps a cell coordinate to its normalised position on the closed
clockwise perimeter of the rectangle (top edge → right edge → bottom edge →
left edge, each corner counted once).  Interior cells and cells outside the
rect return 0.0.

The primary use case is animating box borders.  Combine with `ease::gaussian`
to apply a colour bump that travels around the entire rectangle without any
visible seam:

```no_run
use mullion::{Color, Rect, ease::gaussian};

fn border_color(rect: Rect, x: u16, y: u16, t: f32) -> Color {
    let s     = rect.border_pos(x, y);
    let center = (0.15_f32 + t * 0.09).rem_euclid(1.0); // CW rotation
    let diff  = (s - center + 0.5).rem_euclid(1.0) - 0.5; // shortest arc
    let bump  = gaussian(diff, 0.07);                       // 0..1
    Color::from_hsv(200.0 + bump * 40.0, 0.85, 0.5 + bump * 0.4)
}
```

Because `border_pos` walks the full perimeter as one loop, the bump passes
through corners and between edges with no discontinuity — the color is purely
a function of position on the loop and never has to know which edge it is on.

#### `Color::from_hsv`

```rust
Color::from_hsv(hue_degrees, saturation, value) -> Color::Rgb(r, g, b)
```

HSV → 24-bit RGB conversion.  `h` wraps automatically; `s` and `v` clamp to
[0, 1].  This is the most natural space for hue-cycling animations: hold `s`
near 1 for vivid colours and modulate `h` or `v` to produce palette shifts and
brightness pulses.

### 3.12 Gap-aware border animation — `BorderGap`

`BorderGap` declares a region on a border edge that should be skipped by an
animated rim effect, so that content drawn there keeps its own colours without
interference from the animation pass.

```rust
use mullion::{BorderGap, Rect};

// Default: rim glow skips this region.
let gap = BorderGap::new(Rect::new(x, y, width, 1));

// Explicit opt-in: rim glow applies to this region too.
let glow_gap = BorderGap::new(Rect::new(x, y, width, 1)).with_rim_glow();
```

| Member | Type | Meaning |
|--------|------|---------|
| `rect` | `Rect` | The cells covered by this gap |
| `rim_glow` | `bool` | `false` (default): animation skips these cells; `true`: animation applies |

`gap.contains(x, y)` returns `true` when `(x, y)` is inside `rect`.

**Three-pass render order.** The intended usage is:

1. **Structure** — draw box-drawing characters (corners, dashes, connectors) into the border cells.
2. **Content** — write gap-specific content (legend text, status, coloured markers).
3. **Animation** — walk the perimeter CW; for each border cell skip it when `!gap.rim_glow && gap.contains(x, y)`.

Drawing structure first, then content, then the animation pass means the rim
colour is applied on top of structural dashes but is suppressed inside declared
gaps, so it never overwrites coloured text.

```rust
// Typical animation skip in the perimeter loop:
for &(x, y) in &cells {
    if gaps.iter().any(|g| !g.rim_glow && g.contains(x, y)) { continue; }
    // … apply colour …
}
```

**Bookend placement.** Structural characters (`┤`, `├`) that bracket a gap
should lie *outside* the `BorderGap::rect` so they receive the rim colour and
glow as the animation passes through them.

---

### 3.13 Column layout — `mullion::table`

A declarative column-layout engine backed by `layout::solve`. Declare column
widths once with `ColumnDef`; resolve them to `Rect`s at render time; use the
static `write_*` helpers to paint each cell. Eliminates manual width arithmetic
in data-table views.

```rust
use mullion::{ColumnDef, ColumnGrid, ColumnKind};

let grid = ColumnGrid::new(vec![
    ColumnDef::fill(1, ColumnKind::Text).with_min(8).with_max(28),  // label
    ColumnDef::fixed(1, ColumnKind::Custom),                         // spacer
    ColumnDef::fixed(9, ColumnKind::Number { unit_cols: 1 }),        // value + unit
    ColumnDef::fill(2, ColumnKind::Bar),                             // bar
]);
```

#### `ColumnKind`

| Variant | Write helper |
|---------|--------------|
| `Text` | `write_text` |
| `Number { unit_cols: u16 }` | `write_number` |
| `Bar` | `write_bar` |
| `Custom` | caller writes into the resolved rect directly |

#### `ColumnDef` constructors

| Constructor | Meaning |
|-------------|---------|
| `ColumnDef::fixed(n, kind)` | Exactly `n` cells wide |
| `ColumnDef::fill(weight, kind)` | Shares leftover space by weight (`Size::Fill`) |
| `ColumnDef::percent(p, kind)` | `p`% of the available width |

All three support `.with_min(n)` / `.with_max(n)` clamps and `.with_align(Align)`.

#### Resolving

```rust
// In your render function, resolve once per frame (or when the area changes):
let col_rects: Vec<Rect> = grid.resolve(area);

// Convenience: resolve for a single data row at y:
let row_rects: Vec<Rect> = grid.row_rects(area, y);
```

`resolve` runs `layout::solve` internally. Fixed columns are satisfied first,
then percent, then fill columns share the remainder proportionally — identical
to tile layout.

#### Write helpers (static methods on `ColumnGrid`)

```rust
// Text with alignment and `…` truncation (Align::Start / Center / End):
ColumnGrid::write_text(buf, col_rects[0], y, "hostname-001", Align::End, style);

// Number + unit: rightmost `unit_cols` cells hold the unit, rest hold the
// right-aligned value string.  Both portions carry independent styles.
ColumnGrid::write_number(buf, col_rects[2], y, "42.3", val_style, "%", unit_style, 1);

// Horizontal fill bar (fraction ∈ [0, 1]):
ColumnGrid::write_bar(buf, col_rects[3], y, 0.67,
    '█', filled_style, '░', empty_style, None);

// Bar with per-cell overlay (e.g. histogram dots):
ColumnGrid::write_bar(buf, col_rects[3], y, 0.67,
    '█', filled_style, '░', empty_style,
    Some(&|col_idx| {
        let frac = col_idx as f32 / col_rects[3].width as f32;
        Some(('◻', Style::default().fg(planck_color(frac))))
    }));
```

The `overlay` closure receives the cell index (0 = leftmost) and returns an
optional `(char, Style)` drawn on top; the bar fill is always painted first so
the overlay is always visible.

---

### 3.14 `Table` — header + scrollable body + footer

`Table` composes `ColumnGrid` and `render_carousel` into a single call. All
three closures — header, body row, footer — receive the same resolved column
rects, so alignment across fixed chrome and the scrollable body is guaranteed
without any manual coordinate arithmetic.

```rust
use mullion::{Buffer, Rect, Table};
use mullion::table::{ColumnDef, ColumnGrid, ColumnKind};
use mullion::layout::Node;

let grid = ColumnGrid::new(vec![
    ColumnDef::fill(1, ColumnKind::Text).with_min(8).with_max(28),
    ColumnDef::fixed(9, ColumnKind::Number { unit_cols: 1 }),
    ColumnDef::fill(2, ColumnKind::Bar),
]);
let table = Table::new(grid);
```

#### Scroll setup before rendering

`table.body_area` returns the rect the carousel will occupy (area minus the
header and/or footer rows). Feed it to `scroll_focus_into_view` before calling
`render`:

```rust
tree.scroll_focus_into_view(table.body_area(area, true, false)); // has header, no footer
```

#### Rendering

```rust
table.render(
    buf, area, carousel_node,
    Some(|buf: &mut Buffer, cols: &[Rect]| {
        // draw the header row — cols[n].y is the header y
        ColumnGrid::write_text(buf, cols[0], cols[0].y, "name",  Align::Start, dim);
        ColumnGrid::write_text(buf, cols[1], cols[1].y, "value", Align::End,   dim);
        ColumnGrid::write_text(buf, cols[2], cols[2].y, "graph", Align::Start, dim);
    }),
    Some(|buf: &mut Buffer, cols: &[Rect]| {
        // draw the footer row — cols[n].y is the footer y
        ColumnGrid::write_text(buf, cols[0], cols[0].y, "42 items", Align::Start, dim);
    }),
    |buf: &mut Buffer, id: u64, cols: &[Rect]| {
        // draw one carousel row — cols[n].y is this entry's y
        ColumnGrid::write_text  (buf, cols[0], cols[0].y, &name,  Align::Start, style);
        ColumnGrid::write_number(buf, cols[1], cols[1].y, &value, style, "%", dim, 1);
        ColumnGrid::write_bar   (buf, cols[2], cols[2].y, fraction, '█', style, '░', dim, None);
    },
);
```

Pass `None::<fn(&mut Buffer, &[Rect])>` for any closure you don't need.

**How it works.** Column widths are resolved once from `area.width` (y is
irrelevant for column layout). The header closure is given rects at `area.y`;
the footer closure is given rects at `area.y + area.height - 1`; the row
closure is given rects re-positioned at each carousel entry's `y` by
`render_carousel`. Header and footer each consume one row; the remainder is the
carousel body. If both are absent the entire area is the carousel.

**`bar_w` pattern.** When a `Bar` column's pixel width is needed outside the
closures (e.g. to pre-compute histogram bins), resolve the grid before moving it
into `Table::new`:

```rust
let grid  = ColumnGrid::new(vec![ /* … */ ]);
let bar_w = grid.resolve(Rect::new(area.x, 0, area.width, 1))[bar_col_idx].width as usize;
let table = Table::new(grid);   // grid moved here; bar_w already captured
```

---

### 3.15 Floating tiles & free space — `mullion::float`

A floating child occupies a sub-rectangle **inset** from its parent's borders,
leaving free space around it — unlike a `Split`, which partitions its parent with
no leftover. Floating tiles are a separate, composable layer (not a `Node`
variant), so they solve *alongside* the tiling solve. Placements are
**parent-local**, so a float keeps its position when the parent moves on a
re-solve.

```rust
use mullion::{Rect, FloatLayer, FloatChild, FloatRect};
use mullion::float::{free_intervals_in_rows, free_cells_in_window};

let parent = Rect::new(0, 0, 40, 12);
let layer = FloatLayer::new()
    .with_child(FloatChild::new(1, FloatRect::new(6, 2, 16, 6))); // id 1, parent-local
let placed = layer.solve(parent);          // Vec<(TileId, Rect)>, clipped to parent
let obstacles: Vec<Rect> = placed.iter().map(|&(_, r)| r).collect();

// Two views of the free space, both viewport-bounded, from one geometry:
let slots = free_intervals_in_rows(parent, &obstacles, 1, 0..12); // per-row intervals (text)
let cells = free_cells_in_window(parent, &obstacles, 0, parent);  // free cells (router)
```

The free-space query is the load-bearing output (design-note §2): the text engine
reads it as per-row **slots** and the future connector router reads it as free
**cells**. Because both derive from the same geometry, runaround text and routing
can never disagree about where the obstacles are. The gutter (clearance kept
around each float) is a per-query parameter. Demo: `cargo run --example floating`.

### 3.16 Text engine — `mullion::text`

Bidirectional, paginated paragraph wrapping. The pipeline runs per paragraph →
per visual line: UAX #14 break opportunities → greedy width fill by
grapheme-cluster width → UAX #9 reordering → emit cells **in visual order**. It is
bidi-correct from the first call; pure-LTR text reorders to the identity.

Because mullion hands the terminal cells already in visual order, it must stop a
bidi-aware terminal (e.g. VTE — gnome-terminal, terminator) from re-ordering them
a second time. [`CrosstermBackend`](crate::backend::CrosstermBackend) does this
automatically: on `enter` it switches the terminal to **BDSM explicit** mode
(`ESC[8l`) and restores the default on `leave`. This is a one-time escape with no
per-cell cost; terminals that do not implement it ignore the mode. Without it, a
row mixing RTL text with box-drawing borders would have the borders dragged out of
place at display time.

```rust
use mullion::text::{wrap, render_wrapped, BaseDirection};

let wrapped = wrap("English then العربية then more", 20, BaseDirection::Ltr);
for line in wrapped.lines() {
    // line.cells are already in visual (display) order;
    // line.map is the per-line logical↔visual bijection (§3.2)
    let _visual_of_first = line.map.logical_to_visual(0);
}
render_wrapped(buf, area, &wrapped, /* scroll_top */ 0, style);
```

Each `VisualLine` carries a `CursorMap` — a bijection between logical (edit) order
and visual (display) order, so the cursor moves *visually* while editing
*logically*, and a selection across a direction boundary stays coherent.
**Pagination** (`page`, `page_count`) and **continuous scrolling** (`visible`) are
two views over the same wrapped model. `shape_line` is the single-line primitive
for flowed chrome-adjacent content (table cells, flowing labels) so there is no
bidi-correct/incorrect seam. Wrapping text *around* floating tiles (runaround) is
a later phase. Demo: `cargo run --example text`.

### 3.17 Row virtualization — `mullion::record` + `mullion::vlist`

Scroll a window over a huge record set without materializing it. A `RecordSource`
is **seek/keyset-shaped** — every fetch is anchored to a key, never an integer
offset (`OFFSET 750000` is O(n) in SQL, and LDAP has no offset at all):

```rust
use mullion::{VecRecordSource, VirtualList, render_scrollbar};

// VecRecordSource is the in-memory reference impl; a real one wraps SQL keyset
// pagination (WHERE key > ? ORDER BY key LIMIT n) or LDAP's VLV control.
let src = VecRecordSource::new((0u64..1_000_000).map(|k| (k, k * 7)).collect());
let mut list = VirtualList::new(src, /* viewport */ 20, /* batch */ 32);

list.scroll_by(50_000);                 // window refills via fetch_after/fetch_before
for (key, value) in list.visible() {    // only the on-screen rows are materialized
    // draw through ColumnGrid (reuse, not reinvent)
}

let m = list.scroll_metrics();          // { position, extent, exact }
render_scrollbar(buf, bar_rect, m, style); // solid █ thumb if exact, ▒ shade if estimate
```

The window is kept within `capacity` rows; fetching follows the scroll direction
so an end-to-end pass materializes each row exactly once. The scrollbar has **two
truth levels** (design-note §6.2): the thumb is a true ordinal when the source's
`exact_len` returns `Some`, and an honest **estimate** (drawn with a distinct
glyph) otherwise. Rows render through `ColumnGrid` exactly as a `Table` body does.
Demo: `cargo run --example records`.

### 3.18 Wrapped-line virtualization — `mullion::docview`

Scroll and seek through one **enormous flowed document** without re-wrapping all
of it. Harder than row virtualization because the wrapped-line count depends on
the width — you cannot jump to "wrapped line 750,000" without knowing where it
falls. The fix is a lazy **byte-offset → wrapped-line index**, built incrementally
as you move, cached, and invalidated on width change. This is kept strictly
separate from row virtualization (§3.17); they share only the viewport idea.

```rust
use mullion::{DocView, render_doc};

let mut view = DocView::new(huge_string, 80);   // nothing wrapped yet
view.scroll_by(500);                            // indexes lazily up to here
render_doc(buf, area, &mut view, style);        // only the visible window is wrapped

view.seek_to_byte(offset);    // lands on the wrapped line containing that byte
view.set_width(60);           // invalidates the index, keeps the top anchored
let n = view.total_lines();   // forces a full index (e.g. for an exact scrollbar)
```

The index is extended one paragraph-aligned chunk at a time (cuts always land just
after a `\n`), so incremental wrapping is provably identical to a full-document
wrap; only the **visible window** is re-wrapped for display, via the Phase 2 text
engine (§3.16). `line_count_hint` returns `(indexed_so_far, complete)`, which
drives an estimate-until-fully-indexed scrollbar (reusing the §3.17
`render_scrollbar`). Demo: `cargo run --example document`.

### 3.19 Runaround — `mullion::runaround`

Flow text *around* floating tiles (§3.15) by treating the free space as a stream
of slots (§3.5). For each visible row, subtracting the tiles (plus gutter) leaves
1..n free intervals — "left of tile", "right of tile", or both; flattened
top-to-bottom they form an ordered slot stream, and wrapped tokens flow into
slots instead of full-width lines.

```rust
use mullion::{Rect, runaround::{flow, render_flow}};
use mullion::text::BaseDirection;

let parent = Rect::new(0, 1, 60, 20);
let figure = Rect::new(24, 4, 14, 6);              // a floating tile to read around
let placed = flow(text, parent, &[figure], 1 /* gutter */, BaseDirection::Ltr,
                  parent.y..parent.bottom());      // viewport-bounded by the rows
render_flow(buf, &placed, style);                  // draw the figure on top
```

The obstacle-free case is one full-width slot per row, which flows through the
**same** `wrap_into_slots` path as flat wrapping — so `flow` with no obstacles
reproduces `wrap` (§3.16) line for line. Reflow on a tile drag is bounded by the
rows you pass, not the whole document. Under an **RTL** base the within-row slot
order flips (the right-of-tile slot fills first) — the §3.5 BiDi × runaround
hazard, handled explicitly.

**Words are kept whole.** A word that does not fit a narrow gap between tiles is
moved on to the next slot wide enough to hold it, rather than hard-broken
mid-word; a word is split only when it is wider than *every* slot (so it can fit
nowhere whole). The obstacle-free case has all slots the same width, so this
collapses back to ordinary greedy wrapping. Demos: `cargo run --example runaround`
(single tile) and `cargo run --example runaround_multi` (three tiles, mixed LTR/RTL).

### 3.20 Sockets — `mullion::socket`

A `Socket` is a `BorderGap` (§3.12) **with semantics**: a `(side, offset, flow,
kind)` tuple naming where a connector attaches to a tile's border (§5.1). The hard
part — placing and sizing edge gaps correctly at every box size — is the
gap-interval geometry proven in the `spiral_stress` "surf field", lifted here.

A socket renders as a **bookended gap** carved into the border, with a circle
terminal floating in the opening — `┤○ ├` along a horizontal edge, `┴●┬` stacked
along a vertical one. `●` marks a connected socket, `○` an idle one; the round
glyph never has to meet a line, and the border is capped by [`bookends`] on each
side of it.

```rust
use mullion::{Rect, label::Side};
use mullion::socket::{Socket, Flow, draw_socket, FlowStyle};

let node = Rect::new(0, 0, 24, 12);
let s = Socket::new(Side::Left, /* offset */ 4, Flow::In, /* kind */ 0);
draw_socket(buf, node, &s, /* connected */ true, style); // ┴●┬ in the left edge
let anchor = s.anchor(node);    // the cell a connector attaches to (routing)
// A single typed output socket, and an evenly-spaced, non-overlapping set:
let out = Socket::new(Side::Right, 4, Flow::Out, /* kind */ 7);
let ins = Socket::pack(Side::Left, 3, node.height);
```

`gap` clamps the opening to the edge interior so a socket never lands on a corner,
and returns `None` if it does not fit; `pack` distributes `count` unit sockets
without overlap. `kind` is an opaque type tag (mullion attaches no meaning — a
consuming app decides which sockets may connect).

The optional **connector-flow gradient** streams a hue along a gap or connector to
show flow direction or activity (lifted from the surf field's `stream_color`):

```rust
let fs = FlowStyle { band: 0, direction: 1.0, ..FlowStyle::default() };
let style = fs.color(/* pos 0..1 */ 0.3, /* time */ t, /* active */ true);
```

It is a pure function of position and time — sockets are pinned, so nothing moves
unless the caller advances `t`. The surf field's autonomous drift/pulse/split-merge
is intentionally not lifted. In the demo, a connected socket's circle is recoloured
with this gradient so it pulses with live flow. Demo: `cargo run --example sockets`.

### 3.21 Graph canvas — `mullion::graph`

A `GraphCanvas` is a tile whose floating children are **nodes**, placed by hand
(design note §5.4/§5.7). It is a thin manager over the §3.15 floating-tile
foundation — nodes are `FloatChild`s, so they carry stable `TileId`s across
re-solves and their positions are part of the canvas state.

```rust
use mullion::{GraphCanvas, FloatRect, Rect, mouse::tile_at};

let mut canvas = GraphCanvas::new(80, 24).with_grid(4);
canvas.add(1, FloatRect::new(4, 2, 16, 7));   // id 1 at canvas-local (4,2)
canvas.nudge(1, 1, 0);                          // keyboard move (clamped in-canvas)
canvas.snap_to_grid(1);                         // align to the grid

let window = Rect::new(0, 1, 80, 24);          // where the canvas is shown
let placed = canvas.solve(window);             // Vec<(TileId, Rect)>, absolute
let hit = tile_at(&placed, mouse_x, mouse_y);  // which node is under the cursor
```

Positions are **canvas-local** and every `move_to`/`nudge`/`snap_to_grid` clamps
the node fully inside the canvas (`resize` re-clamps on a size change). `solve`
maps to absolute screen rects against a window and reuses `FloatLayer::solve`, so
the result is the same `(TileId, Rect)` shape the tiling solver produces — which
means the existing `mouse::tile_at` (§3.9) is the node hit-test, the basis for
click-to-select and drag. The canvas uses a **fixed origin**; panning a window
over a larger canvas and culling off-window nodes is a later phase. Sockets
(§3.20) sit on the nodes; wiring them is Phase 8. Demo: `cargo run --example graph`.

### 3.22 Connector routing — `mullion::route`

Wire socket to socket with **orthogonal connector routing** (design note §5.2): a
known-hard problem that terminal scale tames (dozens of wires over an ~80×200
grid). `route` runs **grid A\*** over the free cells (from §3.15), with a **bend
penalty** so the path prefers long straight runs and few corners — the "train
tracks" look. It works in **canvas space**, so routes are stable under future
scrolling and are recomputed on graph edits; callers reroute every frame (cheap
at this scale).

```rust
use mullion::{Rect, route::{render as render_connectors, Connector}};
use mullion::socket::{Socket, Flow};
use mullion::float::free_cells_in_window;
use mullion::label::Side;

// Free cells = canvas minus the node bodies.
let canvas = Rect::new(0, 0, 80, 24);
let free = free_cells_in_window(canvas, &node_rects, 0, canvas).into_iter().collect();

let src = Socket::new(Side::Right, 3, Flow::Out, 0);   // an output port
let dst = Socket::new(Side::Left, 3, Flow::In, 0);     // an input port
let wire = Connector::route(
    &free,
    src.attach(node_a).unwrap(),                       // start = cell outside the socket
    dst.attach(node_b).unwrap(),                       // goal
    /* bend penalty */ 4,
    src.outward().opposite(), dst.outward().opposite(),// inward dirs, for the socket join
);
render_connectors(buf, canvas, (window.x, window.y), &[wire.unwrap()],
                  &node_rects, LineWeight::Light, style);
```

`Socket::attach` gives the free cell just outside a socket (the routing endpoint),
and `Socket::outward` the direction it faces. Rendering reuses the junction glyph
logic (§3.3): every wire is laid into an `EdgeGrid` as box-drawing arms, so turns
— and crossings between wires — resolve to the right glyph automatically. There is
no hop-over glyph (the charset has none); a crossing that is not a join reads as a
`┼` until Phase 9 disambiguates it with color-per-net. Demo: `cargo run --example
wires` (drag a node and the wires reroute live).

---

## 4. API reference by module

| Module | Key items |
|--------|-----------|
| `geometry` | `Rect` (`intersection`, `contains`, `right`, `bottom`, `area`, `border_pos`, `border_len`) |
| `style` | `Style`, `Color` (`from_hsv`, `downsample`), `ColorDepth`, `Modifier` |
| `ease` | `smoothstep`, `lerp`, `gaussian` |
| `theme` | `Theme` (`default`, `light`, `border_style`) |
| `capabilities` | `Capabilities` (`detect`, `full`, `from_env`) |
| `charset` | `box_to_ascii` |
| `buffer` | `Buffer` (`set_string`, `set_grapheme`, `blit`), `Cell` |
| `backend` | `Backend`, `CrosstermBackend` (`apply_capabilities`, `set_color_depth`, `set_unicode`), `TestBackend` |
| `terminal` | `Terminal` |
| `layout` | `solve`, `Node`, `Constraint`, `Size`, `Orientation`, `Axis` |
| `tree` | `Tree`, `Dir`, `Direction`, `tile_id_of`, `node_id`, `id_from_key`, `leaves`, `focus_path`, `focus_override`, `node_by_id`/`node_by_id_mut`, `reconcile_carousel`, `reconcile_split` |
| `layout` (module) | `solve`, `region_of`, `carousel_visible_range`, `min_size` |
| `render` | `render_carousel` |
| `border` | `draw_box`, `frame_tiles`, `render_shared`, `BorderStyle`, `Borders`, `LineWeight`, `CornerStyle`, `BorderGap` |
| `table` | `ColumnGrid` (`resolve`, `row_rects`, `write_text`, `write_number`, `write_bar`), `ColumnDef`, `ColumnKind`, `Table` (`new`, `body_area`, `render`) |
| `float` | `FloatLayer` (`with_child`, `solve`), `FloatChild`, `FloatRect`, `FreeInterval`, `free_intervals_in_rows`, `free_cells_in_window` |
| `graph` | `GraphCanvas` (`new`, `with_grid`, `resize`, `add`, `remove`, `place`, `nodes`, `move_to`, `nudge`, `snap_to_grid`, `solve`) |
| `route` | `route` (grid A\* with bend penalty), `Connector` (`route`), `render` |
| `text` | `wrap`, `wrap_into_slots`, `shape_line`, `render_wrapped`, `render_line`, `WrappedText` (`lines`, `visible`, `page`, `page_count`), `VisualLine`, `VisualCell`, `CursorMap` (`visual_to_logical`, `logical_to_visual`), `BaseDirection` |
| `record` | `RecordSource` (`key_of`, `fetch_after`, `fetch_before`, `approx_position`, `exact_len`), `Window`, `VecRecordSource` (`new`, `estimated`) |
| `vlist` | `VirtualList` (`visible`, `scroll_by`, `set_viewport`, `scroll_metrics`, `at_top`/`at_bottom`, `capacity`), `ScrollMetrics`, `render_scrollbar` |
| `docview` | `DocView` (`new`, `set_width`, `scroll_by`, `scroll_to_line`, `seek_to_byte`, `line_to_byte`, `byte_to_line`, `total_lines`, `line_count_hint`, `visible_lines`), `render_doc` |
| `runaround` | `flow`, `slots_in`, `render_flow`, `Slot`, `PlacedLine` |
| `socket` | `Socket` (`new`, `with_length`, `gap`, `rect`, `anchor`, `outward`, `attach`, `pack`), `Flow`, `bookends`, `draw_socket`, `FlowStyle` (`color`) |
| `junction` | `EdgeGrid`, `EdgeCell`, `resolve` |
| `label` | `draw_label`, `label_period`, `Label`, `Side`, `Align` |
| `input` | `InputRouter`, `KeyOutcome`, `NavCommand`, `Keymap`, `MouseOutcome` (+ re-exported `KeyEvent`/`KeyCode`/`KeyModifiers`, `MouseEvent`/`MouseEventKind`/`MouseButton`) |
| `mouse` | `tile_at`, `carousel_at` |

`Tree` methods worth knowing: `focus_set`/`focus_next`/`focus_prev`/`focus_first`/
`focus_last`/`ensure_focus_valid`, `focus_dir`/`focus_dir_cross`,
`flip_focused_parent`/`swap_focused`, `scroll_by`/`scroll_to`/
`scroll_focus_into_view`, `zoom_to`/`zoom_focus`/`zoom_out`/`zoom_reset`/
`is_zoomed`/`zoom_depth`/`ensure_zoom_valid`, `effective_root`/`effective_root_mut`/
`effective_root_id`/`with_effective_root_mut`.

Common re-exports at the crate root: `Buffer`, `Cell`, `Node`, `Constraint`,
`Size`, `Orientation`, `LineWeight`, `Theme`, `Capabilities`, `box_to_ascii`,
`Color`, `ColorDepth`, `Style`, `node_id`, `id_from_key`, `reconcile_carousel`,
`reconcile_split`, `region_of`, `carousel_visible_range`, `BorderGap`,
`ColumnDef`, `ColumnGrid`, `ColumnKind`, `Table`, `FloatLayer`, `FloatChild`,
`FloatRect`, `FreeInterval`, `wrap`, `shape_line`, `WrappedText`, `VisualLine`,
`CursorMap`, `BaseDirection`, `RecordSource`, `VecRecordSource`, `Window`,
`VirtualList`, `ScrollMetrics`, `render_scrollbar`, `DocView`, `render_doc`,
`wrap_into_slots`, `flow`, `slots_in`, `render_flow`, `Slot`, `PlacedLine`,
`Socket`, `Flow`, `FlowStyle`, `draw_socket`, `bookends`, `GraphCanvas`, `route`,
`Connector`, `render_connectors`.
Module-scoped:
`Axis`, `region_of`, `carousel_visible_range`, `solve` (`layout`);
`Dir`/`Direction` (`tree`).

---

## 5. A complete application

Sections 1–4 cover each piece in isolation. This section assembles them into a
working application with the shape aerie (and most monitors) actually have: static
chrome (header/footer), a scrollable body of live rows, focus, drill-down, and a
loop driven by **two clocks** — a slow *data tick* that mutates state and a fast
*render tick* that redraws. Every pattern here is the one `examples/showcase.rs`
validates; this is the composition to copy.

### 5.1 State: central, no registry

Keep your domain data in one place and let `TileId` join it to the tree. The engine
never stores your content — it hands you a rect, you look the row up by id and paint
it. The `Tree` holds only what is *navigable* (the carousel of rows); the header and
footer are static chrome drawn by plain arithmetic, not tree nodes.

```rust
use mullion::{Buffer, Node, Orientation};
use mullion::tree::{Tree, Direction, id_from_key, reconcile_carousel, node_by_id_mut};
use mullion::geometry::Rect;
use std::collections::HashMap;

const BODY: u64 = 0;          // the carousel's own id
const ROW_H: u16 = 4;         // each row is 4 cells tall

struct Group { label: String, load: f32 }   // your domain row

struct App {
    tree:   Tree,                       // holds the BODY carousel
    groups: HashMap<u64, Group>,        // TileId -> row data (central state)
    frame:  u64,
}

impl App {
    fn new() -> Self {
        let body = Node::Carousel { id: BODY, orientation: Orientation::Vertical,
                                    scroll: 0, children: Vec::new() };
        App { tree: Tree::new(body), groups: HashMap::new(), frame: 0 }
    }
}
```

### 5.2 The data tick: reconcile to fresh data

When new data arrives, derive a stable `TileId` per row from its durable label with
`id_from_key`, then `reconcile_carousel` the body. Survivors keep their scroll and
focus; vanished rows drop; new rows appear — all without disturbing the user's place
(§3.5). Re-validate focus and zoom afterward in case the focused/zoomed row vanished.

```rust
impl App {
    fn on_data(&mut self, fresh: Vec<Group>) {
        // 1. Rebuild the id -> data map and the desired (id, extent) list together.
        let mut desired = Vec::with_capacity(fresh.len());
        self.groups.clear();
        for g in fresh {
            let id = id_from_key(&g.label);     // stable identity from the label
            desired.push((id, ROW_H));
            self.groups.insert(id, g);
        }
        // 2. Reconcile the carousel's children to match (preserves survivors).
        if let Some(body) = node_by_id_mut(self.tree.root_mut(), BODY) {
            reconcile_carousel(body, &desired);
        }
        // 3. The focused or zoomed row may have just disappeared.
        self.tree.ensure_focus_valid();
        self.tree.ensure_zoom_valid();
    }
}
```

> **Data-layer virtualization.** If sampling each row's data is expensive, fetch only
> for on-screen rows: `carousel_visible_range(body, body_area)` (§3.6) gives the
> visible child index range, so you can skip work for rows the user can't see — the
> same virtualization the renderer already does, applied to your data.

### 5.3 The render tick: chrome, then body, then dispatch on zoom

Chrome is arithmetic; the body is either the smooth carousel (overview) or a single
filled tile (drilled in). The crucial detail is the **borrow discipline**: read
focus and zoom state through *shared* borrows that end *before* you take the `&mut`
for `render_carousel`, and let `draw_child` capture only `Copy` values (ids,
counters) so it never borrows the tree (Lesson 1).

```rust
use mullion::render::render_carousel;

fn chrome(area: Rect) -> (Rect, Rect, u16) {     // (header, body, footer_y)
    let header_h = 1u16.min(area.height);
    let footer_y = area.height.saturating_sub(1);
    let body = Rect::new(0, header_h, area.width, footer_y.saturating_sub(header_h));
    (Rect::new(0, 0, area.width, header_h), body, footer_y)
}

impl App {
    fn render(&mut self, buf: &mut Buffer) {
        let area = buf.area;
        if area.height < 4 { return; }
        let (header, body_area, footer_y) = chrome(area);

        // Header / footer: plain text into computed rects.
        buf.set_string(header.x, header.y, "aerie — fleet monitor", Default::default());
        buf.set_string(0, footer_y, "[\u{2191}\u{2193}] focus  [\u{21B5}] open  [esc] back  [q] quit", Default::default());

        // Auto-reveal must use the SAME body rect the carousel renders into.
        self.tree.scroll_focus_into_view(body_area);

        // --- shared borrows: read state, then let them end ---
        let focused = self.tree.focus();
        let zoomed_row = match self.tree.effective_root() {     // borrow ends at `}`
            Node::Tile(id) => Some(*id),
            _ => None,
        };
        let groups = &self.groups;                              // shared, Copy-free capture below uses ids only
        let frame = self.frame;

        // draw one row by id — looks data up centrally, never touches the tree.
        let mut draw_row = |buf: &mut Buffer, id: u64, rect: Rect| {
            if let Some(g) = groups.get(&id) {
                let mark = if Some(id) == focused { '\u{25B6}' } else { ' ' };
                buf.set_string(rect.x, rect.y, &format!("{mark} {}", g.label), Default::default());
                let bar = (g.load * rect.width as f32) as u16;
                buf.set_string(rect.x, rect.y + 1, &"\u{2588}".repeat(bar as usize), Default::default());
                let _ = frame; // (animate marquees / spinners off `frame` here)
            }
        };

        // --- dispatch: drilled into one row, or the carousel overview ---
        if let Some(id) = zoomed_row {
            draw_row(buf, id, body_area);                       // detail fills the body
        } else {
            render_carousel(buf, self.tree.effective_root_mut(), body_area, &mut draw_row);
        }
    }
}
```

### 5.4 Input: the app owns the mode

mullion never tracks "am I inside a row." *You* decide: plain arrows move focus,
Enter drills in, Esc backs out, everything else is yours (or forwarded to the
focused row's content). These are direct `Tree` calls — no router required.

```rust
use mullion::input::{KeyCode, KeyEvent};

impl App {
    /// Returns true to quit.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up    => { self.tree.focus_dir(Direction::Up);   false }
            KeyCode::Down  => { self.tree.focus_dir(Direction::Down); false }
            KeyCode::Enter => { self.tree.zoom_focus();               false }  // drill in
            KeyCode::Esc   => { self.tree.zoom_out();                 false }  // back out
            KeyCode::Char('q') => true,
            _ => {
                // Forward to the focused row: metric toggles, sort keys, etc.
                // (e.g. dispatch on self.tree.focus())
                false
            }
        }
    }
}
```

`focus_dir` stays inside the carousel and wraps (§3.4); `zoom_focus`/`zoom_out`
push and pop the drill-down stack (§3.7). To cross between non-carousel panes use
`focus_dir_cross` on Shift+arrows.

### 5.5 The loop: two clocks

`poll_event` with a timeout *is* the render clock — when it times out you redraw and
advance animation; when data is due you reconcile. `term.draw` diffs and flushes only
changed cells, so redrawing every ~50 ms is cheap even when nothing moved.

```rust
use mullion::{Terminal, backend::CrosstermBackend};
use mullion::input::poll_event;            // mullion re-exports KeyEvent/KeyCode/…
use crossterm::event::Event;               // …but `Event` itself comes from crossterm
use std::{io, time::{Duration, Instant}};

fn main() -> io::Result<()> {
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    term.enter()?;                                  // raw mode, alt screen, hidden cursor
    let mut app = App::new();
    app.on_data(initial_groups());                  // first data tick
    let mut last_data = Instant::now();

    let result = (|| -> io::Result<()> {
        loop {
            term.draw(|buf| app.render(buf))?;      // render tick
            app.frame = app.frame.wrapping_add(1);

            match poll_event(Duration::from_millis(50))? {
                Some(Event::Key(key)) => { if app.on_key(key) { break } }
                _ => {}                              // timeout or resize → just redraw
            }
            if last_data.elapsed() >= Duration::from_secs(2) {
                app.on_data(sample_groups());        // data tick
                last_data = Instant::now();
            }
        }
        Ok(())
    })();

    term.leave()?;                                   // restore the terminal even on error
    result
}
# fn initial_groups() -> Vec<Group> { Vec::new() }
# fn sample_groups()  -> Vec<Group> { Vec::new() }
```

That is the whole architecture: central state joined to the tree by `TileId`,
`reconcile_carousel` on the data clock, a render that dispatches between carousel and
drilled-in tile, and app-owned input — every other feature (labels, theming, mouse,
`focus_dir_cross`, degraded fallback) slots into these same four methods.

---

## 6. Example programs

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

**`examples/floating.rs`** — the §3.15 foundation: two floating tiles over a
parent, with the free space shaded live (free cells as dots, the gutter band
distinct). Move a float with `hjkl`/arrows, switch with `Tab`, change the gutter
with `[`/`]`; the free space re-solves every frame.

```text
cargo run --example floating
```

**`examples/text.rs`** — the §3.16 text engine: a paragraph mixing LTR English, a
hard newline, and an RTL Arabic run, wrapped to an adjustable width. Arrow keys
move the cursor *visually* while the status shows the *logical* index it maps to;
`[`/`]` reflow the width; `p` toggles pagination ↔ scrolling.

```text
cargo run --example text
```

**`examples/records.rs`** — the §3.17 virtual list: scroll a window over 100,000
keyed records rendered through `ColumnGrid`, with a scrollbar that is solid when
exact and a shade when estimated (press `e` to toggle the source).

```text
cargo run --example records
```

**`examples/document.rs`** — the §3.18 wrapped-line virtualization: scroll and
seek a ~60 KB flowed document while only the visible window is wrapped. The
scrollbar is an estimate until the lazy index completes; press `G` to force a full
index and watch it become exact. `[`/`]` change the wrap width (re-wraps, keeps
position).

```text
cargo run --example document
```

**`examples/runaround.rs`** — the §3.19 runaround: a paragraph flows around a
movable floating figure. Move the figure (`hjkl`/arrows) and the visible rows
reflow; `[`/`]` change the gutter; `d` flips LTR ↔ RTL to show the within-row
slot-order flip.

```text
cargo run --example runaround
```

**`examples/runaround_multi.rs`** — runaround (§3.19) around **three** tiles at
once, with a paragraph mixing LTR English and RTL Arabic. `Tab` selects a tile,
`hjkl`/arrows move it (all rows reflow), `[`/`]` change the gutter, `d` flips
LTR ↔ RTL. Exercises both the per-screen terminal-bidi handling (§3.16) and the
words-kept-whole flow (§3.19).

```text
cargo run --example runaround_multi
```

**`examples/sockets.rs`** — the §3.20 sockets: a node with input sockets down its
left edge and outputs down its right, each a bookended gap in the border (`┴●┬`)
with a circle terminal — `●` connected (the circle pulses with the flow gradient),
`○` idle. `↑`/`↓` change the socket count; `space` pauses (and toggles the look).

```text
cargo run --example sockets
```

**`examples/graph.rs`** — the §3.21 graph canvas: three nodes (each carrying
sockets) you can `Tab`-select and nudge with the arrows/`hjkl`, snap to the grid
with `s`, or drag with the mouse. Nodes stay inside the canvas.

```text
cargo run --example graph
```

**`examples/wires.rs`** — the §3.22 connector routing: three nodes wired in a
triangle, each connector routed orthogonally around the others with grid A\*.
Drag a node (or nudge it) and the wires reroute live around it.

```text
cargo run --example wires
```

**`examples/spiral_stress.rs`** (in the `aerie` crate) — an animated stress test
and visual demo.  Draws a stack of nested frames arranged like a Fibonacci /
golden-rectangle spiral that continuously uncurls and re-curls the other way
(Electric Sheep style).  Each border is a closed perimeter loop with three
Gaussian color bumps travelling around it at different speeds and directions.
The gaps in each border carry streaming ◻ bands with independent per-band colors.
Swarm mode tiles the screen with many spirals via `layout::solve`; animated zoom
eases a `Fill` weight to grow one tile to fill the screen and back.

The example exercises `ease::smoothstep` (animated zoom), `ease::gaussian`
(border color bumps), `Rect::border_pos` (perimeter loop geometry), and
`Color::from_hsv` (hue-shift palette) — all the §3.11 animation helpers.

```text
cargo run --release --example spiral_stress        # single spiral
cargo run --release --example spiral_stress --swarm  # swarm + zoom
```

---

## 7. Status & roadmap

**Complete:**

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
| 8b    | `carousel_visible_range`, `with_effective_root_mut`, `id_from_key`, `effective_root_id`; §3.5–3.7 manual |
| 8c    | Animation helpers: `ease` module (`smoothstep`, `lerp`, `gaussian`), `Rect::border_pos`/`border_len`, `Color::from_hsv`; §3.11 manual |
| 8d    | `BorderGap` — gap-aware rim animation with `rim_glow` flag, three-pass render pattern; §3.12 manual |
| 8e    | `mullion::table` — `ColumnGrid`, `ColumnDef`, `ColumnKind`, `write_text`/`write_number`/`write_bar`; §3.13 manual |
| 8f    | `Table` — header + carousel body + footer with shared column rects; `body_area` helper; §3.14 manual |

**Capability layer** (the `docs/mullion-design-note.md` roadmap — floating tiles,
text engine, and node graphs on top of the tiling core):

| Phase | What landed |
|-------|-------------|
| 1     | `mullion::float` — floating tiles + free-space slots/cells; `mullion::record` `RecordSource` trait + `Window`; §3.15 manual |
| 2     | `mullion::text` — bidi-aware wrapping, logical↔visual `CursorMap`, pagination/scrolling, `shape_line`; §3.16 manual |
| 3     | `mullion::vlist` — row virtualization over `RecordSource`, exact/estimate scrollbar; `VecRecordSource`; §3.17 manual |
| 4     | `mullion::docview` — wrapped-line virtualization, lazy byte→line index, width-change invalidation; §3.18 manual |
| 5     | `mullion::runaround` — slot-stream flow around floating tiles (`wrap_into_slots`, `flow`); LTR then BiDi × runaround; §3.19 manual |
| 6     | `mullion::socket` — `Socket` (`BorderGap` with semantics), gap geometry + `pack`, `FlowStyle` connector-flow gradient; §3.20 manual |
| 7     | `mullion::graph` — `GraphCanvas`: hand-placed nodes (drag / nudge / grid-snap) on a fixed-origin canvas; §3.21 manual |
| 8     | `mullion::route` — orthogonal connector routing (grid A\* + bend penalty), canvas-space, junction-glyph rendering; `Socket::attach`/`outward`; §3.22 manual |

**Upcoming (capability layer):** Phase 9 — nudging + crossing resolution: spread
parallel connectors onto separate tracks (gutter capacity), and disambiguate
crossings with color-per-net + extended junction glyphs.

See `docs/tiling-engine-roadmap.md` and `docs/mullion-design-note.md` for the full
plans and open design questions. This manual tracks the public API as each phase
merges.
