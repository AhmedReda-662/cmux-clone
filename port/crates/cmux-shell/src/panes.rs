//! Pane-tree model for the shell's split panes.
//!
//! Pure model over a leaf payload (`PaneLeaf`): splitting, closing, layout
//! rectangles, and application of `cmux-layout` plans (equalize, resize) to
//! the live tree. The leaves carry the panes' state; the tree
//! itself holds no terminal code, so the pure tree operations are unit
//! tested here with dummies.
//!
//! Plan application goes through `cmux_layout` snapshots built from the
//! leaves' `pane_frame`s — a single source of truth for the geometry
//! semantics (equalize/resize/cycle rules stay in `cmux-layout`), so calling
//! `layout` before planning is required.

use cmux_layout::{
    DividerAdjustment, Orientation, PixelRect, ResizeDirection, SplitDirection, TreeNode,
};
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Global id sources (M4.1): panes and workspaces are addressed by id from
// the IPC surface (`CMUX_SURFACE_ID`, hook routing, automation calls), so
// ids must be unique across workspaces — a per-tree counter collides
// (`p2` in workspace A and workspace B). The sources emit `p{n}`/`w{n}`
// starting at 1 (the port's very first pane/workspace keeps its
// historical `p1`/`w1`); restore seeds them above the snapshot's highest
// numeric id so later mints can never collide with restored ids.
// ---------------------------------------------------------------------------

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_WORKSPACE_ID: AtomicU64 = AtomicU64::new(1);

/// Mints the next globally-unique pane id (`p1`, `p2`, …).
pub fn mint_pane_id() -> String {
    format!("p{}", NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed))
}

/// Lifts the pane source above `next` (one past the highest restored id).
/// Seeding below the current position is a no-op; concurrent mints never
/// regress the counter.
pub fn seed_pane_ids(next: u64) {
    seed_atomic(&NEXT_PANE_ID, next);
}

/// Mints the next globally-unique workspace id (`w1`, `w2`, …). Upstream
/// fills workspace ids with UUIDs; the port keeps short deterministic
/// counters (documented wire divergence, ADR-0005 Amendment 2).
pub fn mint_workspace_id() -> String {
    format!("w{}", NEXT_WORKSPACE_ID.fetch_add(1, Ordering::Relaxed))
}

/// Lifts the workspace source above `next` after a restore that carried
/// numeric `w{N}` ids.
pub fn seed_workspace_ids(next: u64) {
    seed_atomic(&NEXT_WORKSPACE_ID, next);
}

