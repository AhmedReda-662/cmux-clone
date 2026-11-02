//! Pane-tree split geometry for the cmux Windows port.
//!
//! One-for-one port of the pure-geometry half of cmux's macOS `CmuxPanes`
//! package (`cmux-main/Packages/macOS/CmuxPanes/Sources/CmuxPanes`,
//! GPL-3.0-or-later): equalize and resize planning
//! (`ExternalTreeNode+SplitGeometry`), split-space verdicts
//! (`ExternalTreeNode+SplitSpace`), spatial ordering
//! (`ExternalTreeNode+SpatialOrder`, `SpatialPanelOrder`), focus cycling
//! (`PaneCycleNavigator`), and the direction values (`SplitDirection`,
//! `ResizeDirection`). Pure functions over a snapshot tree — no OS, I/O, or
//! UI code lives here; the terminal UI applies the produced adjustments in
//! its own split rendering.
//!
//! Deliberate adaptations, where the port owns the data types:
//! - Split ids are plain `String`s; there is no UUID parsing, so the Swift
//!   `hadInvalidSplitIds` flag has no equivalent (ids are validated when
//!   session snapshots are decoded, in the planned config milestone).
//! - Orientations are `Orientation` values instead of raw `"horizontal"`
//!   / `"vertical"` strings (snapshot strings round-trip via
//!   [`Orientation::parse`] / [`Orientation::as_str`]).
//! - Pane nodes carry no tab metadata; panel ordering receives a separate
//!   `pane -> [panel]` map, exactly like `SpatialPanelOrder` does.
//!
//! Dimensions are `f64` axis units (Swift `CGFloat` on 64-bit, cmux frames
//! in points), so numeric results match upstream bit-for-bit.

use std::collections::HashMap;
use std::hash::Hash;

/// The split axis: `Horizontal` means side by side (left/right),
/// `Vertical` means stacked (top/bottom).
///
/// Mirrors cmux's `SessionSplitOrientation` snapshot value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl Orientation {
    /// Whether the axis divides width (left/right halves).
    pub fn is_horizontal(self) -> bool {
        matches!(self, Orientation::Horizontal)
    }

    /// The orientation string used by cmux session snapshots.
    pub fn as_str(self) -> &'static str {
        match self {
            Orientation::Horizontal => "horizontal",
            Orientation::Vertical => "vertical",
        }
    }

    /// Parses the orientation string used by cmux session snapshots.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "horizontal" => Some(Orientation::Horizontal),
            "vertical" => Some(Orientation::Vertical),
            _ => None,
        }
    }
}

/// A pane's rectangle in the container coordinate space (`PixelRect` upstream;
/// cmux frames are in points).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PixelRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl PixelRect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    /// The least rectangle enclosing both (Swift `CGRect.union`).
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        PixelRect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
}

/// A leaf pane: identifier plus its measured frame.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneNode {
    pub id: String,
    pub frame: PixelRect,
}

/// An inner split node: axis, the normalized (0.0–1.0) first-child divider
/// position, and the two children (first = left/top, second = right/bottom).
#[derive(Clone, Debug, PartialEq)]
pub struct SplitNode {
    pub id: String,
    pub orientation: Orientation,
    pub divider_position: f64,
    pub first: Box<TreeNode>,
    pub second: Box<TreeNode>,
}

/// Bonsplit's external tree snapshot shape (`ExternalTreeNode` upstream).
#[derive(Clone, Debug, PartialEq)]
pub enum TreeNode {
    Pane(PaneNode),
    Split(SplitNode),
}

/// One planned divider move: the split to adjust and the normalized
/// (0.0–1.0) divider position to set on it (`SplitDividerAdjustment`).
#[derive(Clone, Debug, PartialEq)]
pub struct DividerAdjustment {
    pub split_id: String,
    pub position: f64,
}

/// The result of an equalize planning pass (`SplitEqualizePlan`, without the
/// `hadInvalidSplitIds` flag — see the crate docs for why).
#[derive(Clone, Debug, PartialEq)]
pub struct EqualizePlan {
    pub adjustments: Vec<DividerAdjustment>,
    /// At least one split matched; `false` means the pass would do nothing.
    pub found_split: bool,
}

impl EqualizePlan {
    fn empty() -> Self {
        EqualizePlan {
            adjustments: Vec::new(),
            found_split: false,
        }
    }
}

/// Split direction for inserting a new pane (`SplitDirection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    /// Insert the new pane to the left of the source pane.
    Left,
    /// Insert the new pane to the right of the source pane.
    Right,
    /// Insert the new pane above the source pane.
    Up,
    /// Insert the new pane below the source pane.
    Down,
}

impl SplitDirection {
    /// Whether the split divides space horizontally (left/right).
    pub fn is_horizontal(self) -> bool {
        matches!(self, SplitDirection::Left | SplitDirection::Right)
    }

    /// The axis for the new split.
    pub fn orientation(self) -> Orientation {
        if self.is_horizontal() {
            Orientation::Horizontal
        } else {
            Orientation::Vertical
        }
    }

    /// If true, insert the new pane on the "first" side (left/top).
    pub fn insert_first(self) -> bool {
        matches!(self, SplitDirection::Left | SplitDirection::Up)
    }
}

/// Resize direction for keyboard/resize commands (`ResizeDirection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeDirection {
    /// Move the controlling divider left.
    Left,
    /// Move the controlling divider right.
    Right,
    /// Move the controlling divider up.
    Up,
    /// Move the controlling divider down.
    Down,
}

impl ResizeDirection {
    /// The axis of the split whose divider this resize moves.
    pub fn split_orientation(self) -> Orientation {
        match self {
            ResizeDirection::Left | ResizeDirection::Right => Orientation::Horizontal,
            ResizeDirection::Up | ResizeDirection::Down => Orientation::Vertical,
        }
    }

    /// A split controls the target pane's right/bottom edge when the target
    /// is the first child, and left/top edge when it is the second child.
    pub fn requires_pane_in_first_child(self) -> bool {
        matches!(self, ResizeDirection::Right | ResizeDirection::Down)
    }

