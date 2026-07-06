# mullion — `curve_map` module: Claude Code prompts

Three sequenced prompts to add a reusable **space-filling-curve map view** to mullion,
so canopy (and any program) can render an occupancy/identity map on a Gilbert curve
without re-deriving the geometry. Ordered: CM-2 assumes CM-1, CM-3 is independent.
One prompt ≈ one branch/PR.

Rationale (why mullion, not canopy): the map's *geometry and rendering* — grid sizing,
curve glyphs, the recursive sub-block partition, the seam-free highlight, region
outlines — are content-agnostic and reusable. Only *what a cell means* (occupancy,
subnet, cluster) is app-specific and stays in canopy. canopy currently carries the
geometry inline; these prompts move it behind a clean mullion API.

---

## Standing context (paste at the top of each session)

> You are working on **mullion**, a content-agnostic terminal-UI engine in Rust. Read
> `docs/mullion-manual.md` (especially the `spacefill`, `border`, `style`, `buffer`
> sections) and the neighbouring modules before writing code.
>
> Hard rules for every change:
> - **Additive only.** Path-dependent apps (`aerie`, `canopy`) must keep compiling —
>   do not change existing public signatures; add new ones.
> - **Content-agnostic.** No domain semantics whatsoever — no addresses, IP, CIDR,
>   subnet, or network terms. Think "a sequence of N cells on a curve", "items",
>   "regions", "a per-cell colour".
> - Match module conventions, naming, error handling, and the **SPDX header of the
>   neighbouring files**. Doc-comment every new public item to mullion density, with a
>   runnable example where non-obvious.
> - Test culture: headless render assertions against a `Buffer`/`TestBackend`;
>   `proptest` with a checked-in regression corpus for the geometric invariants.
> - Before coding, restate: (a) the public API you intend to add, (b) the invariants
>   you'll property-test, (c) anything under-specified — ask, don't guess.

---

## CM-1 — the `curve_map` module: render foundation

> Add a new module `curve_map`: a reusable **space-filling-curve map view** built on
> `spacefill::Gilbert`. It lays a 1-D sequence of `N` cells on a Gilbert curve that
> fills a rectangle and renders each with a caller-supplied colour, drawing the curve's
> own path glyphs. It knows a cell count and a per-cell colour closure — nothing about
> what the cells mean.
>
> Deliver:
> - `curve_map::fit_dims(area: Rect, cells: u128) -> (u32, u32)` — choose a grid whose
>   cell count `w*h` is a **power of two that divides `cells`**, so every cell covers
>   the same `2^k` items. Both dimensions are powers of two and ≥ 2 (or a single 1×1)
>   so `w + h` is even and the curve is strictly continuous. Each cell is **2 columns
>   wide, 1 row tall**; maximise `w*h` (finest resolution) within `area` and `cells`;
>   tie-break to the squarest grid, then the wider.
> - `curve_map::cell_glyph(g: &Gilbert, d: usize) -> char` — the rounded box-drawing
>   glyph for cell `d`'s segment of the curve, from the direction to its previous/next
>   cell: `─ │` straights, `╭ ╮ ╰ ╯` turns, a single-port endpoint, `·` for a lone cell.
> - `curve_map::render(buf, area, g: &Gilbert, paint: impl Fn(usize) -> (Color, Color))`
>   — for each cell `d`, draw `cell_glyph` at its grid position in `paint(d)` = `(fg,
>   bg)`, filling both columns (the second continues the line with `─` when the curve
>   proceeds right, else a space in the bg).
>
> Property-test: `fit_dims` always returns a power-of-two cell count that divides
> `cells` into equal `2^k`-item cells, fits `area`, and has even `w + h`. Render test
> with a `Buffer`: a small grid paints every cell and the glyphs form a connected path.
>
> Reference implementation to lift and *generalise* (drop all address semantics):
> `~/git/canopy/src/tui/map.rs` has a working `fit_dims`, `curve_glyph`, `dir_between`,
> and paint loop.

---

## CM-2 — sub-block chooser + seam-free pulse (builds on CM-1)

> Add the atoms for a **"step through / zoom into a quadrant" chooser** on a Gilbert
> curve.
>
> Deliver:
> - `pub struct SubBlock { pub d_range: core::ops::Range<usize>, pub bounds: Rect }`
> - `Gilbert::subblocks(&self) -> Vec<SubBlock>` — the curve construction's own
>   **top-level recursive partition**: 2 parts when the region is near-square (split
>   the long axis in half), 3 parts when it is elongated (the Gilbert 2:3 split). Each
>   part is a **contiguous `d`-interval**; its `bounds` is the sub-rectangle it fills.
>   Together they tile the region and partition `[0, len)`.
> - `Gilbert::subblock_at(&self, d: usize) -> usize` — the index into `subblocks()`
>   whose `d_range` contains `d`.
> - `curve_map::pulse_segment(len: usize, seg: Range<usize>, t: f32, taper: usize) ->
>   impl Fn(usize) -> f32` — a per-cell **luma multiplier**: a time-varying pulse (a
>   function of `t`) on cells inside `seg`, whose amplitude **ramps smoothly from the
>   pulse down to 0 across the `taper` cells at each end** of `seg`, and is exactly `0`
>   outside `seg`. So a highlighted segment blends into its neighbours on the curve
>   with **no visible seam at the join**. Pure — the caller multiplies its own cell
>   luma by the result.
>
> Property-test: `subblocks` partition `[0, len)` disjointly, each `d`-contiguous, and
> their `bounds` tile the region without overlap; `subblock_at(d)` agrees with the
> partition for every `d`. `pulse_segment` returns `0` at the first and last cell of
> `seg` and everywhere outside it, is symmetric at the two ends, and stays finite and
> non-negative across the whole range of `t`.

---

## CM-3 — rounded region outline (independent; canopy's subnet/VLAN boundaries)

> Add a way to outline an **arbitrary region of cells**, not just a rectangle.
>
> `pub fn draw_region_outline(buf: &mut Buffer, area: Rect, inside: impl Fn(u16, u16)
> -> bool, style: &BorderStyle)` — trace the boundary between `inside` and outside
> cells with a **marching-squares** pass, using the light **rounded** glyphs (`─ │ ╭ ╮
> ╰ ╯`) for corner turns and straight runs, plus `├ ┤ ┬ ┴ ┼` where the boundary
> branches, so a compact region gets a clean closed rounded outline. Draw on the cells
> just outside the region (or a caller-chosen inset) so it frames without overwriting
> content. Honour `BorderStyle` (weight, colour); rounded corners follow the existing
> "rounded is Light-only, else fall back" rule in `border`.
>
> Optionally accept a short label anchored to the top edge of the region's bounding
> box, reusing the existing frame-title machinery in `border`.
>
> Tests (`Buffer`): a rectangular `inside` reproduces `draw_box` with rounded corners;
> an L-shaped region produces a single closed loop (every outline cell has exactly two
> outline neighbours); a one-cell region is a tiny closed box.

---

### After these land

canopy deletes its inline `fit_dims`/`curve_glyph`/`dir_between` and the ad-hoc
group/subnet drawing, and drives `curve_map` with data: `d → item` occupancy and
colour (CM-1 `paint`), the quadrant chooser + zoom (CM-2 `subblocks` + `pulse_segment`),
and subnet/VLAN boundaries (CM-3 `draw_region_outline`). Cluster identity already uses
`mullion::FlowStyle`.
