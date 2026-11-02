//! Sidebar interaction mathematics (owner round 9, 2026-10-27): the pure
//! pieces of the workspace strip's scroll, drag-reorder, and inline-rename
//! behavior. The planner semantics are the clean-room port of upstream's
//! `SidebarDropPlanner` (Packages/macOS/CmuxFoundation/SidebarDrop/,
//! read in full) and `SidebarInlineRenameCommit`
//! (Sources/Sidebar/SidebarInlineRenameCommit.swift); the scroll helpers
//! are the port's own extraction of the upstream ScrollView/`LazyVStack`
//! behavior. No egui, no model, no OS calls — unit tests verify each
//! rule, and `draw_sidebar` consumes them per frame.

/// Which edge of the hovered row a drop indicator anchors to, per the
/// upstream midpoint rule (`edgeForPointer`: `clampedY < targetHeight / 2
/// ? top : bottom`; exactly-half lands bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropEdge {
    Top,
    Bottom,
}

/// The upstream midpoint rule, clamped to the hovered row's band first.
pub fn drop_edge_for_pointer(pointer_y: f32, row_top: f32, row_height: f32) -> DropEdge {
    let clamped = pointer_y.clamp(row_top, row_top + row_height);
    if clamped < row_top + row_height * 0.5 {
        DropEdge::Top
    } else {
        DropEdge::Bottom
    }
}

/// The insertion SLOT (0..=count: boundary line positions) a pointer at
/// `pointer_y` implies over the row list. Rows sit at
/// `content_top + i * advance` with height `advance - gap` (the advance
/// carries the 2px inter-row gap), so the continuous position maps to the
/// nearest boundary and the exact midpoint rounds away from zero (the
/// upstream tie: exactly half = bottom edge = slot i+1).
pub fn insertion_slot(pointer_y: f32, content_top: f32, advance: f32, count: usize) -> usize {
    if count == 0 || advance <= 0.0 {
        return 0;
    }
    let position = (pointer_y - content_top) / advance;
    let slot = position.round();
    slot.clamp(0.0, count as f32) as usize
}

/// Concentrates the drag commit: removing `dragged` and inserting it at
/// `slot`, adjusting for the removal. `None` when the move is a no-op —
/// the upstream `suppressesNoOp` planner variant must not commit (a drag
/// that returns to the origin slot leaves the order untouched).
pub fn reorder_target(dragged: usize, slot: usize, count: usize) -> Option<usize> {
    if count <= 1 || dragged >= count || slot > count {
        return None;
    }
    let target = if slot > dragged { slot - 1 } else { slot };
    if target == dragged {
        None
    } else {
        Some(target)
    }
}

/// The y a 2pt drop indicator paints at for `slot` over the row list
/// (the boundary line positions `insertion_slot` returns): the top edge
/// of the row it would precede, or the bottom edge of the final row for
/// the end slot.
pub fn indicator_top(
    slot: usize,
    content_top: f32,
    advance: f32,
    count: usize,
    row_height: f32,
) -> f32 {
    if count == 0 {
        return content_top;
    }
    if slot >= count {
        content_top + (count - 1) as f32 * advance + row_height
    } else {
        content_top + slot as f32 * advance
    }
}

/// Clamps a scroll offset to the visible window: `[0, max(0, content -
/// viewport)]`. Content shorter than the viewport pins to the top (the
/// collapsed-scroll state the strip paints when every row fits).
pub fn scroll_clamp(scroll: f32, content_height: f32, viewport_height: f32) -> f32 {
    let max = (content_height - viewport_height).max(0.0);
    scroll.clamp(0.0, max)
}

/// The row indices (start, end-exclusive) whose PAINTED band
/// `[i * advance, i * advance + row_height)` intersects the visible
/// window `[scroll, scroll + viewport)` in content coordinates. Rows
/// outside return an empty slice so they are neither painted nor
/// allocated as interactions over the fixed header (a scrolled row must
/// not steal the plus row's clicks).
pub fn visible_row_range(
    scroll: f32,
    count: usize,
    advance: f32,
    row_height: f32,
    viewport: f32,
) -> (usize, usize) {
    if count == 0 || advance <= 0.0 {
        return (0, 0);
    }
    let first = ((((scroll - row_height) / advance) + 1.0)
        .floor()
        .clamp(0.0, count as f32) as usize)
        .min(count - 1);
    let raw_last = ((scroll + viewport) / advance).ceil().max(1.0) as usize;
    let last = raw_last.min(count).max(first + 1);
    (first, last)
}