    /// Positive values move the divider toward the second child (right/down).
    pub fn divider_delta_sign(self) -> f64 {
        if self.requires_pane_in_first_child() {
            1.0
        } else {
            -1.0
        }
    }
}

/// Whether a pane can be split without leaving a pane below the minimum size
/// (`SplitSpaceVerdict`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitSpaceVerdict {
    /// Both halves of the split meet the minimum as the layout stands.
    Fits,
    /// The halves would be too small, but equalizing the run of
    /// same-orientation splits that will hold them gives every pane in the
    /// run the minimum. Apply the split, then equalize that run.
    FitsAfterEqualizingRun,
    /// No layout of the run fits another pane: refuse the split, the way
    /// tmux answers "no space for new pane".
    NoSpace,
}

/// The normalized divider range Bonsplit enforces (clamped on both apply and
/// plan paths, upstream `splitSpaceClampedDividerPosition` and the 0.1–0.9
/// resize clamp).
const DIVIDER_RANGE: (f64, f64) = (0.1, 0.9);

fn clamped_divider_position(position: f64) -> f64 {
    position.clamp(DIVIDER_RANGE.0, DIVIDER_RANGE.1)
}

/// The split nodes from the tree root down to `pane_id`'s parent (outermost
/// first), plus the pane's own node; `None` when the pane is absent. Root
/// panes return an empty split list.
fn split_path<'a>(tree: &'a TreeNode, pane_id: &str) -> Option<(Vec<&'a TreeNode>, &'a PaneNode)> {
    match tree {
        TreeNode::Pane(pane) => {
            if pane.id == pane_id {
                Some((Vec::new(), pane))
            } else {
                None
            }
        }
        TreeNode::Split(split) => {
            let (mut splits, pane) =
                split_path(&split.first, pane_id).or_else(|| split_path(&split.second, pane_id))?;
            splits.insert(0, tree);
            Some((splits, pane))
        }
    }
}

/// Destructures a snapshot node for its split half. Only valid on nodes
/// returned inside a [`split_path`] list, which are always splits.
fn as_split(node: &TreeNode) -> &SplitNode {
    match node {
        TreeNode::Split(split) => split,
        TreeNode::Pane(_) => unreachable!("split_path only returns split nodes"),
    }
}

/// Appends one equalize adjustment for `split`: children counted as spans
/// along `along` give the normalized first-child share.
fn push_divider_position(split: &SplitNode, along: Orientation, plan: &mut EqualizePlan) {
    let first = split.first.span_count(along);
    let second = split.second.span_count(along);
    plan.adjustments.push(DividerAdjustment {
        split_id: split.id.clone(),
        position: first as f64 / (first + second) as f64,
    });
}

/// Post-order equalize over a same-orientation run: recurses only into
/// children that continue the run.
fn append_run_equalize_adjustments(
    split: &SplitNode,
    orientation: Orientation,
    plan: &mut EqualizePlan,
) {
    append_run_equalize_adjustments_node(&split.first, orientation, plan);
    append_run_equalize_adjustments_node(&split.second, orientation, plan);
    plan.found_split = true;
    push_divider_position(split, orientation, plan);
}

fn append_run_equalize_adjustments_node(
    node: &TreeNode,
    orientation: Orientation,
    plan: &mut EqualizePlan,
) {
    if let TreeNode::Split(split) = node {
        if split.orientation == orientation {
            append_run_equalize_adjustments(split, orientation, plan);
        }
    }
}

/// Resolves wrapping previous/next pane focus targets from a stable pane
/// order (`PaneCycleNavigator`). Stateless; create on demand.
#[derive(Clone, Copy, Debug, Default)]
pub struct PaneCycleNavigator;

impl PaneCycleNavigator {
    /// Returns the pane reached by cycling one step from the focused pane.
    ///
    /// `ordered_pane_ids` is the authoritative visual/tree order;
    /// `live_pane_ids` is the controller's current pane set, used to discard
    /// stale ordered ids before indexing. Returns `None` when cycling is not
    /// possible (fewer than two live ordered panes, or no focused pane in
    /// them).
    pub fn target_pane<T: PartialEq + Clone>(
        &self,
        ordered_pane_ids: &[T],
        live_pane_ids: &[T],
        focused_pane_id: Option<&T>,
        forward: bool,
    ) -> Option<T> {
        let ordered_live: Vec<&T> = ordered_pane_ids
            .iter()
            .filter(|id| live_pane_ids.contains(id))
            .collect();
        let focused = focused_pane_id?;
        let current = ordered_live.iter().position(|id| *id == focused)?;
        if ordered_live.len() <= 1 {
            return None;
        }
        let target = if forward {
            (current + 1) % ordered_live.len()
        } else {
            (current + ordered_live.len() - 1) % ordered_live.len()
        };
        Some(ordered_live[target].clone())
    }
}

/// Orders panel ids from pane order alone, without reading clipped pane
/// geometry (`SpatialPanelOrder` upstream): panes appear in the given order,
/// panels within each pane in that pane's tab order, then any panels the
/// panes never listed in `fallback`'s stable order. Repeated panels are
/// dropped after first appearance.
pub fn panel_ids_in_order<T: Eq + Hash + Clone>(
    ordered_pane_ids: &[String],
    pane_tabs: &HashMap<String, Vec<T>>,
    fallback_panel_ids: &[T],
) -> Vec<T> {
    let mut ordered: Vec<T> = Vec::new();
    let mut seen: std::collections::HashSet<T> = std::collections::HashSet::new();
    for pane_id in ordered_pane_ids {
        if let Some(tabs) = pane_tabs.get(pane_id) {
            for panel in tabs {
                if seen.insert(panel.clone()) {
                    ordered.push(panel.clone());
                }
            }
        }
    }
    for panel in fallback_panel_ids {
        if seen.insert(panel.clone()) {
            ordered.push(panel.clone());
        }
    }
    ordered
}

impl TreeNode {
    /// Builds a leaf pane.
    pub fn pane(id: impl Into<String>, frame: PixelRect) -> Self {
        TreeNode::Pane(PaneNode {
            id: id.into(),
            frame,
        })
    }