fn seed_atomic(cell: &AtomicU64, next: u64) {
    let mut current = cell.load(Ordering::Relaxed);
    while current < next {
        match cell.compare_exchange(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

/// Parses the numeric `p{N}`/`w{N}` suffix helpers use for seeding.
fn numeric_suffix(id: &str, prefix: char) -> Option<u64> {
    id.strip_prefix(prefix)?.parse().ok()
}

/// One past the highest numeric `p{N}` id in `ids` (1 when none) — the
/// restore-side seeding helper.
pub fn seed_past_pane_ids(ids: impl IntoIterator<Item = String>) -> u64 {
    numeric_suffixes_max(ids, 'p')
        .map(|max| max + 1)
        .unwrap_or(1)
}

/// One past the highest numeric `w{N}` id in `ids` (1 when none).
pub fn seed_past_workspace_ids(ids: impl IntoIterator<Item = String>) -> u64 {
    numeric_suffixes_max(ids, 'w')
        .map(|max| max + 1)
        .unwrap_or(1)
}

fn numeric_suffixes_max(ids: impl IntoIterator<Item = String>, prefix: char) -> Option<u64> {
    ids.into_iter()
        .filter_map(|id| numeric_suffix(&id, prefix))
        .max()
}

/// Per-leaf data the tree needs for geometry.
pub trait PaneLeaf {
    /// Stable identifier; unique within the tree.
    fn pane_id(&self) -> &str;
    /// The leaf's most recently laid-out rectangle.
    fn pane_frame(&self) -> PixelRect;
    fn set_pane_frame(&mut self, frame: PixelRect);
}

struct Split<L> {
    id: String,
    orientation: Orientation,
    divider: f64,
    first: Box<Node<L>>,
    second: Box<Node<L>>,
}

/// The tree's shape with ids only (no leaf payloads). Session snapshots
/// read this; restore builds a tree back from the same shape.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeOutline {
    Leaf {
        id: String,
    },
    Split {
        orientation: Orientation,
        divider: f64,
        first: Box<TreeOutline>,
        second: Box<TreeOutline>,
    },
}

fn outline_node<L: PaneLeaf>(node: &Node<L>) -> TreeOutline {
    match node {
        Node::Leaf(leaf) => TreeOutline::Leaf {
            id: leaf.pane_id().to_owned(),
        },
        Node::Split(split) => TreeOutline::Split {
            orientation: split.orientation,
            divider: split.divider,
            first: Box::new(outline_node(&split.first)),
            second: Box::new(outline_node(&split.second)),
        },
    }
}

enum Node<L> {
    Leaf(L),
    Split(Split<L>),
}

/// A pane tree whose leaves carry the panes' state.
pub struct PaneTree<L> {
    /// The root; `Some` is the standing invariant (never left empty
    /// between public calls).
    root: Option<Node<L>>,
    next_id: u64,
}

impl<L: PaneLeaf> PaneTree<L> {
    /// Builds a single-pane tree around the given leaf.
    pub fn new(leaf: L) -> Self {
        PaneTree {
            root: Some(Node::Leaf(leaf)),
            next_id: 2,
        }
    }

    /// Reserves the next pane id (`p2`, `p3`, …) for a leaf the caller
    /// creates with it.
    pub fn next_leaf_id(&mut self) -> String {
        let id = format!("p{}", self.next_id);
        self.next_id += 1;
        id
    }

    fn next_split_id(&mut self) -> String {
        let id = format!("s{}", self.next_id);
        self.next_id += 1;
        id
    }

    /// Replaces the leaf with a split in `direction`, the new pane on the
    /// direction's side at a 0.5 divider. Returns the new pane id when a
    /// leaf with `pane_id` was found (the caller's leaf must carry the id
    /// from [`Self::next_leaf_id`]).
    pub fn split_leaf(
        &mut self,
        pane_id: &str,
        direction: SplitDirection,
        new_leaf: L,
    ) -> Option<String> {
        let split_node_id = self.next_split_id();
        let new_leaf_id = new_leaf.pane_id().to_owned();
        let mut holder = Some(new_leaf);
        let root = self.root.take().expect("pane tree root invariant");
        let root = split_leaf_node(root, pane_id, direction, split_node_id, &mut holder);
        self.root = Some(root);
        if holder.is_none() {
            Some(new_leaf_id)
        } else {
            None
        }
    }

    /// Builds a two-pane split root out of two standalone trees (restore
    /// path; ids come from the snapshot, so the caller seeds the id counter
    /// afterwards via [`Self::seed_next_id`]).
    pub fn joined(
        first: PaneTree<L>,
        second: PaneTree<L>,
        orientation: Orientation,
        divider: f64,
        id: String,
    ) -> Self {
        let next_id = first.next_id.max(second.next_id);
        let first_root = first.root.expect("pane tree root invariant");
        let second_root = second.root.expect("pane tree root invariant");
        PaneTree {
            root: Some(Node::Split(Split {
                id,
                orientation,
                divider,
                first: Box::new(first_root),
                second: Box::new(second_root),
            })),
            next_id,
        }
    }

    /// Surfaces the id counter past already-used ids (restore path: future
    /// generated ids must not collide with restored ones).
    pub fn seed_next_id(&mut self, next: u64) {
        self.next_id = self.next_id.max(next);
    }

    /// The tree's shape with ids only (no leaf payloads): session snapshots
    /// and restore seeding read this.
    pub fn root_outline(&self) -> TreeOutline {
        outline_node(self.root.as_ref().expect("pane tree root invariant"))
    }

    /// Removes the leaf; its sibling collapses into the parent's place.
    /// Returns `false` when the leaf is absent or the tree has a single
    /// pane (callers refuse that themselves — the last pane is a product
    /// decision, not tree surgery).
    pub fn close_leaf(&mut self, pane_id: &str) -> bool {
        let root = self.root.take().expect("pane tree root invariant");
        let (root, removed) = close_leaf_node(root, pane_id);
        self.root = Some(root);
        removed
    }

    /// Interior access by pane id.
    pub fn leaf_mut(&mut self, pane_id: &str) -> Option<&mut L> {
        leaf_mut_node(
            self.root.as_mut().expect("pane tree root invariant"),
            pane_id,
        )
    }

    /// Read-only access by pane id.
    pub fn leaf(&self, pane_id: &str) -> Option<&L> {
        leaf_node(
            self.root.as_ref().expect("pane tree root invariant"),
            pane_id,
        )
    }

    /// Number of leaves.
    pub fn leaf_count(&self) -> usize {
        leaf_count_node(self.root.as_ref().expect("pane tree root invariant"))
    }

    /// Pane ids in on-screen order (depth-first, top/left first).
    pub fn ordered_leaf_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        ordered_leaf_ids_node(
            self.root.as_ref().expect("pane tree root invariant"),
            &mut ids,
        );
        ids
    }

    /// Divides `container` by the divider ratios and stores the rectangle on
    /// each leaf; returns leaf id → rectangle in render order.
    ///
    /// Dividers are plain ratios for now — no inset for a future divider
    /// gutter is drawn; the renderer decides chrome.
    pub fn layout(&mut self, container: PixelRect) -> Vec<(String, PixelRect)> {
        let mut layout = Vec::new();
        layout_node(
            self.root.as_mut().expect("pane tree root invariant"),
            container,
            &mut layout,
        );
        layout
    }

    /// The tree as a `cmux_layout` snapshot over leaf ids and frames.
    pub fn to_snapshot(&self) -> TreeNode {
        snapshot_node(self.root.as_ref().expect("pane tree root invariant"))
    }

    /// Equalizes all splits matching `filter` (or all when `None`) on the
    /// live tree, using the leaves' current frames for span accounting.
    pub fn equalize(&mut self, filter: Option<Orientation>) {
        let plan = self.to_snapshot().equalize_plan(filter);
        self.apply_divider_adjustments(&plan.adjustments);
    }

    /// Equalizes the run of same-orientation splits that directly contains
    /// `pane_id` (the "rebalance my row/column" operation). Leaves other
    /// splits at their divider positions.
    pub fn equalize_run_containing(&mut self, pane_id: &str) {
        let plan = self
            .to_snapshot()
            .equalize_plan_for_split_run_containing(pane_id);
        self.apply_divider_adjustments(&plan.adjustments);
    }

    /// Plans a keyboard resize of the pane's controlling divider and applies
    /// the clamped result (no-op when no enclosing split matches).
    pub fn resize_pane(&mut self, pane_id: &str, direction: ResizeDirection, amount_pixels: u16) {
        if let Some(adjustment) =
            self.to_snapshot()
                .resize_divider_adjustment(pane_id, direction, amount_pixels)
        {
            self.apply_divider_adjustments(std::slice::from_ref(&adjustment));
        }
    }

    /// Applies planned divider positions to the live tree.
    pub fn apply_divider_adjustments(&mut self, adjustments: &[DividerAdjustment]) {
        let root = self.root.as_mut().expect("pane tree root invariant");
        for adjustment in adjustments {
            apply_adjustment_node(root, adjustment);
        }
    }
}

fn split_new<L>(
    split_id: String,
    orientation: Orientation,
    divider: f64,
    first: Node<L>,
    second: Node<L>,
) -> Node<L> {
    Node::Split(Split {
        id: split_id,
        orientation,
        divider,
        first: Box::new(first),
        second: Box::new(second),
    })
}

/// Inserts the pending new leaf at the target leaf's position. Consumes
/// `new_leaf` on placement (the caller's `Option` becomes empty then).
fn split_leaf_node<L: PaneLeaf>(
    node: Node<L>,
    pane_id: &str,
    direction: SplitDirection,
    split_id: String,
    new_leaf: &mut Option<L>,
) -> Node<L> {
    match node {
        Node::Leaf(leaf) if leaf.pane_id() == pane_id && new_leaf.is_some() => {
            let new_one = new_leaf.take().expect("presence checked");
            if direction.insert_first() {
                split_new(
                    split_id,
                    direction.orientation(),
                    0.5,
                    Node::Leaf(new_one),
                    Node::Leaf(leaf),
                )
            } else {
                split_new(
                    split_id,
                    direction.orientation(),
                    0.5,
                    Node::Leaf(leaf),
                    Node::Leaf(new_one),
                )
            }
        }
        Node::Split(mut split) => {
            let first = *split.first;
            split.first = Box::new(split_leaf_node(
                first,
                pane_id,
                direction,
                split_id.clone(),
                new_leaf,
            ));
            if new_leaf.is_some() {
                let second = *split.second;
                split.second = Box::new(split_leaf_node(
                    second, pane_id, direction, split_id, new_leaf,
                ));
            }
            Node::Split(split)
        }
        node => node,
    }
}

fn close_leaf_node<L: PaneLeaf>(node: Node<L>, pane_id: &str) -> (Node<L>, bool) {
    match node {
        leaf @ Node::Leaf(_) => (leaf, false),
        Node::Split(mut split) => {
            if matches!(&*split.first, Node::Leaf(leaf) if leaf.pane_id() == pane_id) {
                return (*split.second, true);
            }
            if matches!(&*split.second, Node::Leaf(leaf) if leaf.pane_id() == pane_id) {
                return (*split.first, true);
            }
            let (first, removed_first) = close_leaf_node(*split.first, pane_id);
            split.first = Box::new(first);
            let (second, removed_second) = close_leaf_node(*split.second, pane_id);
            split.second = Box::new(second);
            (Node::Split(split), removed_first || removed_second)
        }
    }
}

fn leaf_node<'a, L: PaneLeaf>(node: &'a Node<L>, pane_id: &str) -> Option<&'a L> {
    match node {
        Node::Leaf(leaf) => (leaf.pane_id() == pane_id).then_some(leaf),
        Node::Split(split) => {
            leaf_node(&split.first, pane_id).or_else(|| leaf_node(&split.second, pane_id))
        }
    }
}

