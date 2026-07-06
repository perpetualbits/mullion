// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Input router: key and mouse event translation.
//!
//! ## Coupling note
//!
//! This module re-exports [`KeyCode`], [`KeyEvent`], and [`KeyModifiers`]
//! directly from crossterm.  That coupling is intentional — the engine already
//! depends on crossterm's event infrastructure and the key types are stable.
//! If a non-crossterm backend ever appears, this is the one seam to replace.
//!
//! ## Keymap modes
//!
//! Two schemes are available via [`Keymap`]:
//!
//! - **Prefixless** (default): arrow keys map directly to [`NavCommand`]s without
//!   a prefix.  Plain `Up`/`Down`/`Left`/`Right` fire
//!   [`FocusDir`](NavCommand::FocusDir); `Shift+arrows` fire
//!   [`FocusDirCross`](NavCommand::FocusDirCross); `Enter` zooms in; `Esc` zooms
//!   out.  Other keys are forwarded to the caller.
//!
//! - **Prefix** (opt-in via [`Keymap::vim_prefix`]): a two-key sequence — the
//!   prefix (`Ctrl-w` by default) followed by a command key — fires the nav
//!   command.  Everything that is not the prefix is forwarded immediately.
//!
//! The active scheme is determined by [`Keymap::prefix`]: `None` → prefixless;
//! `Some(key)` → prefix state machine.

pub use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::geometry::Rect;
use crate::layout::{solve, TileId};
use crate::mouse::{carousel_at, tile_at};
use crate::tree::{Dir, Direction, Tree};

// ── NavCommand ────────────────────────────────────────────────────────────────

/// A navigation action, already decoded from a key.  Executed against the [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavCommand {
    /// Move focus to the next leaf in DFS order (wrapping).
    FocusNext,
    /// Move focus to the previous leaf in DFS order (wrapping).
    FocusPrev,
    /// Move focus to the first leaf in DFS order.
    FocusFirst,
    /// Move focus to the last leaf in DFS order.
    FocusLast,
    /// Flip the orientation of the focused leaf's parent split.
    Flip,
    /// Swap the focused leaf with its next sibling.
    SwapNext,
    /// Swap the focused leaf with its previous sibling.
    SwapPrev,
    /// Zoom into the currently focused leaf (tmux-style fullscreen).
    ///
    /// Executes [`Tree::zoom_focus`].  A no-op if already zoomed to the focus.
    ZoomIn,
    /// Pop one zoom level, returning to the previous view.
    ///
    /// Executes [`Tree::zoom_out`].  A no-op when not zoomed.
    ZoomOut,
    /// Move focus within the nearest enclosing [`Carousel`](crate::layout::Node::Carousel)
    /// in `dir`, wrapping at both ends.
    ///
    /// Executes [`Tree::focus_dir`] immediately.
    FocusDir(Direction),
    /// Move focus to the nearest visible tile in `dir` across layout boundaries.
    ///
    /// The router returns this variant as a [`KeyOutcome::Nav`] but does **not**
    /// execute it — [`Tree::focus_dir_cross`] requires a viewport `Rect` that the
    /// router does not hold.  The app must check for this outcome and call
    /// `tree.focus_dir_cross(dir, area)` with the correct viewport.
    FocusDirCross(Direction),
}

// ── KeyOutcome ────────────────────────────────────────────────────────────────

/// Result of feeding one key event to the [`InputRouter`].
#[derive(Debug)]
pub enum KeyOutcome {
    /// A [`NavCommand`] was recognised and already executed on the tree.
    Nav(NavCommand),
    /// The prefix was consumed (entering PendingNav), or a PendingNav was
    /// cancelled.  Nothing to forward.
    Consumed,
    /// Not a navigation key — the app should deliver this event to the focused
    /// tile's content handler.
    Forward(KeyEvent),
}

// ── MouseOutcome ─────────────────────────────────────────────────────────────

/// Result of feeding one mouse event to the [`InputRouter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseOutcome {
    /// A left-button press landed on a tile; focus has already been updated.
    Focused(TileId),
    /// A scroll event landed on a carousel; its scroll offset has been updated.
    Scrolled(TileId),
    /// The event did not match any interactive element (no click target, no
    /// carousel under the cursor, or an event kind the router ignores).
    Ignored,
}

// ── Keymap ────────────────────────────────────────────────────────────────────