    /// Builds a split node over the two children.
    pub fn split(
        id: impl Into<String>,
        orientation: Orientation,
        divider_position: f64,
        first: TreeNode,
        second: TreeNode,
    ) -> Self {
        TreeNode::Split(SplitNode {
            id: id.into(),
            orientation,
            divider_position,
            first: Box::new(first),
            second: Box::new(second),
        })
    }

    /// Pane ids in on-screen spatial order: depth-first over the split tree,
    /// first/top child before second/bottom child (`orderedPaneIds`).
    pub fn ordered_pane_ids(&self) -> Vec<String> {
        match self {
            TreeNode::Pane(pane) => vec![pane.id.clone()],
            TreeNode::Split(split) => {
                let mut ids = split.first.ordered_pane_ids();
                ids.extend(split.second.ordered_pane_ids());
                ids
            }
        }
    }

    /// Panel ids in on-screen spatial order: panes in [`Self::ordered_pane_ids`]
    /// order, tabs within each pane in tab order, then any panels missing
    /// from the tree in `fallback`'s stable order (`orderedPanelIds`).
    pub fn ordered_panel_ids<T: Eq + Hash + Clone>(
        &self,
        pane_tabs: &HashMap<String, Vec<T>>,
        fallback_panel_ids: &[T],
    ) -> Vec<T> {
        panel_ids_in_order(&self.ordered_pane_ids(), pane_tabs, fallback_panel_ids)
    }

    // ----- equalize planning -----

    /// Plans an equalize pass: every split matching `orientation_filter`
    /// (or every split when `None`) gets its divider set so that each
    /// same-orientation leaf span receives equal space
    /// (`equalizeDividerPlan(orientationFilter:)`).
    pub fn equalize_plan(&self, orientation_filter: Option<Orientation>) -> EqualizePlan {
        let mut plan = EqualizePlan::empty();
        self.append_equalize_adjustments(orientation_filter, &mut plan);
        plan
    }

    fn append_equalize_adjustments(
        &self,
        orientation_filter: Option<Orientation>,
        plan: &mut EqualizePlan,
    ) {
        if let TreeNode::Split(split) = self {
            split
                .first
                .append_equalize_adjustments(orientation_filter, plan);
            split
                .second
                .append_equalize_adjustments(orientation_filter, plan);
            if orientation_filter.is_none_or(|f| f == split.orientation) {
                plan.found_split = true;
                push_divider_position(split, split.orientation, plan);
            }
        }
    }

    /// Plans an equalize pass over the run of same-orientation splits that
    /// directly contains `pane_id`: the pane's parent split plus every
    /// ancestor reached without crossing a split of another orientation, and
    /// the same-orientation splits nested inside that run. Splits elsewhere
    /// in the tree, including same-orientation splits behind a
    /// cross-orientation subtree, are left alone. Returns an empty plan when
    /// `pane_id` is absent or has no parent split
    /// (`equalizeDividerPlan(forSplitRunContainingPaneId:)`).
    pub fn equalize_plan_for_split_run_containing(&self, pane_id: &str) -> EqualizePlan {
        let mut plan = EqualizePlan::empty();
        let Some((splits, _)) = split_path(self, pane_id) else {
            return plan;
        };
        let Some(parent) = splits.last() else {
            return plan; // root pane: no parent split to equalize
        };
        let parent_orientation = as_split(parent).orientation;
        let mut run_root = parent;
        for ancestor in splits[..splits.len() - 1].iter().rev() {
            if as_split(ancestor).orientation != parent_orientation {
                break;
            }
            run_root = ancestor;
        }
        append_run_equalize_adjustments(as_split(run_root), parent_orientation, &mut plan);
        plan
    }

    /// The number of same-orientation leaf spans in this subtree: a
    /// cross-orientation subtree counts as one span (`spanCount(along:)`).
    pub fn span_count(&self, along: Orientation) -> usize {
        match self {
            TreeNode::Pane(_) => 1,
            TreeNode::Split(split) => {
                if split.orientation != along {
                    return 1;
                }
                split.first.span_count(along) + split.second.span_count(along)
            }
        }
    }

    // ----- resize planning -----

    /// Plans a keyboard resize of the pane's controlling divider: walks the
    /// tree for the splits enclosing `target_pane_id` (innermost first, the
    /// legacy candidate order), picks the first split matching the resize
    /// direction's orientation and child side, and converts `amount_pixels`
    /// into a divider delta along that split's axis, clamped to 0.1–0.9.
    /// Returns `None` when the pane is absent or no enclosing split matches
    /// (`resizeDividerAdjustment`).
    pub fn resize_divider_adjustment(
        &self,
        target_pane_id: &str,
        direction: ResizeDirection,
        amount_pixels: u16,
    ) -> Option<DividerAdjustment> {
        let mut candidates: Vec<ResizeCandidate<'_>> = Vec::new();
        let trace = self.collect_resize_candidates(target_pane_id, &mut candidates);
        if !trace.contains_target {
            return None;
        }
        candidates
            .iter()
            .find(|candidate| {
                candidate.orientation == direction.split_orientation()
                    && candidate.pane_in_first_child == direction.requires_pane_in_first_child()
            })
            .map(|candidate| {
                let delta = f64::from(amount_pixels) / candidate.axis_pixels;
                let requested = candidate.divider_position + direction.divider_delta_sign() * delta;
                DividerAdjustment {
                    split_id: candidate.split_id.to_owned(),
                    position: clamped_divider_position(requested),
                }
            })
    }