fn leaf_mut_node<'a, L: PaneLeaf>(node: &'a mut Node<L>, pane_id: &str) -> Option<&'a mut L> {
    match node {
        Node::Leaf(leaf) => (leaf.pane_id() == pane_id).then_some(leaf),
        Node::Split(split) => leaf_mut_node(&mut split.first, pane_id)
            .or_else(|| leaf_mut_node(&mut split.second, pane_id)),
    }
}

fn leaf_count_node<L: PaneLeaf>(node: &Node<L>) -> usize {
    match node {
        Node::Leaf(_) => 1,
        Node::Split(split) => leaf_count_node(&split.first) + leaf_count_node(&split.second),
    }
}

fn ordered_leaf_ids_node<L: PaneLeaf>(node: &Node<L>, ids: &mut Vec<String>) {
    match node {
        Node::Leaf(leaf) => ids.push(leaf.pane_id().to_owned()),
        Node::Split(split) => {
            ordered_leaf_ids_node(&split.first, ids);
            ordered_leaf_ids_node(&split.second, ids);
        }
    }
}

fn layout_node<L: PaneLeaf>(
    node: &mut Node<L>,
    rect: PixelRect,
    layout: &mut Vec<(String, PixelRect)>,
) {
    match node {
        Node::Leaf(leaf) => {
            leaf.set_pane_frame(rect);
            layout.push((leaf.pane_id().to_owned(), rect));
        }
        Node::Split(split) => {
            let (first_rect, second_rect) = match split.orientation {
                Orientation::Horizontal => {
                    let first_width = rect.width * split.divider;
                    (
                        PixelRect {
                            x: rect.x,
                            y: rect.y,
                            width: first_width,
                            height: rect.height,
                        },
                        PixelRect {
                            x: rect.x + first_width,
                            y: rect.y,
                            width: rect.width - first_width,
                            height: rect.height,
                        },
                    )
                }
                Orientation::Vertical => {
                    let first_height = rect.height * split.divider;
                    (
                        PixelRect {
                            x: rect.x,
                            y: rect.y,
                            width: rect.width,
                            height: first_height,
                        },
                        PixelRect {
                            x: rect.x,
                            y: rect.y + first_height,
                            width: rect.width,
                            height: rect.height - first_height,
                        },
                    )
                }
            };
            layout_node(&mut split.first, first_rect, layout);
            layout_node(&mut split.second, second_rect, layout);
        }
    }
}