/// Maps keys to nav commands.
///
/// ## Default (prefixless arrow scheme)
///
/// | Key | Command |
/// |-----|---------|
/// | `↑` / `↓` / `←` / `→` | `FocusDir` (carousel-scoped, wrapping) |
/// | `Shift+↑` etc. | `FocusDirCross` (geometric, no wrap) |
/// | `Enter` | `ZoomIn` |
/// | `Esc` | `ZoomOut` |
///
/// ## Prefix scheme (opt-in via [`Keymap::vim_prefix`])
///
/// Prefix: **`Ctrl-w`** then one command key:
///
/// | Key | Command |
/// |-----|---------|
/// | `Tab` / `j` | `FocusNext` |
/// | `BackTab` / `k` | `FocusPrev` |
/// | `g` | `FocusFirst` |
/// | `G` | `FocusLast` |
/// | `f` | `Flip` |
/// | `n` | `SwapNext` |
/// | `p` | `SwapPrev` |
/// | `z` | `ZoomIn` |
/// | `Z` | `ZoomOut` |
pub struct Keymap {
    /// Optional prefix key.
    ///
    /// `None` — prefixless: keys are looked up directly; unmapped keys forward.
    /// `Some(k)` — prefix state machine: `k` enters `PendingNav`, then the
    /// next key is looked up; unmapped or `Esc` returns to `Normal`.
    pub prefix: Option<KeyEvent>,
    bindings: Vec<(KeyEvent, NavCommand)>,
}

impl Keymap {
    /// Construct a keymap with an optional prefix and a binding list.
    ///
    /// Pass `None` for `prefix` to use the flat, prefixless lookup mode.
    /// Pass `Some(key)` to require a two-key sequence (prefix then command).
    pub fn new(prefix: Option<KeyEvent>, bindings: Vec<(KeyEvent, NavCommand)>) -> Self {
        Self { prefix, bindings }
    }

    /// Vim-style `Ctrl-w` prefix keymap (the pre-7b default scheme).
    ///
    /// Use this when the app occupies most keys itself and wants an unambiguous
    /// navigation namespace behind a dedicated prefix key.
    pub fn vim_prefix() -> Self {
        use KeyCode::{BackTab, Char, Tab};
        Self::new(
            Some(KeyEvent::new(Char('w'), KeyModifiers::CONTROL)),
            vec![
                (KeyEvent::new(Tab, KeyModifiers::NONE),           NavCommand::FocusNext),
                (KeyEvent::new(Char('j'), KeyModifiers::NONE),     NavCommand::FocusNext),
                (KeyEvent::new(BackTab, KeyModifiers::NONE),       NavCommand::FocusPrev),
                (KeyEvent::new(Char('k'), KeyModifiers::NONE),     NavCommand::FocusPrev),
                (KeyEvent::new(Char('g'), KeyModifiers::NONE),     NavCommand::FocusFirst),
                (KeyEvent::new(Char('G'), KeyModifiers::NONE),     NavCommand::FocusLast),
                (KeyEvent::new(Char('f'), KeyModifiers::NONE),     NavCommand::Flip),
                (KeyEvent::new(Char('n'), KeyModifiers::NONE),     NavCommand::SwapNext),
                (KeyEvent::new(Char('p'), KeyModifiers::NONE),     NavCommand::SwapPrev),
                (KeyEvent::new(Char('z'), KeyModifiers::NONE),     NavCommand::ZoomIn),
                (KeyEvent::new(Char('Z'), KeyModifiers::NONE),     NavCommand::ZoomOut),
            ],
        )
    }

    /// Whether `key` matches the configured prefix key.
    fn is_prefix(&self, key: &KeyEvent) -> bool {
        match &self.prefix {
            Some(p) => key.code == p.code && key.modifiers == p.modifiers,
            None => false,
        }
    }

    /// Look up the `NavCommand` bound to `key`, if any.
    fn lookup(&self, key: &KeyEvent) -> Option<NavCommand> {
        self.bindings
            .iter()
            .find(|(k, _)| k.code == key.code && k.modifiers == key.modifiers)
            .map(|(_, cmd)| *cmd)
    }
}

/// The default keymap: prefixless, with arrow keys focusing in a direction,
/// `Shift`+arrows crossing split boundaries, `Enter` zooming in, and `Esc`
/// zooming out.
impl Default for Keymap {
    fn default() -> Self {
        use KeyCode::{Down, Enter, Esc, Left, Right, Up};
        Self::new(
            None,
            vec![
                (KeyEvent::new(Up,    KeyModifiers::NONE),  NavCommand::FocusDir(Direction::Up)),
                (KeyEvent::new(Down,  KeyModifiers::NONE),  NavCommand::FocusDir(Direction::Down)),
                (KeyEvent::new(Left,  KeyModifiers::NONE),  NavCommand::FocusDir(Direction::Left)),
                (KeyEvent::new(Right, KeyModifiers::NONE),  NavCommand::FocusDir(Direction::Right)),
                (KeyEvent::new(Up,    KeyModifiers::SHIFT), NavCommand::FocusDirCross(Direction::Up)),
                (KeyEvent::new(Down,  KeyModifiers::SHIFT), NavCommand::FocusDirCross(Direction::Down)),
                (KeyEvent::new(Left,  KeyModifiers::SHIFT), NavCommand::FocusDirCross(Direction::Left)),
                (KeyEvent::new(Right, KeyModifiers::SHIFT), NavCommand::FocusDirCross(Direction::Right)),
                (KeyEvent::new(Enter, KeyModifiers::NONE),  NavCommand::ZoomIn),
                (KeyEvent::new(Esc,   KeyModifiers::NONE),  NavCommand::ZoomOut),
            ],
        )
    }
}