    fn collect_resize_candidates<'a>(
        &'a self,
        target_pane_id: &str,
        candidates: &mut Vec<ResizeCandidate<'a>>,
    ) -> ResizeTrace {
        match self {
            TreeNode::Pane(pane) => ResizeTrace {
                contains_target: pane.id == target_pane_id,
                bounds: pane.frame,
            },
            TreeNode::Split(split) => {
                let first = split
                    .first
                    .collect_resize_candidates(target_pane_id, candidates);
                let second = split
                    .second
                    .collect_resize_candidates(target_pane_id, candidates);
                let combined_bounds = first.bounds.union(second.bounds);
                let contains_target = first.contains_target || second.contains_target;
                if contains_target {
                    let axis_pixels = match split.orientation {
                        Orientation::Horizontal => combined_bounds.width,
                        Orientation::Vertical => combined_bounds.height,
                    }
                    .max(1.0);
                    candidates.push(ResizeCandidate {
                        split_id: &split.id,
                        orientation: split.orientation,
                        pane_in_first_child: first.contains_target,
                        divider_position: split.divider_position,
                        axis_pixels,
                    });
                }
                ResizeTrace {
                    contains_target,
                    bounds: combined_bounds,
                }
            }
        }
    }

    // ----- split-space verdicts -----

    /// Decides whether splitting `pane_id` along `orientation` fits
    /// (`splitSpaceVerdict`). A split halves the pane, so it fits in place
    /// when each half keeps `minimum_extent` along the split axis. Otherwise
    /// it may borrow from the panes it stacks with: the pane's parent split
    /// and every ancestor of the same orientation form one run, and
    /// equalizing that run after the split gives each of its slots an equal
    /// share. A slot that is itself split across the axis needs the most its
    /// children need. Frames of zero extent mean the layout has not been
    /// measured yet, so the split is allowed rather than refused on unknown
    /// geometry — the same is true for a pane missing from the tree.
    ///
    /// `divider_position` is the requested first-child ratio; `None` uses the
    /// 0.5 default (requests are clamped to 0.1–0.9 either way).
    // `!(extent > 0.0)` below deliberately mirrors Swift's
    // `guard paneExtent > 0 else { return .fits }`: NaN also counts as
    // unmeasured, which `<= 0.0` would not preserve.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    pub fn split_space_verdict(
        &self,
        splitting_pane_id: &str,
        orientation: Orientation,
        minimum_extent: f64,
        divider_thickness: f64,
        divider_position: Option<f64>,
    ) -> SplitSpaceVerdict {
        let Some((splits, pane)) = split_path(self, splitting_pane_id) else {
            return SplitSpaceVerdict::Fits;
        };
        let pane_extent = pane_axis_extent(pane.frame, orientation);
        if !(pane_extent > 0.0) {
            return SplitSpaceVerdict::Fits; // unmeasured layout
        }
        let position = clamped_divider_position(divider_position.unwrap_or(0.5));
        let available = pane_extent - divider_thickness;
        if available * position >= minimum_extent && available * (1.0 - position) >= minimum_extent
        {
            return SplitSpaceVerdict::Fits;
        }

        let Some(parent) = splits.last() else {
            return SplitSpaceVerdict::NoSpace; // root pane: no run to borrow from
        };
        if as_split(parent).orientation != orientation {
            return SplitSpaceVerdict::NoSpace;
        }
        let mut run_root = parent;
        for ancestor in splits[..splits.len() - 1].iter().rev() {
            if as_split(ancestor).orientation != orientation {
                break;
            }
            run_root = ancestor;
        }
        let run_extent = run_root.axis_extent(orientation);
        if fits_after_equalizing_run(
            run_root,
            run_extent,
            orientation,
            splitting_pane_id,
            minimum_extent,
            divider_thickness,
        ) {
            SplitSpaceVerdict::FitsAfterEqualizingRun
        } else {
            SplitSpaceVerdict::NoSpace
        }
    }

    /// The extent along the axis covered by this subtree's pane frames
    /// (`axisExtent`); 0 when the subtree has no panes (does not occur for
    /// trees built by this crate).
    pub fn axis_extent(&self, orientation: Orientation) -> f64 {
        match self.axis_span(orientation.is_horizontal()) {
            Some((lower, upper)) => upper - lower,
            None => 0.0,
        }
    }

    fn axis_span(&self, is_horizontal: bool) -> Option<(f64, f64)> {
        match self {
            TreeNode::Pane(pane) => {
                let (start, length) = if is_horizontal {
                    (pane.frame.x, pane.frame.width)
                } else {
                    (pane.frame.y, pane.frame.height)
                };
                Some((start, start + length.max(0.0)))
            }
            TreeNode::Split(split) => {
                let first = split.first.axis_span(is_horizontal);
                let second = split.second.axis_span(is_horizontal);
                match (first, second) {
                    (Some((l1, u1)), Some((l2, u2))) => Some((l1.min(l2), u1.max(u2))),
                    (one, other) => one.or(other),
                }
            }
        }
    }
}

/// The pane's own extent along the split axis (leaf `axisExtent`).
fn pane_axis_extent(frame: PixelRect, orientation: Orientation) -> f64 {
    let length = if orientation.is_horizontal() {
        frame.width
    } else {
        frame.height
    };
    length.max(0.0)
}

/// Simulates the exact divider positions that the split controller applies
/// when the run is equalized. Ratios outside 0.1–0.9 are clamped, so
/// averaging all slots would otherwise overestimate the smaller side of an
/// 11-or-more-slot run (`fitsAfterEqualizingRun`).
fn fits_after_equalizing_run(
    node: &TreeNode,
    extent: f64,
    orientation: Orientation,
    splitting_pane_id: &str,
    minimum_extent: f64,
    divider_thickness: f64,
) -> bool {
    match node {
        TreeNode::Pane(pane) => {
            if pane.id == splitting_pane_id {
                (extent - divider_thickness) / 2.0 >= minimum_extent
            } else {
                extent >= minimum_extent
            }
        }
        TreeNode::Split(split) => {
            if split.orientation != orientation {
                return required_extent(node, orientation, minimum_extent, divider_thickness)
                    <= extent;
            }
            let first_span = prospective_span_count(&split.first, orientation, splitting_pane_id);
            let second_span = prospective_span_count(&split.second, orientation, splitting_pane_id);
            let available = extent - divider_thickness;
            if available < 0.0 {
                return false;
            }
            let position =
                clamped_divider_position(first_span as f64 / (first_span + second_span) as f64);
            fits_after_equalizing_run(
                &split.first,
                available * position,
                orientation,
                splitting_pane_id,
                minimum_extent,
                divider_thickness,
            ) && fits_after_equalizing_run(
                &split.second,
                available * (1.0 - position),
                orientation,
                splitting_pane_id,
                minimum_extent,
                divider_thickness,
            )
        }
    }
}