fn snapshot_node<L: PaneLeaf>(node: &Node<L>) -> TreeNode {
    match node {
        Node::Leaf(leaf) => TreeNode::pane(leaf.pane_id(), leaf.pane_frame()),
        Node::Split(split) => TreeNode::split(
            split.id.clone(),
            split.orientation,
            split.divider,
            snapshot_node(&split.first),
            snapshot_node(&split.second),
        ),
    }
}

fn apply_adjustment_node<L: PaneLeaf>(node: &mut Node<L>, adjustment: &DividerAdjustment) {
    if let Node::Split(split) = node {
        if split.id == adjustment.split_id {
            split.divider = adjustment.position;
        } else {
            apply_adjustment_node(&mut split.first, adjustment);
            apply_adjustment_node(&mut split.second, adjustment);
        }
    }
}

/// Which sides of a laid-out pane rest against another pane. Renderer-side
/// visual geometry, computed from leaf frames alone, so the `cmux-layout`
/// engine stays partition-symmetric and chrome-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct NeighborSides {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// Per-side render inset for one pane: half of the shared gutter on sides
/// that touch a neighbor, and nothing on flush sides (the window frame),
/// which keeps a single-pane window full-bleed exactly like the M1 look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GutterInset {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

/// Frames come out of one recursive divide and are only compared for
/// coordinate equality across independently computed subtree fractions.
const FRAME_EPSILON: f64 = 0.001;

