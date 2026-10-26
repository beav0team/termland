//! Binary split tree — the Hyprland "dwindle" half of the window manager,
//! minus the animation. Every leaf is a pane; every inner node is a split.
//!
//! `Dir::Horizontal` means the children sit *side by side* (a vertical cut),
//! `Dir::Vertical` means they are *stacked* (a horizontal cut). That is the
//! flexbox reading, not the tmux one, and it is the only naming that did not
//! confuse me while writing the geometry.

use ratatui::layout::Rect;

pub type PaneId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// children left | right
    Horizontal,
    /// children top / bottom
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavDir {
    Left,
    Right,
    Up,
    Down,
}

impl NavDir {
    pub fn axis(self) -> Dir {
        match self {
            NavDir::Left | NavDir::Right => Dir::Horizontal,
            NavDir::Up | NavDir::Down => Dir::Vertical,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        dir: Dir,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// Cut `area` in two. Both halves are guaranteed non-empty when `area` is at
/// least 2 cells along the cut axis; callers should refuse to split smaller
/// panes (see `App::can_split`).
pub fn split_rect(area: Rect, dir: Dir, ratio: f32) -> (Rect, Rect) {
    let r = ratio.clamp(0.05, 0.95);
    match dir {
        Dir::Horizontal => {
            let w = area.width;
            let a = ((w as f32 * r).round() as u16).clamp(1.min(w), w.saturating_sub(1).max(1));
            (
                Rect::new(area.x, area.y, a, area.height),
                Rect::new(area.x.saturating_add(a), area.y, w.saturating_sub(a), area.height),
            )
        }
        Dir::Vertical => {
            let h = area.height;
            let a = ((h as f32 * r).round() as u16).clamp(1.min(h), h.saturating_sub(1).max(1));
            (
                Rect::new(area.x, area.y, area.width, a),
                Rect::new(area.x, area.y.saturating_add(a), area.width, h.saturating_sub(a)),
            )
        }
    }
}

impl Node {
    pub fn leaf(id: PaneId) -> Self {
        Node::Leaf(id)
    }

    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(leaf) => *leaf == id,
            Node::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// Geometry for every leaf, in reading order.
    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.walk(area, &mut out);
        out
    }

    fn walk(&self, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, area)),
            Node::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                let (a, b) = split_rect(area, *dir, *ratio);
                first.walk(a, out);
                second.walk(b, out);
            }
        }
    }

    pub fn rect_of(&self, area: Rect, id: PaneId) -> Option<Rect> {
        self.rects(area)
            .into_iter()
            .find(|(p, _)| *p == id)
            .map(|(_, r)| r)
    }

    /// Replace the leaf `target` with a split holding the old leaf and `new`.
    /// The new pane always lands in the *second* half (right / bottom).
    pub fn split_leaf(&mut self, target: PaneId, new: PaneId, dir: Dir, ratio: f32) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let old = Node::Leaf(*id);
                *self = Node::Split {
                    dir,
                    ratio,
                    first: Box::new(old),
                    second: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split_leaf(target, new, dir, ratio)
                    || second.split_leaf(target, new, dir, ratio)
            }
        }
    }

    /// Remove a leaf and collapse its parent into the surviving sibling.
    /// Returns false for the root-level single leaf, which the caller has to
    /// turn into an empty workspace itself.
    pub fn remove_leaf(&mut self, target: PaneId) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                if let Node::Leaf(id) = **first {
                    if id == target {
                        *self = (**second).clone();
                        return true;
                    }
                }
                if let Node::Leaf(id) = **second {
                    if id == target {
                        *self = (**first).clone();
                        return true;
                    }
                }
                first.remove_leaf(target) || second.remove_leaf(target)
            }
        }
    }

    /// Exchange two leaves in place. Used by `Super+Shift+h/j/k/l`, which
    /// moves a window the way Hyprland does: the pane keeps its geometry slot
    /// and the two exchange slots.
    pub fn swap_leaves(&mut self, a: PaneId, b: PaneId) -> bool {
        let (mut found_a, mut found_b) = (false, false);
        self.swap_walk(a, b, &mut found_a, &mut found_b);
        found_a && found_b
    }

    fn swap_walk(&mut self, a: PaneId, b: PaneId, found_a: &mut bool, found_b: &mut bool) {
        match self {
            Node::Leaf(id) => {
                if *id == a {
                    *id = b;
                    *found_a = true;
                } else if *id == b {
                    *id = a;
                    *found_b = true;
                }
            }
            Node::Split { first, second, .. } => {
                first.swap_walk(a, b, found_a, found_b);
                second.swap_walk(a, b, found_a, found_b);
            }
        }
    }

    pub fn replace_leaf(&mut self, old: PaneId, new: PaneId) -> bool {
        match self {
            Node::Leaf(id) if *id == old => {
                *id = new;
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.replace_leaf(old, new) || second.replace_leaf(old, new)
            }
        }
    }

    /// Grow/shrink the innermost split of the matching axis that contains `id`.
    pub fn resize_along(&mut self, id: PaneId, dir: Dir, delta: f32) -> bool {
        let Node::Split {
            dir: d,
            ratio,
            first,
            second,
        } = self
        else {
            return false;
        };
        if *d != dir || !(first.contains(id) || second.contains(id)) {
            return false;
        }
        // Deepest matching split wins, so resizing acts on the divider you see.
        if first.contains(id) && first.resize_along(id, dir, delta) {
            return true;
        }
        if second.contains(id) && second.resize_along(id, dir, delta) {
            return true;
        }
        *ratio = (*ratio + delta).clamp(0.1, 0.9);
        true
    }

    /// Flip a leaf's parent between side-by-side and stacked.
    pub fn toggle_dir_of(&mut self, id: PaneId) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split {
                dir, first, second, ..
            } => {
                if first.contains(id) || second.contains(id) {
                    if first.toggle_dir_of(id) {
                        return true;
                    }
                    if second.toggle_dir_of(id) {
                        return true;
                    }
                    *dir = match dir {
                        Dir::Horizontal => Dir::Vertical,
                        Dir::Vertical => Dir::Horizontal,
                    };
                    return true;
                }
                false
            }
        }
    }

    /// Find the divider under screen coordinates `(x, y)` relative to `area`.
    /// Returns the split's direction and current ratio if the point sits on a
    /// divider line between two children.
    pub fn divider_at_point(&self, area: Rect, x: u16, y: u16) -> Option<(Dir, f32)> {
        match self {
            Node::Leaf(_) => None,
            Node::Split {
                dir, ratio, first, second,
            } => {
                let (a_rect, b_rect) = split_rect(area, *dir, *ratio);
                match dir {
                    Dir::Horizontal => {
                        let div_x = a_rect.x.saturating_add(a_rect.width);
                        if x == div_x && y >= a_rect.y && y < a_rect.y.saturating_add(a_rect.height)
                        {
                            return Some((*dir, *ratio));
                        }
                    }
                    Dir::Vertical => {
                        let div_y = a_rect.y.saturating_add(a_rect.height);
                        if y == div_y && x >= a_rect.x && x < a_rect.x.saturating_add(a_rect.width)
                        {
                            return Some((*dir, *ratio));
                        }
                    }
                }
                first
                    .divider_at_point(a_rect, x, y)
                    .or_else(|| second.divider_at_point(b_rect, x, y))
            }
        }
    }

    /// Adjust the ratio of the split whose divider is under `(x, y)`, based on
    /// the mouse position within `area`. Used during drag-to-resize.
    pub fn resize_divider_at(&mut self, area: Rect, x: u16, y: u16) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split {
                dir, ratio, first, second,
            } => {
                let (a_rect, b_rect) = split_rect(area, *dir, *ratio);
                let on_divider = match dir {
                    Dir::Horizontal => {
                        let div_x = a_rect.x.saturating_add(a_rect.width);
                        (x == div_x || x == div_x.saturating_sub(1))
                            && y >= a_rect.y
                            && y < a_rect.y.saturating_add(a_rect.height)
                    }
                    Dir::Vertical => {
                        let div_y = a_rect.y.saturating_add(a_rect.height);
                        (y == div_y || y == div_y.saturating_sub(1))
                            && x >= a_rect.x
                            && x < a_rect.x.saturating_add(a_rect.width)
                    }
                };
                if on_divider {
                    let new_ratio = match dir {
                        Dir::Horizontal => (x as f32 - area.x as f32) / area.width as f32,
                        Dir::Vertical => (y as f32 - area.y as f32) / area.height as f32,
                    };
                    *ratio = new_ratio.clamp(0.1, 0.9);
                    return true;
                }
                first.resize_divider_at(a_rect, x, y)
                    || second.resize_divider_at(b_rect, x, y)
            }
        }
    }

    /// Geometric directional focus, the way every tiler does it: among panes on
    /// the correct side, prefer ones that actually sit *in front of* the origin
    /// (perpendicular overlap), then the closest along the travel axis, then the
    /// smallest perpendicular offset. Skipping the overlap test is what makes
    /// "up" jump diagonally across the window instead of moving one pane over.
    pub fn neighbor(&self, area: Rect, from: PaneId, dir: NavDir) -> Option<PaneId> {
        let rects = self.rects(area);
        let origin = rects.iter().find(|(p, _)| *p == from).map(|(_, r)| *r)?;
        // Doubled coordinates keep rect centres on integers.
        let ox = origin.x as i32 * 2 + origin.width as i32;
        let oy = origin.y as i32 * 2 + origin.height as i32;

        let mut best: Option<((i32, i32, i32), PaneId)> = None;
        for (id, r) in &rects {
            if *id == from || r.width == 0 || r.height == 0 {
                continue;
            }
            let cx = r.x as i32 * 2 + r.width as i32;
            let cy = r.y as i32 * 2 + r.height as i32;
            // Edge-based side test: the candidate must be entirely on the requested
            // side of the origin (not just have its centre there). A full-height
            // pane has nothing above or below it; a full-width pane has nothing
            // left or right of it.
            let (along, across, on_side) = match dir {
                NavDir::Right => (
                    cx - ox,
                    (cy - oy).abs(),
                    r.x as i32 >= origin.x as i32 + origin.width as i32,
                ),
                NavDir::Left => (
                    ox - cx,
                    (cy - oy).abs(),
                    r.x as i32 + r.width as i32 <= origin.x as i32,
                ),
                NavDir::Down => (
                    cy - oy,
                    (cx - ox).abs(),
                    r.y as i32 >= origin.y as i32 + origin.height as i32,
                ),
                NavDir::Up => (
                    oy - cy,
                    (cx - ox).abs(),
                    r.y as i32 + r.height as i32 <= origin.y as i32,
                ),
            };
            if !on_side || along <= 0 {
                continue;
            }
            let overlaps = match dir {
                NavDir::Left | NavDir::Right => {
                    ranges_overlap(origin.y, origin.height, r.y, r.height)
                }
                NavDir::Up | NavDir::Down => {
                    ranges_overlap(origin.x, origin.width, r.x, r.width)
                }
            };
            let key = (if overlaps { 0 } else { 1 }, along, across);
            let better = match best {
                None => true,
                Some((bk, _)) => key < bk,
            };
            if better {
                best = Some((key, *id));
            }
        }
        best.map(|(_, id)| id)
    }
}

