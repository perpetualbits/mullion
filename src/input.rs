// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Input router: modal navigation via a prefix key.
//!
//! ## Coupling note
//!
//! This module re-exports [`KeyCode`], [`KeyEvent`], and [`KeyModifiers`]
//! directly from crossterm.  That coupling is intentional — the engine already
//! depends on crossterm's event infrastructure and the key types are stable.
//! If a non-crossterm backend ever appears, this is the one seam to replace.
//!
//! ## Collision model
//!
//! Consumer code (e.g. apptop) binds plain keys for in-tile actions.  To avoid
//! ambiguity the engine reserves a **navigation namespace** behind a prefix key.
//! Default prefix: **`Ctrl-w`** (vim-window style).  Everything else is
//! forwarded to the caller to deliver to the focused tile.
//!
//! The prefix and bindings are held in a replaceable [`Keymap`], so a consumer
//! can choose a different scheme (e.g. a `Ctrl-b` prefix for tmux-style nav).

pub use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tree::{Dir, Tree};

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

// ── Keymap ────────────────────────────────────────────────────────────────────

/// Maps keys to nav commands while in the PendingNav state.
///
/// ## Default bindings
///
/// Prefix: **`Ctrl-w`**
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
    prefix: KeyEvent,
    bindings: Vec<(KeyEvent, NavCommand)>,
}

impl Keymap {
    /// Construct a keymap with a custom prefix and binding list.
    pub fn new(prefix: KeyEvent, bindings: Vec<(KeyEvent, NavCommand)>) -> Self {
        Self { prefix, bindings }
    }

    fn is_prefix(&self, key: &KeyEvent) -> bool {
        key.code == self.prefix.code && key.modifiers == self.prefix.modifiers
    }

    fn lookup(&self, key: &KeyEvent) -> Option<NavCommand> {
        self.bindings
            .iter()
            .find(|(k, _)| k.code == key.code && k.modifiers == key.modifiers)
            .map(|(_, cmd)| *cmd)
    }
}