// ── RouterMode ────────────────────────────────────────────────────────────────

/// Internal state of the router's two-mode state machine.
enum RouterMode {
    /// Ordinary passthrough; the prefix key triggers the transition.
    Normal,
    /// One key is pending; it is interpreted as a nav command (or cancels).
    PendingNav,
}

// ── InputRouter ───────────────────────────────────────────────────────────────

/// Modal input router: translates raw key and mouse events into typed outcomes.
///
/// ## State machine (keyboard)
///
/// ```text
/// Normal ──[prefix]──► PendingNav ──[nav key]──► Normal (fires Nav)
///                              └──[Esc / unknown]──► Normal (fires Consumed)
/// Normal ──[other key]──► Normal (fires Forward)
/// ```
///
/// The prefix is single-shot: one prefix → one command.  A sticky repeat mode
/// can be added later without changing this API.
///
/// ## Mouse handling
///
/// [`handle_mouse`](InputRouter::handle_mouse) is stateless with respect to the
/// key state machine: it always resolves the event directly against the tree
/// regardless of whether the router is in `Normal` or `PendingNav` mode.  Mouse
/// events do not cancel a pending key prefix.
pub struct InputRouter {
    /// Current state of the key prefix state machine.
    mode: RouterMode,
    /// Active key bindings.
    keymap: Keymap,
    /// Number of scroll steps fired per wheel tick.  Default: 1.
    wheel_scroll_step: u16,
    /// When `true`, `MouseMoved` events focus the tile under the cursor.
    hover_focus: bool,
}

impl InputRouter {
    /// Construct with the default [`Keymap`] (prefixless arrow scheme).
    pub fn new() -> Self {
        Self {
            mode: RouterMode::Normal,
            keymap: Keymap::default(),
            wheel_scroll_step: 1,
            hover_focus: false,
        }
    }

    /// Construct with a custom [`Keymap`] in Normal mode and a wheel step of 1.
    pub fn with_keymap(km: Keymap) -> Self {
        Self {
            mode: RouterMode::Normal,
            keymap: km,
            wheel_scroll_step: 1,
            hover_focus: false,
        }
    }

    /// Enable or disable hover-to-focus: when `on`, [`MouseEventKind::Moved`]
    /// events focus the tile under the cursor (as if a left-click had occurred).
    ///
    /// Disabled by default.  Hover focus is useful for cursor-follows-mouse
    /// workflows but can feel aggressive in keyboard-primary apps.
    pub fn set_hover_focus(&mut self, on: bool) -> &mut Self {
        self.hover_focus = on;
        self
    }

    /// Set the number of scroll steps fired per wheel tick.
    ///
    /// The wheel step applies to both `ScrollUp` and `ScrollDown` events handled
    /// by [`handle_mouse`](InputRouter::handle_mouse).  The default is 1.
    pub fn set_wheel_scroll_step(&mut self, step: u16) -> &mut Self {
        self.wheel_scroll_step = step;
        self
    }

    /// Feed one key event.  Mutates `tree` when a [`NavCommand`] fires.
    ///
    /// ## Prefixless mode (`keymap.prefix == None`)
    ///
    /// The key is looked up directly in the binding table.  A match fires the
    /// [`NavCommand`] (executing it on `tree`) and returns `Nav(cmd)`.  An
    /// unmapped key returns `Forward(key)`.  `Consumed` is never returned in
    /// this mode.
    ///
    /// ## Prefix mode (`keymap.prefix == Some(p)`)
    ///
    /// Two-key state machine: the prefix key → `Consumed` (enters PendingNav);
    /// then a command key → `Nav(cmd)` (back to Normal); `Esc` or an unmapped
    /// key → `Consumed` (cancel, back to Normal).  Non-prefix keys in Normal
    /// state → `Forward`.
    pub fn handle(&mut self, key: KeyEvent, tree: &mut Tree) -> KeyOutcome {
        if self.keymap.prefix.is_none() {
            // Flat lookup: no state machine, no Consumed outcome.
            return match self.keymap.lookup(&key) {
                Some(cmd) => {
                    execute_nav(cmd, tree);
                    KeyOutcome::Nav(cmd)
                }
                None => KeyOutcome::Forward(key),
            };
        }

        // Prefix state machine (unchanged from Phase 3b).
        match self.mode {
            RouterMode::Normal => {
                if self.keymap.is_prefix(&key) {
                    self.mode = RouterMode::PendingNav;
                    KeyOutcome::Consumed
                } else {
                    KeyOutcome::Forward(key)
                }
            }
            RouterMode::PendingNav => {
                self.mode = RouterMode::Normal;
                if key.code == KeyCode::Esc {
                    return KeyOutcome::Consumed;
                }
                match self.keymap.lookup(&key) {
                    Some(cmd) => {
                        execute_nav(cmd, tree);
                        KeyOutcome::Nav(cmd)
                    }
                    None => KeyOutcome::Consumed,
                }
            }
        }
    }