/// Marks the sides of `pane_id` that a sibling pane touches, per the last
/// [`PaneTree::layout`] output.
pub fn neighbor_sides(layout: &[(String, PixelRect)], pane_id: &str) -> NeighborSides {
    let Some((_, rect)) = layout.iter().find(|(id, _)| id == pane_id) else {
        return NeighborSides::default();
    };
    let pane_right = rect.x + rect.width;
    let pane_bottom = rect.y + rect.height;

    let mut sides = NeighborSides::default();
    for (other_id, other) in layout {
        if other_id == pane_id {
            continue;
        }
        let other_right = other.x + other.width;
        let other_bottom = other.y + other.height;
        let vertical_overlap = other.y.max(rect.y) + FRAME_EPSILON < other_bottom.min(pane_bottom);
        let horizontal_overlap = other.x.max(rect.x) + FRAME_EPSILON < other_right.min(pane_right);

        sides.left |= (other_right - rect.x).abs() <= FRAME_EPSILON && vertical_overlap;
        sides.right |= (other.x - pane_right).abs() <= FRAME_EPSILON && vertical_overlap;
        sides.top |= (other_bottom - rect.y).abs() <= FRAME_EPSILON && horizontal_overlap;
        sides.bottom |= (other.y - pane_bottom).abs() <= FRAME_EPSILON && horizontal_overlap;
    }
    sides
}