impl Default for Keymap {
    fn default() -> Self {
        use KeyCode::{BackTab, Char, Tab};
        Self::new(
            KeyEvent::new(Char('w'), KeyModifiers::CONTROL),
            vec![
                (KeyEvent::new(Tab, KeyModifiers::NONE), NavCommand::FocusNext),
                (KeyEvent::new(Char('j'), KeyModifiers::NONE), NavCommand::FocusNext),
                (KeyEvent::new(BackTab, KeyModifiers::NONE), NavCommand::FocusPrev),
                (KeyEvent::new(Char('k'), KeyModifiers::NONE), NavCommand::FocusPrev),
                (KeyEvent::new(Char('g'), KeyModifiers::NONE), NavCommand::FocusFirst),
                (KeyEvent::new(Char('G'), KeyModifiers::NONE), NavCommand::FocusLast),
                (KeyEvent::new(Char('f'), KeyModifiers::NONE), NavCommand::Flip),
                (KeyEvent::new(Char('n'), KeyModifiers::NONE), NavCommand::SwapNext),
                (KeyEvent::new(Char('p'), KeyModifiers::NONE), NavCommand::SwapPrev),
                (KeyEvent::new(Char('z'), KeyModifiers::NONE), NavCommand::ZoomIn),
                (KeyEvent::new(Char('Z'), KeyModifiers::NONE), NavCommand::ZoomOut),
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

/// Modal input router: translates raw key events into [`KeyOutcome`]s.
///
/// ## State machine
///
/// ```text
/// Normal ──[prefix]──► PendingNav ──[nav key]──► Normal (fires Nav)
///                              └──[Esc / unknown]──► Normal (fires Consumed)
/// Normal ──[other key]──► Normal (fires Forward)
/// ```
///
/// The prefix is single-shot: one prefix → one command.  A sticky repeat mode
/// can be added later without changing this API.
pub struct InputRouter {
    mode: RouterMode,
    keymap: Keymap,
}

impl InputRouter {
    /// Construct with the default [`Keymap`] in Normal mode.
    pub fn new() -> Self {
        Self { mode: RouterMode::Normal, keymap: Keymap::default() }
    }

    /// Construct with a custom [`Keymap`] in Normal mode.
    pub fn with_keymap(km: Keymap) -> Self {
        Self { mode: RouterMode::Normal, keymap: km }
    }

    /// Feed one key event.  Mutates `tree` when a [`NavCommand`] fires.
    pub fn handle(&mut self, key: KeyEvent, tree: &mut Tree) -> KeyOutcome {
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
}

impl Default for InputRouter {
    fn default() -> Self {
        Self::new()
    }
}

fn execute_nav(cmd: NavCommand, tree: &mut Tree) {
    match cmd {
        NavCommand::FocusNext  => tree.focus_next(),
        NavCommand::FocusPrev  => tree.focus_prev(),
        NavCommand::FocusFirst => tree.focus_first(),
        NavCommand::FocusLast  => tree.focus_last(),
        NavCommand::Flip       => tree.flip_focused_parent(),
        NavCommand::SwapNext   => tree.swap_focused(Dir::Next),
        NavCommand::SwapPrev   => tree.swap_focused(Dir::Prev),
        NavCommand::ZoomIn     => { tree.zoom_focus(); }
        NavCommand::ZoomOut    => tree.zoom_out(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Constraint, Node, Orientation, Size};
    use crate::tree::leaves;

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

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn two_tile_tree() -> Tree {
        Tree::new(h_split(vec![tile(0), tile(1)]))
    }

    // ── Router state machine ──────────────────────────────────────────────

    #[test]
    fn prefix_in_normal_enters_pending_nav() {
        let mut router = InputRouter::new();
        let mut tree = two_tile_tree();
        let outcome = router.handle(ctrl('w'), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Consumed));
        // Mode is now PendingNav — next nav key should fire.
        let outcome = router.handle(key(KeyCode::Tab), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::FocusNext)));
    }

    #[test]
    fn non_prefix_in_normal_forwards_key() {
        let mut router = InputRouter::new();
        let mut tree = two_tile_tree();
        let outcome = router.handle(key(KeyCode::Char('a')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Forward(_)));
        // Still in Normal — next key also forwards.
        let outcome = router.handle(key(KeyCode::Enter), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Forward(_)));
    }

    #[test]
    fn prefix_tab_fires_focus_next_and_returns_to_normal() {
        let mut router = InputRouter::new();
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
        let mut router = InputRouter::new();
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
        let mut router = InputRouter::new();
        let mut tree = two_tile_tree();
        let before = tree.focus();
        router.handle(ctrl('w'), &mut tree);
        // 'Q' is not in the default keymap → Consumed, no tree change.
        let outcome = router.handle(key(KeyCode::Char('Q')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Consumed));
        assert_eq!(tree.focus(), before);
        // Back in Normal.
        assert!(matches!(router.handle(key(KeyCode::Char('a')), &mut tree), KeyOutcome::Forward(_)));
    }

    #[test]
    fn custom_keymap_routes_with_different_prefix_and_bindings() {
        let km = Keymap::new(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            vec![
                (key(KeyCode::Char('n')), NavCommand::FocusNext),
                (key(KeyCode::Char('p')), NavCommand::FocusPrev),
            ],
        );
        let mut router = InputRouter::with_keymap(km);
        let mut tree = two_tile_tree();

        // Default prefix (Ctrl-w) is no longer the prefix — forwards.
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
    fn all_default_bindings_fire() {
        use NavCommand::*;
        let cases: &[(KeyEvent, NavCommand)] = &[
            (key(KeyCode::Tab), FocusNext),
            (key(KeyCode::Char('j')), FocusNext),
            (key(KeyCode::BackTab), FocusPrev),
            (key(KeyCode::Char('k')), FocusPrev),
            (key(KeyCode::Char('g')), FocusFirst),
            (key(KeyCode::Char('G')), FocusLast),
            (key(KeyCode::Char('f')), Flip),
            (key(KeyCode::Char('n')), SwapNext),
            (key(KeyCode::Char('p')), SwapPrev),
            (key(KeyCode::Char('z')), ZoomIn),
            (key(KeyCode::Char('Z')), ZoomOut),
        ];

        let root = Node::Split {
            orientation: Orientation::Horizontal,
            children: (0u64..4)
                .map(|i| (Constraint::new(Size::Fill(1)), Node::Tile(i)))
                .collect(),
        };

        for (key_event, expected) in cases {
            let mut router = InputRouter::new();
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
        let mut router = InputRouter::new();
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1), tile(2)]));
        router.handle(ctrl('w'), &mut tree);
        router.handle(key(KeyCode::Char('n')), &mut tree); // SwapNext
        assert_eq!(leaves(tree.root()), vec![1, 0, 2]);
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn flip_via_router_changes_orientation() {
        let mut router = InputRouter::new();
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        router.handle(ctrl('w'), &mut tree);
        router.handle(key(KeyCode::Char('f')), &mut tree); // Flip
        assert!(matches!(tree.root(), Node::Split { orientation: Orientation::Vertical, .. }));
    }

    #[test]
    fn zoom_in_via_router_drives_zoom_focus() {
        let mut router = InputRouter::new();
        // focus starts at tile 0; Ctrl-w z → ZoomIn → zoom_focus() into tile 0.
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
        let mut router = InputRouter::new();
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.zoom_focus(); // zoom in manually so we can test zoom-out
        assert!(tree.is_zoomed());

        router.handle(ctrl('w'), &mut tree);
        let outcome = router.handle(key(KeyCode::Char('Z')), &mut tree);
        assert!(matches!(outcome, KeyOutcome::Nav(NavCommand::ZoomOut)));
        assert!(!tree.is_zoomed(), "tree must not be zoomed after ZoomOut");
    }
}