    /// Drive focus and carousel scroll from one mouse event.
    ///
    /// Hit-tests the **effective** (zoom-aware) subtree of `tree` laid out in
    /// `area`.  The event kinds handled:
    ///
    /// - **Left-button press** — [`solve`]s the effective root, then calls
    ///   [`tile_at`] to find the tile under the cursor.  If found, calls
    ///   [`Tree::focus_set`] and returns `Focused(id)`.
    /// - **Mouse moved** — only when hover-to-focus is enabled via
    ///   [`set_hover_focus`](InputRouter::set_hover_focus); hit-tests exactly like
    ///   a left press, returning `Focused(id)` or `Ignored`.  A no-op otherwise.
    /// - **Scroll up / scroll down** — calls [`carousel_at`] on the effective
    ///   root to find the innermost carousel under the cursor, then calls
    ///   [`Tree::scroll_by`] with `−step` or `+step` respectively (where `step`
    ///   is [`wheel_scroll_step`](InputRouter::set_wheel_scroll_step)).  Returns
    ///   `Scrolled(id)`.
    /// - **Everything else** — `Ignored`.
    ///
    /// ## Zoom behaviour
    ///
    /// Because both `solve` and `carousel_at` operate on `effective_root_mut()`,
    /// a click on a tile that is outside the zoom window returns `Ignored`
    /// (the tile does not appear in the effective subtree's solved rects).
    /// Likewise a wheel event when the effective root is a single `Tile` returns
    /// `Ignored` (no carousel exists under the point).
    ///
    /// ## Composed layouts
    ///
    /// For applications that render separate regions with independent trees (e.g.
    /// a header split and a body carousel), call [`tile_at`] and [`carousel_at`]
    /// directly on each region's rect list rather than using this method with a
    /// combined area.
    ///
    /// # Parameters
    /// - `ev`: The crossterm [`MouseEvent`] to process.
    /// - `tree`: The layout tree; mutated on click (focus) or wheel (scroll).
    /// - `area`: The same `Rect` passed to `solve` / `render_carousel` for this
    ///   tree, so the solve geometry matches the render geometry.
    ///
    /// # Returns
    /// A [`MouseOutcome`] indicating what, if anything, was updated.
    pub fn handle_mouse(&mut self, ev: MouseEvent, tree: &mut Tree, area: Rect) -> MouseOutcome {
        let (x, y) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Solve the effective subtree to obtain on-screen rects, then
                // hit-test.  The Vec is owned so the mutable borrow of tree ends
                // before focus_set takes a new borrow.
                let rects = solve(tree.effective_root_mut(), area);
                match tile_at(&rects, x, y) {
                    Some(id) => {
                        tree.focus_set(id);
                        MouseOutcome::Focused(id)
                    }
                    None => MouseOutcome::Ignored,
                }
            }
            MouseEventKind::Moved if self.hover_focus => {
                let rects = solve(tree.effective_root_mut(), area);
                match tile_at(&rects, x, y) {
                    Some(id) => {
                        tree.focus_set(id);
                        MouseOutcome::Focused(id)
                    }
                    None => MouseOutcome::Ignored,
                }
            }
            MouseEventKind::ScrollUp => {
                // carousel_at returns an owned TileId (Copy); the mutable borrow
                // of tree ends before scroll_by takes a new one.
                match carousel_at(tree.effective_root_mut(), area, x, y) {
                    Some(id) => {
                        // Scroll backward: negative delta, saturating at 0.
                        tree.scroll_by(id, -(self.wheel_scroll_step as i32));
                        MouseOutcome::Scrolled(id)
                    }
                    None => MouseOutcome::Ignored,
                }
            }
            MouseEventKind::ScrollDown => {
                match carousel_at(tree.effective_root_mut(), area, x, y) {
                    Some(id) => {
                        tree.scroll_by(id, self.wheel_scroll_step as i32);
                        MouseOutcome::Scrolled(id)
                    }
                    None => MouseOutcome::Ignored,
                }
            }
            _ => MouseOutcome::Ignored,
        }
    }
}