/// Shrink amounts for the renderer: half the gutter on neighbor-touching
/// sides, zero on flush sides (unknown/nonexistent panes get all-zero).
pub fn gutter_inset(layout: &[(String, PixelRect)], pane_id: &str, gutter: f64) -> GutterInset {
    let half = gutter / 2.0;
    let sides = neighbor_sides(layout, pane_id);
    GutterInset {
        left: if sides.left { half } else { 0.0 },
        right: if sides.right { half } else { 0.0 },
        top: if sides.top { half } else { 0.0 },
        bottom: if sides.bottom { half } else { 0.0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Leaf {
        id: String,
        frame: PixelRect,
    }

    impl Leaf {
        fn new(id: &str) -> Self {
            Leaf {
                id: id.to_owned(),
                frame: PixelRect::default(),
            }
        }
    }

    impl PaneLeaf for Leaf {
        fn pane_id(&self) -> &str {
            &self.id
        }

        fn pane_frame(&self) -> PixelRect {
            self.frame
        }

        fn set_pane_frame(&mut self, frame: PixelRect) {
            self.frame = frame;
        }
    }

    fn dummy(id: &str) -> Leaf {
        Leaf::new(id)
    }

    fn container(x: f64, y: f64, width: f64, height: f64) -> PixelRect {
        PixelRect::new(x, y, width, height)
    }

    #[test]
    fn split_right_inserts_new_pane_to_the_second_side() {
        let mut tree = PaneTree::new(dummy("a"));
        let new_id = tree.next_leaf_id();
        assert_eq!(new_id, "p2");

        let placed = tree.split_leaf("a", SplitDirection::Right, dummy("p2"));

        assert_eq!(placed.as_deref(), Some("p2"));
        assert_eq!(tree.ordered_leaf_ids(), ["a", "p2"]);
        // A right split is side by side: horizontal orientation.
        let (orientation, divider) = match &tree.to_snapshot() {
            TreeNode::Split(split) => (split.orientation, split.divider_position),
            TreeNode::Pane(_) => panic!("expected a split"),
        };
        assert_eq!(orientation, Orientation::Horizontal);
        assert_eq!(divider, 0.5);
    }

    #[test]
    fn split_directions_control_orientation_and_side() {
        let mut tree = PaneTree::new(dummy("a"));
        tree.split_leaf("a", SplitDirection::Left, dummy("p2"));
        // Left → new pane first (left side), horizontal axis.
        assert_eq!(tree.ordered_leaf_ids(), ["p2", "a"]);
        match &tree.to_snapshot() {
            TreeNode::Split(split) => assert_eq!(split.orientation, Orientation::Horizontal),
            _ => panic!(),
        }

        let mut tree = PaneTree::new(dummy("a"));
        tree.split_leaf("a", SplitDirection::Down, dummy("p2"));
        assert_eq!(tree.ordered_leaf_ids(), ["a", "p2"]);
        match &tree.to_snapshot() {
            TreeNode::Split(split) => assert_eq!(split.orientation, Orientation::Vertical),
            _ => panic!(),
        }

        let mut tree = PaneTree::new(dummy("a"));
        tree.split_leaf("a", SplitDirection::Up, dummy("p2"));
        assert_eq!(tree.ordered_leaf_ids(), ["p2", "a"]);
    }

    #[test]
    fn layout_divides_by_divider_ratios_and_marks_leaf_frames() {
        let mut tree = PaneTree::new(dummy("a"));
        let p2 = tree.next_leaf_id();
        tree.split_leaf("a", SplitDirection::Right, dummy(&p2));

        let layout = tree.layout(container(0.0, 0.0, 800.0, 600.0));

        assert_eq!(
            layout[0],
            ("a".to_owned(), container(0.0, 0.0, 400.0, 600.0))
        );
        assert_eq!(
            layout[1],
            ("p2".to_owned(), container(400.0, 0.0, 400.0, 600.0))
        );
        assert_eq!(tree.leaf("a").unwrap().pane_frame().width, 400.0);
    }

    #[test]
    fn nested_layout_respects_inner_divider() {
        let mut tree = PaneTree::new(dummy("a"));
        let p2 = tree.next_leaf_id();
        tree.split_leaf("a", SplitDirection::Right, dummy(&p2));
        assert_eq!(tree.leaf_count(), 2);
        let p3 = tree.next_leaf_id();
        tree.split_leaf("p2", SplitDirection::Down, dummy(&p3));

        let layout = tree.layout(container(0.0, 0.0, 800.0, 600.0));

        assert_eq!(layout[0].0, "a");
        assert_eq!(layout[1].0, p2);
        assert_eq!(layout[2].0, p3);
        assert_eq!(
            layout[0],
            ("a".to_owned(), container(0.0, 0.0, 400.0, 600.0))
        );
        assert_eq!(layout[1], (p2.clone(), container(400.0, 0.0, 400.0, 300.0)));
        assert_eq!(
            layout[2],
            (p3.clone(), container(400.0, 300.0, 400.0, 300.0))
        );
    }

    #[test]
    fn close_collapses_the_parent_split() {
        let mut tree = PaneTree::new(dummy("a"));
        tree.split_leaf("a", SplitDirection::Right, dummy("p2"));
        tree.split_leaf("p2", SplitDirection::Down, dummy("p3"));

        assert!(tree.close_leaf("p2"));
        assert_eq!(tree.leaf_count(), 2);
        assert_eq!(tree.ordered_leaf_ids(), ["a", "p3"]);
        // p3 collapsed into p2's parent split; a is still first.
        let layout = tree.layout(container(0.0, 0.0, 800.0, 600.0));
        assert_eq!(
            layout[1],
            ("p3".to_owned(), container(400.0, 0.0, 400.0, 600.0))
        );

        assert!(tree.close_leaf("p3"));
        assert_eq!(tree.leaf_count(), 1);
        // The last pane cannot be closed from the tree.
        assert!(!tree.close_leaf("a"));
    }

    #[test]
    fn splitting_an_absent_pane_is_a_no_op() {
        let mut tree = PaneTree::new(dummy("a"));
        assert!(tree
            .split_leaf("zz", SplitDirection::Right, dummy("p2"))
            .is_none());
        assert_eq!(tree.leaf_count(), 1);
        assert!(!tree.close_leaf("zz"));
        assert_eq!(tree.leaf_count(), 1);
    }

    #[test]
    fn resize_moves_the_controlling_divider() {
        let mut tree = PaneTree::new(dummy("a"));
        tree.split_leaf("a", SplitDirection::Right, dummy("p2"));
        tree.layout(container(0.0, 0.0, 800.0, 600.0));

        // "a" sits in the first child, so .right grows its share by 80/800px.
        tree.resize_pane("a", ResizeDirection::Right, 80);

        let layout = tree.layout(container(0.0, 0.0, 800.0, 600.0));
        assert_eq!(layout[0].1.width, 480.0);
        assert_eq!(layout[1].1.x, 480.0);

        // Clamped at the legacy 0.9 bound.
        tree.resize_pane("a", ResizeDirection::Right, 1600);
        let layout = tree.layout(container(0.0, 0.0, 800.0, 600.0));
        assert_eq!(layout[0].1.width, 720.0);
    }

    #[test]
    fn equalize_rebalances_nested_rows() {
        let mut tree = PaneTree::new(dummy("a"));
        tree.split_leaf("a", SplitDirection::Right, dummy("p2"));
        tree.split_leaf("p2", SplitDirection::Right, dummy("p3"));
        tree.layout(container(0.0, 0.0, 900.0, 300.0));

        tree.equalize(None);

        let layout = tree.layout(container(0.0, 0.0, 900.0, 300.0));
        assert_eq!(layout[0].1.x, 0.0);
        assert_eq!(layout[0].1.width, 300.0);
        assert_eq!(layout[1].1.x, 300.0);
        assert_eq!(layout[1].1.width, 300.0);
        assert_eq!(layout[2].1.x, 600.0);
        assert_eq!(layout[2].1.width, 300.0);
    }

    #[test]
    fn equalize_run_targets_only_the_panes_row() {
        // A vertical split over (a | row of two): the run that contains the
        // row's pane stops at the cross-orientation ancestor, leaving the
        // vertical divider at its position.
        let mut tree = PaneTree::new(dummy("a"));
        tree.split_leaf("a", SplitDirection::Down, dummy("p2"));
        tree.layout(container(0.0, 0.0, 800.0, 600.0));
        tree.resize_pane("a", ResizeDirection::Down, 120); // vertical divider 0.7
        let p3 = tree.next_leaf_id();
        tree.split_leaf("p2", SplitDirection::Right, dummy(&p3));
        tree.layout(container(0.0, 0.0, 800.0, 600.0));
        // Skew the row: p2 keeps 60%.
        tree.resize_pane("p2", ResizeDirection::Right, 80);

        tree.equalize_run_containing("p3");

        let layout = tree.layout(container(0.0, 0.0, 800.0, 600.0));
        // The row is rebalanced to equal halves…
        assert_eq!(layout[1].1.width, 400.0);
        assert_eq!(layout[2].1.width, 400.0);
        // …and the vertical divider above is untouched (a keeps 70% height).
        assert_eq!(layout[0].1.height, 420.0);
        assert_eq!(layout[1].1.y, 420.0);
    }

    #[test]
    fn neighbor_sides_mark_only_touching_sides() {
        let layout = vec![
            ("a".to_owned(), container(0.0, 0.0, 400.0, 600.0)),
            ("b".to_owned(), container(400.0, 0.0, 400.0, 600.0)),
        ];

        let sides = neighbor_sides(&layout, "a");
        assert!(sides.right);
        assert!(!sides.left && !sides.top && !sides.bottom);

        let sides = neighbor_sides(&layout, "b");
        assert!(sides.left);
        assert!(!sides.right && !sides.top && !sides.bottom);
    }

    #[test]
    fn neighbor_sides_follow_overlap_not_extent_equality() {
        // A full-height pane left of a column of two stacked panes.
        let layout = vec![
            ("a".to_owned(), container(0.0, 0.0, 300.0, 600.0)),
            ("t".to_owned(), container(300.0, 0.0, 500.0, 300.0)),
            ("b".to_owned(), container(300.0, 300.0, 500.0, 300.0)),
        ];

        let sides = neighbor_sides(&layout, "a");
        assert!(sides.right);
        // "a"'s other sides sit on the window frame — flush.
        assert!(!sides.left && !sides.top && !sides.bottom);

        // The stacked panes neighbor their horizontal counterpart…
        let sides = neighbor_sides(&layout, "t");
        assert!(sides.left && sides.bottom);
        assert!(!sides.top && !sides.right);

        let sides = neighbor_sides(&layout, "b");
        assert!(sides.left && sides.top);
        assert!(!sides.bottom && !sides.right);
    }

    #[test]
    fn gutter_inset_halves_the_shared_gutter() {
        let layout = vec![
            ("a".to_owned(), container(0.0, 0.0, 400.0, 600.0)),
            ("b".to_owned(), container(400.0, 0.0, 400.0, 600.0)),
        ];

        let inset = gutter_inset(&layout, "a", 12.0);
        assert_eq!(
            inset,
            GutterInset {
                left: 0.0,
                right: 6.0,
                top: 0.0,
                bottom: 0.0
            }
        );

        let inset = gutter_inset(&layout, "b", 12.0);
        assert_eq!(
            inset,
            GutterInset {
                left: 6.0,
                right: 0.0,
                top: 0.0,
                bottom: 0.0
            }
        );
    }

    #[test]
    fn no_inset_for_single_pane_or_unknown_ids() {
        let layout = vec![("a".to_owned(), container(0.0, 0.0, 100.0, 100.0))];
        let zero = GutterInset {
            left: 0.0,
            right: 0.0,
            top: 0.0,
            bottom: 0.0,
        };
        assert_eq!(gutter_inset(&layout, "a", 12.0), zero);
        assert_eq!(gutter_inset(&layout, "zz", 12.0), zero);
    }

    #[test]
    fn outline_reflects_split_shape_and_join_rebuilds_it() {
        let mut left = PaneTree::new(dummy("p1"));
        let left_id = left.next_leaf_id();
        left.split_leaf("p1", SplitDirection::Right, dummy(&left_id))
            .unwrap_or_else(|| panic!("split must land"));

        // The outline mirrors the live split (divider included).
        assert_eq!(
            left.root_outline(),
            TreeOutline::Split {
                orientation: Orientation::Horizontal,
                divider: 0.5,
                first: Box::new(TreeOutline::Leaf {
                    id: "p1".to_owned(),
                }),
                second: Box::new(TreeOutline::Leaf {
                    id: "p2".to_owned(),
                }),
            }
        );

        // `joined` rebuilds a root split with the snapshot's geometry.
        let mut joined = PaneTree::joined(
            PaneTree::new(dummy("p1")),
            PaneTree::new(dummy("p9")),
            Orientation::Horizontal,
            0.4,
            "s-res".to_owned(),
        );
        assert_eq!(
            joined.root_outline(),
            TreeOutline::Split {
                orientation: Orientation::Horizontal,
                divider: 0.4,
                first: Box::new(TreeOutline::Leaf {
                    id: "p1".to_owned(),
                }),
                second: Box::new(TreeOutline::Leaf {
                    id: "p9".to_owned(),
                }),
            }
        );
        let layout = joined.layout(container(0.0, 0.0, 100.0, 50.0));
        let p1 = layout
            .iter()
            .find(|(id, _)| id == "p1")
            .expect("p1 laid out");
        assert!(
            (p1.1.width - 40.0).abs() < 0.001,
            "first child takes the divider's 40%: {p1:?}"
        );
        let p9 = layout
            .iter()
            .find(|(id, _)| id == "p9")
            .expect("p9 laid out");
        assert!(
            (p9.1.width - 60.0).abs() < 0.001,
            "second child takes the rest: {p9:?}"
        );
    }

    #[test]
    fn seed_next_id_lifts_the_counter() {
        let mut tree = PaneTree::joined(
            PaneTree::new(dummy("p5")),
            PaneTree::new(dummy("p8")),
            Orientation::Vertical,
            0.5,
            "s-res".to_owned(),
        );
        tree.seed_next_id(9);
        assert_eq!(tree.next_leaf_id(), "p9");
        // Seeding never lowers the counter.
        tree.seed_next_id(3);
        assert_eq!(tree.next_leaf_id(), "p10");
    }

    // ----- M4.1: global id sources -----

    #[test]
    fn global_pane_mints_are_monotonic_and_distinct() {
        // Assertions stay relative: other tests mint concurrently and
        // Rust's unit tests share the process's atomics.
        let first = mint_pane_id();
        let second = mint_pane_id();
        assert_ne!(first, second);
        let first_n: u64 = first.strip_prefix('p').unwrap().parse().unwrap();
        let second_n: u64 = second.strip_prefix('p').unwrap().parse().unwrap();
        assert!(second_n > first_n, "{first} must precede {second}");
    }

    #[test]
    fn pane_seeding_lifts_above_restored_ids() {
        // One past the restored ids: a mint after seeding lands past them.
        seed_pane_ids(seed_past_pane_ids([
            "p3".into(),
            "p17".into(),
            "foo".into(),
        ]));
        let minted = mint_pane_id();
        let n: u64 = minted.strip_prefix('p').unwrap().parse().unwrap();
        assert!(n >= 18, "seeded mints stay past p17: {minted}");
        // Seeding below the current position never regresses the counter.
        seed_pane_ids(1);
        let after = mint_pane_id();
        let after_n: u64 = after.strip_prefix('p').unwrap().parse().unwrap();
        assert!(after_n > n, "no regression after a low seed: {after}");
    }

    #[test]
    fn workspace_mints_are_monotonic_and_seeded() {
        let first = mint_workspace_id();
        let second = mint_workspace_id();
        assert_ne!(first, second);
        let first_n: u64 = first.strip_prefix('w').unwrap().parse().unwrap();
        let second_n: u64 = second.strip_prefix('w').unwrap().parse().unwrap();
        assert!(second_n > first_n, "{first} must precede {second}");

        seed_workspace_ids(seed_past_workspace_ids(["w2".into()]));
        let minted = mint_workspace_id();
        let n: u64 = minted.strip_prefix('w').unwrap().parse().unwrap();
        assert!(n >= 3, "seeded workspace mints stay past w2: {minted}");
    }
}