/// Reveals a row in the visible window with the minimal scroll motion:
/// rows above jump so their top aligns to the window top, rows below so
/// their bottom aligns to the window bottom, visible rows keep the
/// scroll. Upstream's `shouldScrollSelectedWorkspace` policy on selection
/// change; the caller clamps the result through [`scroll_clamp`].
pub fn scroll_to_reveal(
    scroll: f32,
    row_index: usize,
    advance: f32,
    row_height: f32,
    viewport: f32,
) -> f32 {
    let top = row_index as f32 * advance;
    let bottom = top + row_height;
    if top < scroll {
        top
    } else if bottom > scroll + viewport {
        bottom - viewport
    } else {
        scroll
    }
}

/// One frame of drag autoscroll: px to ADD to the scroll offset while the
/// pointer sits inside `margin` of the region's top/bottom edge, ramping
/// linearly to `max_speed` at the edge (the upstream
/// `SidebarDragAutoScrollController` shape). Outside both margins or the
/// region vertically: zero.
pub fn drag_autoscroll(
    pointer_y: f32,
    region_top: f32,
    region_bottom: f32,
    margin: f32,
    max_speed: f32,
) -> f32 {
    if margin <= 0.0 {
        return 0.0;
    }
    let top_dist = pointer_y - region_top;
    let bottom_dist = region_bottom - pointer_y;
    if top_dist >= 0.0 && top_dist < margin {
        return -max_speed * (1.0 - top_dist / margin);
    }
    if bottom_dist >= 0.0 && bottom_dist < margin {
        return max_speed * (1.0 - bottom_dist / margin);
    }
    0.0
}

/// The rename commit policy, ported from upstream
/// (`SidebarInlineRenameCommit.titleToCommit`): whitespace trims, an
/// empty result never writes (so it can never clear a title either),
/// and retyping the current auto-derived name while no user title exists
/// commits NOTHING (auto-naming is not frozen by a look-alike input).
/// A user-custom title being reconfirmed (with optional whitespace) is a
/// real edit (" user " -> "user").
pub fn rename_commit(
    buffer: &str,
    baseline: Option<&str>,
    custom_title: Option<&str>,
) -> Option<String> {
    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        return None;
    }
    if custom_title.is_none() && Some(trimmed) == baseline {
        return None;
    }
    Some(trimmed.to_owned())
}