/// The number of leaf spans this subtree will pose after `splitting_pane_id`
/// is split: the splitting slot counts two, a cross-orientation subtree
/// counts one (`prospectiveSpanCount`).
fn prospective_span_count(
    node: &TreeNode,
    orientation: Orientation,
    splitting_pane_id: &str,
) -> usize {
    match node {
        TreeNode::Pane(pane) => {
            if pane.id == splitting_pane_id {
                2
            } else {
                1
            }
        }
        TreeNode::Split(split) => {
            if split.orientation != orientation {
                return 1;
            }
            prospective_span_count(&split.first, orientation, splitting_pane_id)
                + prospective_span_count(&split.second, orientation, splitting_pane_id)
        }
    }
}

/// The smallest extent along `orientation` that keeps every pane in this
/// subtree at `minimum_extent` (`requiredExtent`).
fn required_extent(
    node: &TreeNode,
    orientation: Orientation,
    minimum_extent: f64,
    divider_thickness: f64,
) -> f64 {
    match node {
        TreeNode::Pane(_) => minimum_extent,
        TreeNode::Split(split) => {
            let first =
                required_extent(&split.first, orientation, minimum_extent, divider_thickness);
            let second = required_extent(
                &split.second,
                orientation,
                minimum_extent,
                divider_thickness,
            );
            if split.orientation != orientation {
                return first.max(second);
            }
            let position = clamped_divider_position(split.divider_position);
            (first / position).max(second / (1.0 - position)) + divider_thickness
        }
    }
}

struct ResizeCandidate<'a> {
    split_id: &'a str,
    orientation: Orientation,
    pane_in_first_child: bool,
    divider_position: f64,
    axis_pixels: f64,
}

struct ResizeTrace {
    contains_target: bool,
    bounds: PixelRect,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test builders mirroring the upstream Swift suites.

    fn pane(id: &str, x: f64, y: f64, width: f64, height: f64) -> TreeNode {
        TreeNode::pane(id, PixelRect::new(x, y, width, height))
    }

    fn split(
        id: &str,
        orientation: Orientation,
        divider_position: f64,
        first: TreeNode,
        second: TreeNode,
    ) -> TreeNode {
        TreeNode::split(id, orientation, divider_position, first, second)
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.0001
    }

    // ----- Equalize planning (SplitGeometryTests) -----

    #[test]
    fn equalize_weights_nested_same_orientation_spans() {
        let tree = split(
            "outer",
            Orientation::Horizontal,
            0.8,
            pane("a", 0.0, 0.0, 100.0, 100.0),
            split(
                "inner",
                Orientation::Horizontal,
                0.2,
                pane("b", 80.0, 0.0, 10.0, 100.0),
                pane("c", 90.0, 0.0, 10.0, 100.0),
            ),
        );

        let plan = tree.equalize_plan(None);

        assert!(plan.found_split);
        // Children are planned before their parent (legacy post-order).
        assert_eq!(
            plan.adjustments
                .iter()
                .map(|a| a.split_id.as_str())
                .collect::<Vec<_>>(),
            ["inner", "outer"]
        );
        // Inner split divides its two leaves evenly; the outer divider gives
        // its first child 1 of 3 same-orientation spans.
        assert_eq!(plan.adjustments[0].position, 0.5);
        assert!(near(plan.adjustments[1].position, 1.0 / 3.0));
    }

    #[test]
    fn equalize_treats_cross_orientation_subtree_as_one_span() {
        let tree = split(
            "outer",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 50.0, 100.0),
            split(
                "inner",
                Orientation::Vertical,
                0.5,
                pane("b", 50.0, 0.0, 50.0, 50.0),
                pane("c", 50.0, 50.0, 50.0, 50.0),
            ),
        );

        let plan = tree.equalize_plan(None);