fn ranges_overlap(a0: u16, alen: u16, b0: u16, blen: u16) -> bool {
    (a0 as u32) < (b0 as u32 + blen as u32) && (b0 as u32) < (a0 as u32 + alen as u32)
}

/// Sensible split axis for a pane: cut along its long side, like Hyprland's
/// `split_ratio`-aware dwindle default.
pub fn auto_dir(area: Rect) -> Dir {
    if area.width >= area.height.saturating_mul(2) {
        Dir::Horizontal
    } else if area.height >= area.width.saturating_mul(2) {
        Dir::Vertical
    } else if area.width >= area.height {
        Dir::Horizontal
    } else {
        Dir::Vertical
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[test]
    fn split_rect_halves_add_up() {
        let (a, b) = split_rect(area(80, 24), Dir::Horizontal, 0.5);
        assert_eq!(a.width + b.width, 80);
        assert_eq!(a.height, 24);
        assert_eq!(b.x, a.width);

        let (a, b) = split_rect(area(80, 24), Dir::Vertical, 0.5);
        assert_eq!(a.height + b.height, 24);
        assert_eq!(b.y, a.height);
    }

    #[test]
    fn split_rect_never_returns_a_zero_half_when_it_can_help() {
        for w in 2..8u16 {
            let (a, b) = split_rect(area(w, 3), Dir::Horizontal, 0.5);
            assert_eq!(a.width + b.width, w);
            assert!(a.width >= 1 && b.width >= 1, "width {w} gave an empty half");
        }
    }

    #[test]
    fn a_one_cell_pane_cannot_be_split_in_two() {
        // One column is not divisible; the arithmetic still has to add up, and
        // it is the caller's job to refuse (App::split requires >= 6 columns).
        let (a, b) = split_rect(area(1, 3), Dir::Horizontal, 0.5);
        assert_eq!(a.width + b.width, 1);
        assert_eq!(b.width, 0);
    }

    #[test]
    fn split_then_remove_collapses_cleanly() {
        let mut n = Node::leaf(1);
        assert!(n.split_leaf(1, 2, Dir::Horizontal, 0.5));
        assert_eq!(n.leaves(), vec![1, 2]);
        assert!(n.split_leaf(2, 3, Dir::Vertical, 0.5));
        assert_eq!(n.leaves(), vec![1, 2, 3]);
        assert!(n.remove_leaf(2));
        assert_eq!(n.leaves(), vec![1, 3], "2's slot must be gone, 3 promoted");
        assert!(n.remove_leaf(3));
        assert_eq!(n.leaves(), vec![1]);
        assert!(!n.remove_leaf(1), "last leaf cannot be removed");
    }

    #[test]
    fn rects_tile_the_window_without_overlap() {
        let mut n = Node::leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal, 0.5);
        n.split_leaf(2, 3, Dir::Vertical, 0.5);
        let rects = n.rects(area(100, 40));
        let total: u32 = rects.iter().map(|(_, r)| r.width as u32 * r.height as u32).sum();
        assert_eq!(total, 100 * 40);
    }

    #[test]
    fn neighbor_stays_on_the_requested_side() {
        let mut n = Node::leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal, 0.5); // 1 left | 2 right
        n.split_leaf(2, 3, Dir::Vertical, 0.5); // 2 top-right, 3 bottom-right
        let a = area(100, 40);
        // From pane 1 both right-hand panes are exactly equidistant, so the
        // tie-break is reading order. Deterministic beats clever.
        assert_eq!(n.neighbor(a, 1, NavDir::Right), Some(2));
        assert_eq!(n.neighbor(a, 3, NavDir::Up), Some(2));
        assert_eq!(n.neighbor(a, 3, NavDir::Left), Some(1));
        assert_eq!(n.neighbor(a, 1, NavDir::Left), None);
        assert_eq!(n.neighbor(a, 1, NavDir::Up), None);
    }

    #[test]
    fn neighbor_prefers_the_nearer_pane() {
        let mut n = Node::leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal, 0.33);
        n.split_leaf(2, 3, Dir::Horizontal, 0.5);
        let a = area(120, 20);
        assert_eq!(
            n.neighbor(a, 1, NavDir::Right),
            Some(2),
            "must not leapfrog the adjacent pane to reach 3"
        );
        assert_eq!(n.neighbor(a, 3, NavDir::Left), Some(2));
    }

    #[test]
    fn resize_along_moves_the_inner_divider_only() {
        let mut n = Node::leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal, 0.5);
        n.split_leaf(1, 3, Dir::Horizontal, 0.5);
        assert!(n.resize_along(3, Dir::Horizontal, 0.1));
        let rects = n.rects(area(100, 10));
        let w3 = rects.iter().find(|(p, _)| *p == 3).unwrap().1.width;
        assert!(w3 < 30, "pane 3 should have shrunk, got {w3}");
        let w2 = rects.iter().find(|(p, _)| *p == 2).unwrap().1.width;
        assert_eq!(w2, 50, "the outer divider must not move");
    }

    #[test]
    fn toggle_dir_flips_the_parent_split() {
        let mut n = Node::leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal, 0.5);
        assert!(n.toggle_dir_of(2));
        match n {
            Node::Split { dir, .. } => assert_eq!(dir, Dir::Vertical),
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn replace_leaf_keeps_position() {
        let mut n = Node::leaf(1);
        n.split_leaf(1, 2, Dir::Horizontal, 0.5);
        assert!(n.replace_leaf(2, 99));
        assert_eq!(n.leaves(), vec![1, 99]);
    }

    #[test]
    fn auto_dir_cuts_the_long_side() {
        assert_eq!(auto_dir(area(200, 40)), Dir::Horizontal);
        assert_eq!(auto_dir(area(40, 200)), Dir::Vertical);
    }
}
