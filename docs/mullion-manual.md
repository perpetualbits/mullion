# mullion — Programming Manual

> A terminal UI **tiling engine** in Rust. You describe a layout as a tree;
> mullion resolves it into one rectangle per tile; you paint into those
> rectangles. A double-buffered `Terminal` diffs and flushes only what changed.
>
> **Status:** Phases 0–7b complete — rendering substrate, layout solver, borders +
> junctions, focus, input, smooth virtualized carousels, zoom, border labels,
> mouse, and directional (arrow) navigation. Theming + degraded-terminal fallback
> (7c/7d) and apptop integration / the `TileContent` trait (8) are still ahead and
> are flagged **(upcoming)** below.

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

A complete, runnable program lives in `examples/showcase.rs` — it exercises the
smooth carousel, labels, focus, zoom, and animation together (see §5).

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

```rust
fn reconcile(children: &mut Vec<(Constraint, Node)>, desired: &[(TileId, Constraint)]) {
    let mut old: std::collections::HashMap<u64, (Constraint, Node)> = children
        .drain(..).filter_map(|(c, n)| mullion::tree::tile_id_of(&n).map(|id| (id, (c, n)))).collect();
    for &(id, c) in desired {
        match old.remove(&id) {
            Some((_, node)) => children.push((c, node)),         // survivor: keep its state
            None            => children.push((c, Node::Tile(id))), // newly appeared
        }
    } // leftovers in `old` are gone — dropped here
}
```

Address a container (a `Carousel`) or tile by id with `node_by_id` /
`node_by_id_mut`. Unbounded, runtime-populated collections belong in a `Carousel`
(§3.6); the fixed skeleton stays a `Split`.

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

### 3.10 Theming & degraded terminals **(upcoming — Phase 7c/7d)**

A `Theme` of named style roles (border, focused border, text, dim, accent,
selection), color **downsampling** (truecolor → 256 → 16) applied at the backend,
an **ASCII** box-drawing fallback for terminals without reliable Unicode, and
capability detection to select depth + charset automatically.

---

## 4. API reference by module

| Module | Key items |
|--------|-----------|
| `geometry` | `Rect` (`intersection`, `contains`, `right`, `bottom`, `area`) |
| `style` | `Style`, `Color`, `Modifier` |
| `buffer` | `Buffer` (`set_string`, `set_grapheme`, `blit`), `Cell` |
| `backend` | `Backend`, `CrosstermBackend`, `TestBackend` |
| `terminal` | `Terminal` |
| `layout` | `solve`, `Node`, `Constraint`, `Size`, `Orientation`, `Axis` |
| `tree` | `Tree`, `Dir`, `Direction`, `tile_id_of`, `leaves`, `focus_path`, `focus_override`, `node_by_id`/`node_by_id_mut` |
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
`Size`, `Orientation`, `LineWeight`. Module-scoped: `Axis` (`layout`),
`Dir`/`Direction` (`tree`).

---

## 5. A worked example

`examples/showcase.rs` is a runnable monitor: a `render_shared` header strip, a
vertical smooth-scrolling `Carousel` of node tiles rendered with `render_carousel`,
each tile carrying a marquee top-border label and an upright units label, with
arrow-key focus, a Heavy-border focus highlight, Enter/Esc zoom, virtualization,
and render-tick animation. Run it with `cargo run --example showcase`. It is also
the reference for the `render_carousel` ↔ `render_shared` composition.

---

## 6. Status & roadmap

Complete and reviewed: Phases 0–7b. Upcoming: 7c (theming + color downsampling),
7d (ASCII fallback + capability detection), 8 (apptop integration + the
`TileContent` trait). See `docs/tiling-engine-roadmap.md` for the full plan and
open design questions. This manual tracks the public API as each phase merges.