        // The vertical subtree counts as a single horizontal span, so the
        // outer divider lands at 1/2.
        assert!(plan.adjustments.iter().all(|a| a.position == 0.5));
    }

    #[test]
    fn equalize_orientation_filter_skips_other_orientations() {
        let tree = split(
            "outer",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 50.0, 100.0),
            split(
                "inner",
                Orientation::Vertical,
                0.5,
                pane("b", 50.0, 0.0, 50.0, 50.0),
                pane("c", 50.0, 50.0, 50.0, 50.0),
            ),
        );

        let plan = tree.equalize_plan(Some(Orientation::Vertical));

        assert!(plan.found_split);
        assert_eq!(
            plan.adjustments
                .iter()
                .map(|a| a.split_id.as_str())
                .collect::<Vec<_>>(),
            ["inner"]
        );

        let no_match =
            pane("solo", 0.0, 0.0, 100.0, 100.0).equalize_plan(Some(Orientation::Vertical));
        assert!(!no_match.found_split);
        assert!(no_match.adjustments.is_empty());
    }

    #[test]
    fn split_run_plan_rebalances_the_new_panes_row() {
        let tree = split(
            "outer",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 33.0, 100.0),
            split(
                "inner",
                Orientation::Horizontal,
                0.5,
                pane("b", 33.0, 0.0, 33.0, 100.0),
                pane("c", 67.0, 0.0, 33.0, 100.0),
            ),
        );

        let plan = tree.equalize_plan_for_split_run_containing("c");

        assert!(plan.found_split);
        assert_eq!(
            plan.adjustments
                .iter()
                .map(|a| a.split_id.as_str())
                .collect::<Vec<_>>(),
            ["inner", "outer"]
        );
        assert_eq!(plan.adjustments[0].position, 0.5);
        assert!(near(plan.adjustments[1].position, 1.0 / 3.0));
    }

    #[test]
    fn split_run_plan_leaves_other_rows_and_orientations_alone() {
        let tree = split(
            "root",
            Orientation::Vertical,
            0.3,
            split(
                "top_row",
                Orientation::Horizontal,
                0.2,
                pane("a", 0.0, 0.0, 20.0, 30.0),
                pane("b", 20.0, 0.0, 80.0, 30.0),
            ),
            split(
                "bottom_row",
                Orientation::Horizontal,
                0.5,
                pane("c", 0.0, 30.0, 50.0, 70.0),
                pane("d", 50.0, 30.0, 50.0, 70.0),
            ),
        );

        let plan = tree.equalize_plan_for_split_run_containing("d");

        assert_eq!(
            plan.adjustments
                .iter()
                .map(|a| a.split_id.as_str())
                .collect::<Vec<_>>(),
            ["bottom_row"]
        );
        assert_eq!(plan.adjustments[0].position, 0.5);
    }

    #[test]
    fn split_run_plan_is_empty_without_a_parent_split() {
        assert!(
            !pane("solo", 0.0, 0.0, 100.0, 100.0)
                .equalize_plan_for_split_run_containing("solo")
                .found_split
        );
        let tree = split(
            "s",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 50.0, 100.0),
            pane("b", 50.0, 0.0, 50.0, 100.0),
        );
        assert!(tree
            .equalize_plan_for_split_run_containing("missing")
            .adjustments
            .is_empty());
    }

    // ----- Resize planning (SplitGeometryTests) -----

    #[test]
    fn resize_moves_controlling_divider_by_pixel_delta() {
        let tree = split(
            "s",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 300.0, 400.0),
            pane("b", 300.0, 0.0, 300.0, 400.0),
        );

        // Target the second child; .left controls a divider whose target sits
        // in the second child and moves it toward the first child.
        let adjustment = tree.resize_divider_adjustment("b", ResizeDirection::Left, 60);

        assert!(adjustment.is_some());
        assert_eq!(adjustment.as_ref().unwrap().split_id, "s");
        // 60px over a 600px axis = 0.1 delta, signed negative for .left.
        assert!(near(adjustment.unwrap().position, 0.4));
    }

    #[test]
    fn resize_requires_matching_child_side() {
        let tree = split(
            "s",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 300.0, 400.0),
            pane("b", 300.0, 0.0, 300.0, 400.0),
        );

        // .right requires the target in the first child; "b" is the second.
        assert!(tree
            .resize_divider_adjustment("b", ResizeDirection::Right, 10)
            .is_none());
        // Vertical resize has no vertical split to control.
        assert!(tree
            .resize_divider_adjustment("b", ResizeDirection::Up, 10)
            .is_none());
        // Unknown pane plans nothing.
        assert!(tree
            .resize_divider_adjustment("zz", ResizeDirection::Left, 10)
            .is_none());
    }

    #[test]
    fn resize_prefers_innermost_enclosing_split() {
        let tree = split(
            "outer",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 300.0, 400.0),
            split(
                "inner",
                Orientation::Horizontal,
                0.5,
                pane("b", 300.0, 0.0, 150.0, 400.0),
                pane("c", 450.0, 0.0, 150.0, 400.0),
            ),
        );

        // "c" sits in the second child of BOTH splits; the innermost
        // (closest enclosing) candidate wins, matching the legacy order.
        let adjustment = tree.resize_divider_adjustment("c", ResizeDirection::Left, 30);
        assert!(adjustment.is_some());
        assert_eq!(adjustment.unwrap().split_id, "inner");
    }

    #[test]
    fn resize_clamps_divider_to_legacy_bounds() {
        let tree = split(
            "s",
            Orientation::Vertical,
            0.85,
            pane("a", 0.0, 0.0, 600.0, 200.0),
            pane("b", 0.0, 200.0, 600.0, 200.0),
        );

        // A huge downward move from 0.85 clamps to 0.9.
        let adjustment = tree.resize_divider_adjustment("a", ResizeDirection::Down, 400);
        assert!(adjustment.is_some());
        assert_eq!(adjustment.unwrap().position, 0.9);
    }

    // ----- Direction values (SplitGeometryTests) -----

    #[test]
    fn split_direction_maps_orientation_and_insertion_side() {
        assert!(SplitDirection::Left.is_horizontal());
        assert!(SplitDirection::Right.is_horizontal());
        assert!(!SplitDirection::Up.is_horizontal());
        assert_eq!(SplitDirection::Left.orientation(), Orientation::Horizontal);
        assert_eq!(SplitDirection::Down.orientation(), Orientation::Vertical);
        assert!(SplitDirection::Left.insert_first());
        assert!(SplitDirection::Up.insert_first());
        assert!(!SplitDirection::Right.insert_first());
        assert!(!SplitDirection::Down.insert_first());
    }

    #[test]
    fn resize_direction_maps_split_axis_and_sign() {
        assert!(ResizeDirection::Left.split_orientation().is_horizontal());
        assert!(ResizeDirection::Down.split_orientation() == Orientation::Vertical);
        assert!(ResizeDirection::Right.requires_pane_in_first_child());
        assert!(ResizeDirection::Down.requires_pane_in_first_child());
        assert!(!ResizeDirection::Left.requires_pane_in_first_child());
        assert_eq!(ResizeDirection::Right.divider_delta_sign(), 1.0);
        assert_eq!(ResizeDirection::Up.divider_delta_sign(), -1.0);
    }

    // ----- Spatial order (SpatialOrderTests) -----

    #[test]
    fn ordered_pane_ids_walks_depth_first() {
        let tree = split(
            "s1",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 50.0, 100.0),
            split(
                "s2",
                Orientation::Vertical,
                0.5,
                pane("b", 50.0, 0.0, 50.0, 50.0),
                pane("c", 50.0, 50.0, 50.0, 50.0),
            ),
        );
        assert_eq!(tree.ordered_pane_ids(), ["a", "b", "c"]);
    }

    #[test]
    fn ordered_panel_ids_uses_pane_tabs_then_fallback() {
        let tree = split(
            "s1",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 50.0, 100.0),
            pane("b", 50.0, 0.0, 50.0, 100.0),
        );
        let mut pane_tabs = HashMap::new();
        pane_tabs.insert("a".to_owned(), vec![11u64, 22]);
        pane_tabs.insert("b".to_owned(), vec![33, 11]);

        let result = tree.ordered_panel_ids(&pane_tabs, &[99, 22]);

        assert_eq!(result, [11, 22, 33, 99]);
    }

    #[test]
    fn spatial_panel_order_matches_tree_order() {
        let tree = split(
            "s1",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 50.0, 100.0),
            split(
                "s2",
                Orientation::Vertical,
                0.5,
                pane("b", 50.0, 0.0, 50.0, 50.0),
                pane("c", 50.0, 50.0, 50.0, 50.0),
            ),
        );
        let mut pane_tabs = HashMap::new();
        pane_tabs.insert("a".to_owned(), vec![11u64, 22]);
        pane_tabs.insert("b".to_owned(), vec![33, 11]);
        pane_tabs.insert("c".to_owned(), vec![]);

        let from_pane_order =
            panel_ids_in_order(&["a".into(), "b".into(), "c".into()], &pane_tabs, &[99, 22]);
        assert_eq!(from_pane_order, [11, 22, 33, 99]);
        assert_eq!(
            tree.ordered_panel_ids(&pane_tabs, &[99, 22]),
            from_pane_order
        );
    }

    // ----- Focus cycling (SpatialOrderTests/PaneCycleNavigator) -----

    #[test]
    fn pane_cycle_navigator_wraps_forward_and_backward() {
        let navigator = PaneCycleNavigator;

        assert_eq!(
            navigator.target_pane(&[1u64, 2, 3], &[1, 2, 3], Some(&1), true),
            Some(2)
        );
        assert_eq!(
            navigator.target_pane(&[1u64, 2, 3], &[1, 2, 3], Some(&3), true),
            Some(1)
        );
        assert_eq!(
            navigator.target_pane(&[1u64, 2, 3], &[1, 2, 3], Some(&1), false),
            Some(3)
        );
    }

    #[test]
    fn pane_cycle_navigator_rejects_invalid_cycle_inputs() {
        let navigator = PaneCycleNavigator;

        // Only one pane survives filtering from the ordered list.
        assert_eq!(
            navigator.target_pane(&[1u64, 2], &[1], Some(&1), true),
            None
        );
        // Stale focused pane.
        assert_eq!(
            navigator.target_pane(&[1u64, 2, 3], &[1, 2, 3], Some(&99), true),
            None
        );
        // A live pane missing from the ordered list shortens it below two.
        assert_eq!(
            navigator.target_pane(&[1u64, 99], &[1, 2], Some(&1), true),
            None
        );
        // No focused pane.
        assert_eq!(
            navigator.target_pane(&[1u64, 2, 3], &[1, 2, 3], None, true),
            None
        );
    }

    // ----- Split-space verdicts (SplitSpaceVerdictTests) -----
    // Upstream suite default: 1200-wide panes, 100 minimum, 1-thick dividers.

    fn verdict(
        tree: &TreeNode,
        pane_id: &str,
        orientation: Orientation,
        minimum: f64,
        divider_position: Option<f64>,
    ) -> SplitSpaceVerdict {
        tree.split_space_verdict(pane_id, orientation, minimum, 1.0, divider_position)
    }

    /// The layout an 860pt column reaches after four splits down: each split
    /// halved the focused (last) pane.
    fn halved_column() -> TreeNode {
        split(
            "s1",
            Orientation::Vertical,
            0.5,
            pane("a", 0.0, 0.0, 1200.0, 430.0),
            split(
                "s2",
                Orientation::Vertical,
                0.5,
                pane("b", 0.0, 430.0, 1200.0, 215.0),
                split(
                    "s3",
                    Orientation::Vertical,
                    0.5,
                    pane("c", 0.0, 645.0, 1200.0, 107.5),
                    split(
                        "s4",
                        Orientation::Vertical,
                        0.5,
                        pane("d", 0.0, 752.5, 1200.0, 53.75),
                        pane("e", 0.0, 806.25, 1200.0, 53.75),
                    ),
                ),
            ),
        )
    }

    #[test]
    fn a_pane_with_room_for_two_halves_fits() {
        assert_eq!(
            verdict(
                &pane("a", 0.0, 0.0, 1200.0, 860.0),
                "a",
                Orientation::Vertical,
                100.0,
                None
            ),
            SplitSpaceVerdict::Fits
        );
        assert_eq!(
            verdict(&halved_column(), "a", Orientation::Vertical, 100.0, None),
            SplitSpaceVerdict::Fits
        );
    }

    #[test]
    fn an_explicit_divider_position_must_leave_both_children_at_the_minimum() {
        let tree = pane("a", 0.0, 0.0, 1200.0, 300.0);
        assert_eq!(
            verdict(&tree, "a", Orientation::Vertical, 100.0, Some(0.66)),
            SplitSpaceVerdict::Fits
        );
        assert_eq!(
            verdict(&tree, "a", Orientation::Vertical, 100.0, Some(0.9)),
            SplitSpaceVerdict::NoSpace
        );
        // Requests beyond the range are clamped to 0.9 first.
        assert_eq!(
            verdict(&tree, "a", Orientation::Vertical, 100.0, Some(0.99)),
            SplitSpaceVerdict::NoSpace
        );
    }

    #[test]
    fn a_small_pane_borrows_from_its_column() {
        // Splitting e in place gives two 26pt panes; six equal slots in the
        // 860pt column give each about 143pt.
        assert_eq!(
            verdict(&halved_column(), "e", Orientation::Vertical, 100.0, None),
            SplitSpaceVerdict::FitsAfterEqualizingRun
        );
    }

    #[test]
    fn a_full_column_refuses_the_split() {
        // Three 100pt panes cannot fit in 300pt with their dividers.
        let tree = split(
            "s",
            Orientation::Vertical,
            0.5,
            pane("a", 0.0, 0.0, 1200.0, 150.0),
            pane("b", 0.0, 150.0, 1200.0, 150.0),
        );
        assert_eq!(
            verdict(&tree, "b", Orientation::Vertical, 100.0, None),
            SplitSpaceVerdict::NoSpace
        );
    }

    #[test]
    fn a_small_pane_without_a_same_orientation_run_refuses() {
        // b sits in a side-by-side split, so a split down has nothing to
        // borrow from in its column.
        let tree = split(
            "s",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 600.0, 150.0),
            pane("b", 600.0, 0.0, 600.0, 150.0),
        );
        assert_eq!(
            verdict(&tree, "b", Orientation::Vertical, 100.0, None),
            SplitSpaceVerdict::NoSpace
        );
        assert_eq!(
            verdict(
                &pane("a", 0.0, 0.0, 1200.0, 150.0),
                "a",
                Orientation::Vertical,
                100.0,
                None
            ),
            SplitSpaceVerdict::NoSpace
        );
    }

    #[test]
    fn width_uses_the_horizontal_axis() {
        let tree = split(
            "s",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 200.0, 860.0),
            pane("b", 200.0, 0.0, 200.0, 860.0),
        );
        assert_eq!(
            verdict(&tree, "b", Orientation::Horizontal, 160.0, None),
            SplitSpaceVerdict::NoSpace
        );
        assert_eq!(
            verdict(&tree, "b", Orientation::Vertical, 100.0, None),
            SplitSpaceVerdict::Fits
        );
    }

    #[test]
    fn a_stacked_slot_needs_room_for_each_of_its_panes() {
        // a | (b over c) | d side by side, 400pt wide, and d (150pt) is
        // being split right. Its halves would be 74.5pt; equal quarters
        // give each slot 99.25pt, and the stacked slot only needs one
        // pane's width, so a 90pt minimum fits after equalizing.
        let stacked = split(
            "stacked",
            Orientation::Vertical,
            0.5,
            pane("b", 125.0, 0.0, 125.0, 430.0),
            pane("c", 125.0, 430.0, 125.0, 430.0),
        );
        let tree = split(
            "outer",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 125.0, 860.0),
            split(
                "inner",
                Orientation::Horizontal,
                0.5,
                stacked,
                pane("d", 250.0, 0.0, 150.0, 860.0),
            ),
        );
        assert_eq!(
            verdict(&tree, "d", Orientation::Horizontal, 90.0, None),
            SplitSpaceVerdict::FitsAfterEqualizingRun
        );

        // A slot that nests another side-by-side split needs both widths.
        let nested = split(
            "nested",
            Orientation::Vertical,
            0.5,
            split(
                "swap",
                Orientation::Horizontal,
                0.5,
                pane("b", 125.0, 0.0, 62.5, 430.0),
                pane("x", 187.5, 0.0, 62.5, 430.0),
            ),
            pane("c", 125.0, 430.0, 125.0, 430.0),
        );
        let crowded = split(
            "outer",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 125.0, 860.0),
            split(
                "inner",
                Orientation::Horizontal,
                0.5,
                nested,
                pane("d", 250.0, 0.0, 150.0, 860.0),
            ),
        );
        assert_eq!(
            verdict(&crowded, "d", Orientation::Horizontal, 90.0, None),
            SplitSpaceVerdict::NoSpace
        );
    }

    #[test]
    fn a_nested_split_keeps_its_proportion_when_the_run_equalizes() {
        // a | S | d in 1400pt, where S stacks (b | x at 0.6) over c. Equal
        // thirds give S about 466pt, but x keeps 40% of S, so x needs S to
        // be 400pt for a 160pt minimum; a fourth slot (349pt) would leave
        // x at 140pt.
        let nested = split(
            "nested",
            Orientation::Horizontal,
            0.6,
            pane("b", 700.0, 0.0, 240.0, 430.0),
            pane("x", 940.0, 0.0, 160.0, 430.0),
        );
        let slot = split(
            "slot",
            Orientation::Vertical,
            0.5,
            nested,
            pane("c", 700.0, 430.0, 400.0, 430.0),
        );
        let tree = split(
            "outer",
            Orientation::Horizontal,
            0.5,
            pane("a", 0.0, 0.0, 700.0, 860.0),
            split(
                "inner",
                Orientation::Horizontal,
                0.5,
                slot,
                pane("d", 1100.0, 0.0, 300.0, 860.0),
            ),
        );
        assert_eq!(
            verdict(&tree, "d", Orientation::Horizontal, 160.0, None),
            SplitSpaceVerdict::NoSpace
        );
    }

    #[test]
    fn an_eleventh_exact_capacity_slot_accounts_for_ratio_clamping() {
        // Ten 100pt panes plus ten 1pt dividers would arithmetically fit an
        // eleventh pane in 1110pt. Equalizing this right-heavy tree asks its
        // root for a 1/11 ratio, which clamps to 0.1, leaving the other ten
        // slots under their exact capacity.
        let mut tree = pane("target", 1000.0, 0.0, 110.0, 860.0);
        for index in (0..=8).rev() {
            tree = split(
                &format!("p{index}"),
                Orientation::Horizontal,
                0.5,
                pane(
                    &format!("pane{index}"),
                    index as f64 * 100.0,
                    0.0,
                    100.0,
                    860.0,
                ),
                tree,
            );
        }
        assert_eq!(
            verdict(&tree, "target", Orientation::Horizontal, 100.0, None),
            SplitSpaceVerdict::NoSpace
        );
    }

    #[test]
    fn an_unmeasured_layout_allows_the_split() {
        let tree = split(
            "s",
            Orientation::Vertical,
            0.5,
            pane("a", 0.0, 0.0, 1200.0, 0.0),
            pane("b", 0.0, 0.0, 1200.0, 0.0),
        );
        assert_eq!(
            verdict(&tree, "b", Orientation::Vertical, 100.0, None),
            SplitSpaceVerdict::Fits
        );
        assert_eq!(
            verdict(&tree, "missing", Orientation::Vertical, 100.0, None),
            SplitSpaceVerdict::Fits
        );
    }
}