/// The display-name precedence shared by every consumer of a workspace's
/// row label: a user custom title wins over the live shell-derived title,
/// which wins over the positional fallback (upstream: customTitle >
/// processTitle style composition; the port's live title IS the
/// process-title equivalent, M2.4b).
pub fn preferred_name(
    custom_title: Option<&str>,
    live_title: Option<String>,
    fallback: String,
) -> String {
    if let Some(custom) = custom_title.map(str::trim).filter(|name| !name.is_empty()) {
        return custom.to_owned();
    }
    live_title.unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_edge_follows_the_upstream_midpoint_rule() {
        // Upper half -> Top.
        assert_eq!(drop_edge_for_pointer(0.0, 0.0, 30.0), DropEdge::Top);
        assert_eq!(drop_edge_for_pointer(14.9, 0.0, 30.0), DropEdge::Top);
        // Exactly-half and below -> Bottom (the documented tie).
        assert_eq!(drop_edge_for_pointer(15.0, 0.0, 30.0), DropEdge::Bottom);
        assert_eq!(drop_edge_for_pointer(29.0, 0.0, 30.0), DropEdge::Bottom);
        // Outside the row band clamps to the edge the pointer is near.
        assert_eq!(drop_edge_for_pointer(-5.0, 0.0, 30.0), DropEdge::Top);
        assert_eq!(drop_edge_for_pointer(99.0, 0.0, 30.0), DropEdge::Bottom);
    }

    #[test]
    fn insertion_slot_maps_rows_to_boundaries() {
        // Three rows on a 32 advance starting at content_top 72: over row
        // 0 (72..104) in its upper half is slot 0.
        assert_eq!(insertion_slot(60.0, 72.0, 32.0, 3), 0);
        assert_eq!(insertion_slot(80.0, 72.0, 32.0, 3), 0);
        // The exact midpoint of row 0 (88) ties to the NEXT slot: the
        // upstream rule, exactly-half = bottom edge.
        assert_eq!(insertion_slot(88.0, 72.0, 32.0, 3), 1);
        // Row 1 (104..136): top edge is slot 1, upper half slot 1.
        assert_eq!(insertion_slot(104.0, 72.0, 32.0, 3), 1);
        assert_eq!(insertion_slot(112.0, 72.0, 32.0, 3), 1);
        // Row 1's lower half rounds to its bottom edge (2); the same rule
        // over row 2.
        assert_eq!(insertion_slot(120.0, 72.0, 32.0, 3), 2);
        assert_eq!(insertion_slot(152.0, 72.0, 32.0, 3), 3);
        // Below the final row clamps to the end slot (3 = after last).
        assert_eq!(insertion_slot(400.0, 72.0, 32.0, 3), 3);
    }

    #[test]
    fn reorder_target_removes_then_inserts_and_suppresses_noops() {
        // Dragging row 1 onto the slot before row 0 -> lands at 0.
        assert_eq!(reorder_target(1, 0, 3), Some(0));
        // Dragging row 0 into the end slot 3 -> adjusted for the removal.
        assert_eq!(reorder_target(0, 3, 3), Some(2));
        // Dropping row 1 back at its own slot (or after itself) is a no-op.
        assert_eq!(reorder_target(1, 2, 3), None);
        assert_eq!(reorder_target(1, 1, 3), None);
        // Degenerate inputs never commit.
        assert_eq!(reorder_target(0, 0, 1), None);
        assert_eq!(reorder_target(2, 1, 2), None);
        assert_eq!(reorder_target(0, 5, 2), None);
        // Identity check: removing + inserting reproduces [b,d,c] from
        // [a,b,c] with dragged 1 -> slot 3.
        let mut items = vec!["a", "b", "c"];
        let target = reorder_target(1, 3, 3).unwrap();
        let moved = items.remove(1);
        items.insert(target, moved);
        assert_eq!(items, vec!["a", "c", "b"]);
    }

    #[test]
    fn indicator_top_sits_on_row_boundaries() {
        // Into the end slot: the bottom edge of the LAST row (not the
        // phantom top of a nonexistent row).
        assert_eq!(
            indicator_top(3, 72.0, 32.0, 3, 30.0),
            72.0 + 2.0 * 32.0 + 30.0
        );
        assert_eq!(indicator_top(0, 72.0, 32.0, 3, 30.0), 72.0);
        assert_eq!(indicator_top(2, 72.0, 32.0, 3, 30.0), 72.0 + 2.0 * 32.0);
    }

    #[test]
    fn scroll_clamp_pins_short_content_and_caps_long() {
        assert_eq!(scroll_clamp(9.0, 100.0, 200.0), 0.0);
        assert_eq!(scroll_clamp(-5.0, 400.0, 100.0), 0.0);
        assert_eq!(scroll_clamp(500.0, 400.0, 100.0), 300.0);
        assert_eq!(scroll_clamp(150.0, 400.0, 100.0), 150.0);
    }

    #[test]
    fn visible_row_range_matches_the_window() {
        // 3 rows, 32 advance, 30 row height: a 60 viewport shows rows 0-1
        // whole.
        assert_eq!(visible_row_range(0.0, 3, 32.0, 30.0, 60.0), (0, 2));
        // Scrolled 20 down: row 0's tail still clips into the window.
        assert_eq!(visible_row_range(20.0, 3, 32.0, 30.0, 60.0), (0, 3));
        // Scrolled past row 0's band entirely: only later rows belong.
        assert_eq!(visible_row_range(35.0, 3, 32.0, 30.0, 60.0), (1, 3));
        // Heavy scroll downdrives to the trailing rows.
        assert_eq!(visible_row_range(1000.0, 3, 32.0, 30.0, 60.0), (2, 3));
        // Empty list.
        assert_eq!(visible_row_range(0.0, 0, 32.0, 30.0, 60.0), (0, 0));
    }

    #[test]
    fn scroll_to_reveal_moves_minimally() {
        // Row 1 spans 32..62 on content; through a scroll of 10 with a
        // 200 viewport it is fully visible: no motion.
        assert_eq!(scroll_to_reveal(10.0, 1, 32.0, 30.0, 200.0), 10.0);
        // Above the window start: align the row's top.
        assert_eq!(scroll_to_reveal(72.0, 1, 32.0, 30.0, 200.0), 32.0);
        assert_eq!(scroll_to_reveal(100.0, 0, 32.0, 30.0, 200.0), 0.0);
        // Below the window end: row 3 spans 96..126; aligning its bottom
        // to the window bottom gives 126 - 64.
        assert_eq!(scroll_to_reveal(0.0, 3, 32.0, 30.0, 64.0), 62.0);
    }

    #[test]
    fn drag_autoscroll_ramps_linearly_to_the_edge() {
        // Dead center: nothing.
        assert_eq!(drag_autoscroll(150.0, 0.0, 300.0, 24.0, 12.0), 0.0);
        // Full speed AT the boundary (distance 0); the ramp is linear in
        // the distance from the edge inside the margin.
        assert_eq!(drag_autoscroll(0.0, 0.0, 300.0, 24.0, 12.0), -12.0);
        assert_eq!(drag_autoscroll(300.0, 0.0, 300.0, 24.0, 12.0), 12.0);
        let near_top = drag_autoscroll(1.0, 0.0, 300.0, 24.0, 12.0);
        assert!((near_top - -11.5).abs() < 0.001, "{near_top}");
        // Half inside the margin: half speed.
        let at = drag_autoscroll(288.0, 0.0, 300.0, 24.0, 12.0);
        assert!((at - 6.0).abs() < 0.001, "{at}");
        // Overshoot past the region: zero (pointer left while dragging).
        assert_eq!(drag_autoscroll(400.0, 0.0, 300.0, 24.0, 12.0), 0.0);
        assert_eq!(drag_autoscroll(-5.0, 0.0, 300.0, 24.0, 12.0), 0.0);
    }

    #[test]
    fn rename_commit_ports_upstreams_title_to_commit() {
        // Whitespace trims outside the value.
        assert_eq!(
            rename_commit("  docs lane  ", Some("w1"), None),
            Some("docs lane".to_owned())
        );
        // An empty result never writes (never clears a custom title).
        assert_eq!(rename_commit("   ", Some("kept"), Some("kept")), None);
        // Retyping the auto-derived name without a user title: no write.
        assert_eq!(rename_commit("powershell", Some("powershell"), None), None);
        assert_eq!(
            rename_commit(" powershell ", Some("powershell"), None),
            None
        );
        // Retyping/differing the user title is a real commit.
        assert_eq!(
            rename_commit("user", Some("user"), Some("user")),
            Some("user".to_owned())
        );
        assert_eq!(
            rename_commit("renamed", Some("auto"), None),
            Some("renamed".to_owned())
        );
        // Differing from the live title while uncustomized commits.
        assert_eq!(
            rename_commit('d'.to_string().as_str(), Some("live"), None),
            Some("d".to_owned())
        );
    }

    #[test]
    fn preferred_name_orders_custom_live_fallback() {
        assert_eq!(
            preferred_name(
                Some("  custom "),
                Some("live".to_owned()),
                "shell 1".to_owned()
            ),
            "custom"
        );
        // A whitespace-only custom title never wins (defensive: the
        // model only stores the trimmed commit, but old fixtures may not).
        assert_eq!(
            preferred_name(Some("   "), Some("live".to_owned()), "shell 1".to_owned()),
            "live"
        );
        assert_eq!(
            preferred_name(None, Some("live".to_owned()), "shell 1".to_owned()),
            "live"
        );
        assert_eq!(preferred_name(None, None, "shell 2".to_owned()), "shell 2");
    }
}