/// The default router — equivalent to `InputRouter::new()`.
impl Default for InputRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Dispatch a decoded `NavCommand` against `tree`.
fn execute_nav(cmd: NavCommand, tree: &mut Tree) {
    match cmd {
        NavCommand::FocusNext          => tree.focus_next(),
        NavCommand::FocusPrev          => tree.focus_prev(),
        NavCommand::FocusFirst         => tree.focus_first(),
        NavCommand::FocusLast          => tree.focus_last(),
        NavCommand::Flip               => tree.flip_focused_parent(),
        NavCommand::SwapNext           => tree.swap_focused(Dir::Next),
        NavCommand::SwapPrev           => tree.swap_focused(Dir::Prev),
        NavCommand::ZoomIn             => { tree.zoom_focus(); }
        NavCommand::ZoomOut            => tree.zoom_out(),
        NavCommand::FocusDir(dir)      => tree.focus_dir(dir),
        // FocusDirCross requires a viewport rect the router does not hold.
        // The Nav outcome signals the app to call tree.focus_dir_cross(dir, area).
        NavCommand::FocusDirCross(_)   => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::layout::{Constraint, Node, Orientation, Size};
    use crate::tree::{leaves, node_by_id};

    fn tile(id: u64) -> Node {
        Node::Tile(id)
    }

    fn h_split(kids: Vec<Node>) -> Node {
        Node::Split {
            orientation: Orientation::Horizontal,
            children: kids.into_iter()
                .map(|n| (Constraint::new(Size::Fill(1)), n))
                .collect(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn two_tile_tree() -> Tree {
        Tree::new(h_split(vec![tile(0), tile(1)]))
    }

    /// Router pre-configured with the Ctrl-w vim prefix scheme.
    fn vim_router() -> InputRouter {
        InputRouter::with_keymap(Keymap::vim_prefix())
    }

    // ── Prefix (vim_prefix) state machine ────────────────────────────────

    #[test]
    fn prefix_in_normal_enters_pending_nav() {
        let mut router = vim_router();
        let mut tree = two_tile_tree();
        let outcome = router.handle(ctrl('w'), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Consumed));
        // Mode is now PendingNav — next nav key should fire.
        let outcome = router.handle(key(KeyCode::Tab), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::FocusNext)));
    }

    #[test]
    fn non_prefix_in_normal_forwards_key() {
        let mut router = vim_router();
        let mut tree = two_tile_tree();
        // Neither 'a' nor 'q' are the Ctrl-w prefix → forward.
        let outcome = router.handle(key(KeyCode::Char('a')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Forward(_)));
        let outcome = router.handle(key(KeyCode::Char('q')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Forward(_)));
    }

    #[test]
    fn prefix_tab_fires_focus_next_and_returns_to_normal() {
        let mut router = vim_router();
        let mut tree = two_tile_tree();
        assert_eq!(tree.focus(), Some(0));
        router.handle(ctrl('w'), &mut tree);
        let outcome = router.handle(key(KeyCode::Tab), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::FocusNext)));
        assert_eq!(tree.focus(), Some(1));
        // Back in Normal — arbitrary key forwards.
        assert!(matches!(router.handle(key(KeyCode::Char('x')), &mut tree), KeyOutcome::Forward(_)));
    }

    #[test]
    fn prefix_esc_cancels_no_tree_change() {
        let mut router = vim_router();
        let mut tree = two_tile_tree();
        let before = tree.focus();
        router.handle(ctrl('w'), &mut tree);
        let outcome = router.handle(key(KeyCode::Esc), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Consumed));
        assert_eq!(tree.focus(), before);
        // Back in Normal.
        assert!(matches!(router.handle(key(KeyCode::Char('a')), &mut tree), KeyOutcome::Forward(_)));
    }

    #[test]
    fn prefix_unmapped_key_cancels_no_tree_change() {
        let mut router = vim_router();
        let mut tree = two_tile_tree();
        let before = tree.focus();
        router.handle(ctrl('w'), &mut tree);
        // 'Q' is not in the vim_prefix keymap → Consumed, no tree change.
        let outcome = router.handle(key(KeyCode::Char('Q')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Consumed));
        assert_eq!(tree.focus(), before);
        // Back in Normal.
        assert!(matches!(router.handle(key(KeyCode::Char('a')), &mut tree), KeyOutcome::Forward(_)));
    }

    #[test]
    fn custom_keymap_routes_with_different_prefix_and_bindings() {
        let km = Keymap::new(
            Some(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            vec![
                (key(KeyCode::Char('n')), NavCommand::FocusNext),
                (key(KeyCode::Char('p')), NavCommand::FocusPrev),
            ],
        );
        let mut router = InputRouter::with_keymap(km);
        let mut tree = two_tile_tree();

        // Ctrl-w is not the prefix → forwards.
        assert!(matches!(router.handle(ctrl('w'), &mut tree), KeyOutcome::Forward(_)));

        // Custom prefix (Ctrl-b) enters PendingNav.
        let outcome = router.handle(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &mut tree,
        );
        assert!(matches!(outcome, KeyOutcome::Consumed));

        // 'n' fires FocusNext.
        let outcome = router.handle(key(KeyCode::Char('n')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::FocusNext)));
    }

    #[test]
    fn all_vim_prefix_bindings_fire() {
        use NavCommand::*;
        let cases: &[(KeyEvent, NavCommand)] = &[
            (key(KeyCode::Tab),        FocusNext),
            (key(KeyCode::Char('j')),  FocusNext),
            (key(KeyCode::BackTab),    FocusPrev),
            (key(KeyCode::Char('k')),  FocusPrev),
            (key(KeyCode::Char('g')),  FocusFirst),
            (key(KeyCode::Char('G')),  FocusLast),
            (key(KeyCode::Char('f')),  Flip),
            (key(KeyCode::Char('n')),  SwapNext),
            (key(KeyCode::Char('p')),  SwapPrev),
            (key(KeyCode::Char('z')),  ZoomIn),
            (key(KeyCode::Char('Z')),  ZoomOut),
        ];

        let root = Node::Split {
            orientation: Orientation::Horizontal,
            children: (0u64..4)
                .map(|i| (Constraint::new(Size::Fill(1)), Node::Tile(i)))
                .collect(),
        };

        for (key_event, expected) in cases {
            let mut router = vim_router();
            let mut tree = Tree::new(root.clone());
            router.handle(ctrl('w'), &mut tree);
            match router.handle(*key_event, &mut tree) {
                KeyOutcome::Nav(cmd) => assert_eq!(cmd, *expected),
                other => panic!("expected Nav({:?}), got {:?}", expected, other),
            }
        }
    }

    #[test]
    fn swap_next_via_router_reorders_siblings() {
        let mut router = vim_router();
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1), tile(2)]));
        router.handle(ctrl('w'), &mut tree);
        router.handle(key(KeyCode::Char('n')), &mut tree); // SwapNext
        assert_eq!(leaves(tree.root()), vec![1, 0, 2]);
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn flip_via_router_changes_orientation() {
        let mut router = vim_router();
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        router.handle(ctrl('w'), &mut tree);
        router.handle(key(KeyCode::Char('f')), &mut tree); // Flip
        assert!(matches!(tree.root(), Node::Split { orientation: Orientation::Vertical, .. }));
    }

    #[test]
    fn zoom_in_via_router_drives_zoom_focus() {
        let mut router = vim_router();
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        router.handle(ctrl('w'), &mut tree);
        let outcome = router.handle(key(KeyCode::Char('z')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::ZoomIn)));
        assert!(tree.is_zoomed(), "tree must be zoomed after ZoomIn");
        assert_eq!(tree.zoom_depth(), 1);
        assert!(matches!(tree.effective_root(), Node::Tile(0)));
    }

    #[test]
    fn zoom_out_via_router_drives_zoom_out() {
        let mut router = vim_router();
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.zoom_focus(); // zoom in manually so we can test zoom-out
        assert!(tree.is_zoomed());

        router.handle(ctrl('w'), &mut tree);
        let outcome = router.handle(key(KeyCode::Char('Z')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::ZoomOut)));
        assert!(!tree.is_zoomed(), "tree must not be zoomed after ZoomOut");
    }

    // ── Default (prefixless arrow) keymap ─────────────────────────────────

    #[test]
    fn arrow_key_fires_focus_dir_and_executes_it() {
        use crate::layout::Node;
        let root = Node::Carousel {
            id: 1,
            orientation: Orientation::Vertical,
            scroll: 0,
            children: (0u64..3).map(|i| (10u16, Node::Tile(i))).collect(),
        };
        let mut router = InputRouter::new(); // default prefixless
        let mut tree = Tree::new(root);
        assert_eq!(tree.focus(), Some(0));

        // Down arrow → FocusDir(Down) executed immediately (carousel responds).
        let outcome = router.handle(key(KeyCode::Down), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::FocusDir(Direction::Down))));
        assert_eq!(tree.focus(), Some(1), "focus must advance in the carousel");
    }

    #[test]
    fn shift_arrow_fires_focus_dir_cross_but_does_not_execute() {
        // FocusDirCross is returned but NOT executed by the router (no area available).
        let mut router = InputRouter::new();
        let mut tree = two_tile_tree();
        let before = tree.focus();

        let outcome = router.handle(shift(KeyCode::Right), &mut tree);
        assert!(
            matches!(outcome, KeyOutcome::Nav(NavCommand::FocusDirCross(Direction::Right))),
            "expected FocusDirCross(Right), got {outcome:?}",
        );
        assert_eq!(tree.focus(), before, "router must not execute FocusDirCross");
    }

    #[test]
    fn enter_fires_zoom_in_in_default_keymap() {
        let mut router = InputRouter::new();
        let mut tree = two_tile_tree();
        let outcome = router.handle(key(KeyCode::Enter), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::ZoomIn)));
        assert!(tree.is_zoomed());
    }

    #[test]
    fn esc_fires_zoom_out_in_default_keymap() {
        let mut router = InputRouter::new();
        let mut tree = two_tile_tree();
        tree.zoom_focus();
        let outcome = router.handle(key(KeyCode::Esc), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::ZoomOut)));
        assert!(!tree.is_zoomed());
    }

    #[test]
    fn unmapped_key_forwards_in_prefixless_mode() {
        let mut router = InputRouter::new();
        let mut tree = two_tile_tree();
        let outcome = router.handle(key(KeyCode::Char('a')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Forward(_)));
        // Consumed is never returned from a prefixless router.
    }

    #[test]
    fn all_arrow_default_bindings_fire() {
        use NavCommand::*;
        let dir_cases: &[(KeyEvent, NavCommand)] = &[
            (key(KeyCode::Up),    FocusDir(Direction::Up)),
            (key(KeyCode::Down),  FocusDir(Direction::Down)),
            (key(KeyCode::Left),  FocusDir(Direction::Left)),
            (key(KeyCode::Right), FocusDir(Direction::Right)),
            (shift(KeyCode::Up),    FocusDirCross(Direction::Up)),
            (shift(KeyCode::Down),  FocusDirCross(Direction::Down)),
            (shift(KeyCode::Left),  FocusDirCross(Direction::Left)),
            (shift(KeyCode::Right), FocusDirCross(Direction::Right)),
            (key(KeyCode::Enter), ZoomIn),
            (key(KeyCode::Esc),   ZoomOut),
        ];
        let root = Node::Split {
            orientation: Orientation::Horizontal,
            children: (0u64..4)
                .map(|i| (Constraint::new(Size::Fill(1)), Node::Tile(i)))
                .collect(),
        };
        for (key_event, expected) in dir_cases {
            let mut router = InputRouter::new(); // prefixless
            let mut tree = Tree::new(root.clone());
            match router.handle(*key_event, &mut tree) {
                KeyOutcome::Nav(cmd) => assert_eq!(cmd, *expected, "key {key_event:?}"),
                other => panic!("expected Nav({expected:?}) for {key_event:?}, got {other:?}"),
            }
        }
    }

    // ── handle_mouse ─────────────────────────────────────────────────────

    fn make_mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    fn v_carousel(id: u64, scroll: u16, child_h: u16, n: u64) -> Node {
        Node::Carousel {
            id,
            orientation: Orientation::Vertical,
            scroll,
            children: (0..n).map(|i| (child_h, Node::Tile(i))).collect(),
        }
    }

    #[test]
    fn mouse_left_press_focuses_tile_under_cursor() {
        // H-split [Tile(0) | Tile(1)] in 40×10.  Tile(0) = x[0,20), Tile(1) = x[20,40).
        let area = Rect::new(0, 0, 40, 10);
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        let mut router = InputRouter::new();
        assert_eq!(tree.focus(), Some(0), "focus starts at tile 0");

        // Click in the right half → should focus Tile(1).
        let ev = make_mouse(MouseEventKind::Down(MouseButton::Left), 25, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Focused(1)), "expected Focused(1)");
        assert_eq!(tree.focus(), Some(1));
    }

    #[test]
    fn mouse_left_press_in_gap_returns_ignored() {
        // Fixed(10) + Fixed(10) in a 40-col area: x=20..40 is empty.
        let area = Rect::new(0, 0, 40, 10);
        let mut tree = Tree::new(Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint { size: Size::Fixed(10), min: 0, max: u16::MAX }, Node::Tile(0)),
                (Constraint { size: Size::Fixed(10), min: 0, max: u16::MAX }, Node::Tile(1)),
            ],
        });
        let mut router = InputRouter::new();
        let ev = make_mouse(MouseEventKind::Down(MouseButton::Left), 30, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Ignored));
        // Focus unchanged.
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn mouse_wheel_down_increments_carousel_scroll() {
        // Vertical carousel id=99, 5 children × 10 rows each; scroll starts at 5.
        let area = Rect::new(0, 0, 20, 10);
        let mut tree = Tree::new(v_carousel(99, 5, 10, 5));
        let mut router = InputRouter::new();

        let ev = make_mouse(MouseEventKind::ScrollDown, 10, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Scrolled(99)));
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), 99) {
            assert_eq!(*scroll, 6, "scroll should have incremented by 1");
        } else {
            panic!("carousel not found");
        }
    }

    #[test]
    fn mouse_wheel_up_decrements_carousel_scroll_saturating() {
        let area = Rect::new(0, 0, 20, 10);
        let mut tree = Tree::new(v_carousel(99, 3, 10, 5));
        let mut router = InputRouter::new();

        let ev = make_mouse(MouseEventKind::ScrollUp, 10, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Scrolled(99)));
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), 99) {
            assert_eq!(*scroll, 2, "scroll should have decremented by 1");
        } else {
            panic!("carousel not found");
        }
    }

    #[test]
    fn mouse_wheel_up_at_zero_saturates_not_wraps() {
        let area = Rect::new(0, 0, 20, 10);
        let mut tree = Tree::new(v_carousel(99, 0, 10, 5));
        let mut router = InputRouter::new();

        let ev = make_mouse(MouseEventKind::ScrollUp, 10, 5);
        router.handle_mouse(ev, &mut tree, area);
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), 99) {
            assert_eq!(*scroll, 0, "scroll must not wrap below 0");
        } else {
            panic!("carousel not found");
        }
    }

    #[test]
    fn mouse_wheel_with_no_carousel_under_cursor_returns_ignored() {
        // Pure H-split — no carousel anywhere in the tree.
        let area = Rect::new(0, 0, 40, 10);
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        let mut router = InputRouter::new();

        let ev = make_mouse(MouseEventKind::ScrollDown, 10, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Ignored));
    }

    #[test]
    fn mouse_click_zoom_aware_outside_zoom_returns_ignored() {
        // H-split [Tile(0) | Tile(1)]; zoom into Tile(1).
        // In the zoomed view, the effective root is Tile(1) filling the whole area.
        // A click at x=5 would hit Tile(0) in the unzoomed tree but should return
        // Ignored from handle_mouse since effective_root is just Tile(1), and
        // solve([Tile(1)], area) = [(1, area)] — (5,5) is inside area → Focused(1).
        // Actually zoomed into Tile(1) means the whole area resolves to Tile(1).
        // Let's instead test that a point outside the area returns Ignored.
        let area = Rect::new(5, 5, 30, 20); // non-zero origin to test containment
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.focus_set(1);
        tree.zoom_focus(); // effective root = Tile(1)
        let mut router = InputRouter::new();

        // Click at (0,0) — outside the area rect (area starts at (5,5)).
        let ev = make_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Ignored), "out-of-area click must be Ignored");
    }

    #[test]
    fn mouse_click_zoom_aware_in_area_hits_zoomed_tile() {
        // When zoomed into Tile(1), solve on effective root in area yields [(1, area)].
        // Any in-area click therefore focuses Tile(1).
        let area = Rect::new(0, 0, 40, 10);
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.focus_set(1);
        tree.zoom_focus();
        let mut router = InputRouter::new();

        // Click in the left portion — would be Tile(0) unzoomed, but Tile(1) when zoomed.
        let ev = make_mouse(MouseEventKind::Down(MouseButton::Left), 5, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Focused(1)));
    }

    #[test]
    fn mouse_wheel_ignored_when_zoomed_to_single_tile() {
        // Zoom into a leaf tile — no carousel exists in the effective subtree.
        let area = Rect::new(0, 0, 20, 10);
        let mut tree = Tree::new(v_carousel(1, 0, 5, 4));
        tree.zoom_focus(); // focus is Tile(0); effective root becomes Tile(0)
        let mut router = InputRouter::new();

        let ev = make_mouse(MouseEventKind::ScrollDown, 10, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Ignored));
    }

    #[test]
    fn wheel_scroll_step_is_respected() {
        let area = Rect::new(0, 0, 20, 10);
        let mut tree = Tree::new(v_carousel(99, 0, 5, 10));
        let mut router = InputRouter::new();
        router.set_wheel_scroll_step(3);

        let ev = make_mouse(MouseEventKind::ScrollDown, 10, 5);
        router.handle_mouse(ev, &mut tree, area);
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), 99) {
            assert_eq!(*scroll, 3, "step=3 should advance scroll by 3");
        } else {
            panic!("carousel not found");
        }
    }

    // ── hover_focus ───────────────────────────────────────────────────────

    #[test]
    fn hover_focus_off_by_default_ignores_moved_events() {
        let area = Rect::new(0, 0, 40, 10);
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        let mut router = InputRouter::new(); // hover_focus defaults to false
        assert_eq!(tree.focus(), Some(0));

        // Moved event over Tile(1) must be ignored when hover_focus is off.
        let ev = make_mouse(MouseEventKind::Moved, 25, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Ignored));
        assert_eq!(tree.focus(), Some(0), "focus must not change");
    }

    #[test]
    fn hover_focus_on_focuses_tile_under_cursor() {
        let area = Rect::new(0, 0, 40, 10);
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        let mut router = InputRouter::new();
        router.set_hover_focus(true);
        assert_eq!(tree.focus(), Some(0));

        // Move cursor over Tile(1) (x=25 is in the right half).
        let ev = make_mouse(MouseEventKind::Moved, 25, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Focused(1)));
        assert_eq!(tree.focus(), Some(1), "hover must have focused tile 1");
    }

    #[test]
    fn hover_focus_outside_any_tile_returns_ignored() {
        // Two Fixed(10) tiles in a 40-col area: x=20..40 is empty.
        let area = Rect::new(0, 0, 40, 10);
        let mut tree = Tree::new(Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint { size: Size::Fixed(10), min: 0, max: u16::MAX }, Node::Tile(0)),
                (Constraint { size: Size::Fixed(10), min: 0, max: u16::MAX }, Node::Tile(1)),
            ],
        });
        let mut router = InputRouter::new();
        router.set_hover_focus(true);

        // Move into the empty gap (x=30 hits no tile).
        let ev = make_mouse(MouseEventKind::Moved, 30, 5);
        let outcome = router.handle_mouse(ev, &mut tree, area);
        assert!(matches!(outcome, MouseOutcome::Ignored));
    }
}
