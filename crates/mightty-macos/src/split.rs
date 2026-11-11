//! How a window's panes share it: a tree of splits, each dividing its
//! space between two parts, side by side or stacked, down to the panes.
//! Rectangles are in AppKit's coordinates: points, origin bottom left.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Side by side, the first part on the left.
    Row,
    /// Stacked, the first part on top.
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }
}

#[derive(Debug)]
pub enum Node<T> {
    Leaf(T),
    Split { axis: Axis, ratio: f64, first: Box<Node<T>>, second: Box<Node<T>> },
}

/// Where a pane sat, to put it back after it's removed: beside the part
/// holding `sibling`'s panes, split as it was.
#[derive(Clone, Debug, PartialEq)]
pub struct Place<T> {
    pub sibling: Vec<T>,
    pub axis: Axis,
    pub ratio: f64,
    /// It was the split's first part: left or above.
    pub first: bool,
}

/// The line between a split's two parts.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    /// From the root to the split: `false` for a first part, `true` for a second.
    pub path: Vec<bool>,
    pub axis: Axis,
    pub rect: Rect,
    /// What the split divides.
    pub span: Rect,
}

impl<T: Copy + PartialEq> Node<T> {
    /// The panes, left to right and top to bottom.
    pub fn leaves(&self) -> Vec<T> {
        let mut out = Vec::new();
        self.walk(Rect::default(), 0.0, &mut Vec::new(), &mut |leaf, _| out.push(leaf), &mut |_| {});
        out
    }

    /// Divides `at`'s space with `new`, which goes right of or below it.
    pub fn split(&mut self, at: T, new: T, axis: Axis) -> bool {
        match self {
            Node::Leaf(leaf) if *leaf == at => {
                *self = Node::Split { axis, ratio: 0.5, first: Box::new(Node::Leaf(at)), second: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => first.split(at, new, axis) || second.split(at, new, axis),
        }
    }

    /// Takes `leaf` out, its sibling taking the space. The last pane can't
    /// be removed: that's closing the window.
    pub fn remove(&mut self, leaf: T) -> bool {
        let Node::Split { first, second, .. } = self else { return false };
        let keep = match (&**first, &**second) {
            (Node::Leaf(l), _) if *l == leaf => second,
            (_, Node::Leaf(l)) if *l == leaf => first,
            _ => return first.remove(leaf) || second.remove(leaf),
        };
        let keep = std::mem::replace(&mut **keep, Node::Leaf(leaf));
        *self = keep;
        true
    }

    /// Where `leaf` sits, if not alone.
    pub fn place_of(&self, leaf: T) -> Option<Place<T>> {
        let Node::Split { axis, ratio, first, second } = self else { return None };
        let place = |sibling: &Node<T>, first| Place { sibling: sibling.leaves(), axis: *axis, ratio: *ratio, first };
        match (&**first, &**second) {
            (Node::Leaf(l), _) if *l == leaf => Some(place(second, true)),
            (_, Node::Leaf(l)) if *l == leaf => Some(place(first, false)),
            _ => first.place_of(leaf).or_else(|| second.place_of(leaf)),
        }
    }

    /// Puts `leaf` back at `place`: beside the part holding just the
    /// sibling's panes still here, or, the layout having changed since,
    /// beside the first of them. False if none is left.
    pub fn put_back(&mut self, leaf: T, place: &Place<T>) -> bool {
        let here = self.leaves();
        let wanted: Vec<T> = place.sibling.iter().copied().filter(|t| here.contains(t)).collect();
        let Some(&anchor) = wanted.first() else { return false };
        let target = self.find(&wanted).is_some();
        let node = if target { self.find(&wanted) } else { self.find(&[anchor]) };
        let Some(node) = node else { return false };
        let sibling = std::mem::replace(node, Node::Leaf(leaf));
        let (a, b) = if place.first { (Node::Leaf(leaf), sibling) } else { (sibling, Node::Leaf(leaf)) };
        *node = Node::Split { axis: place.axis, ratio: place.ratio, first: Box::new(a), second: Box::new(b) };
        true
    }

    /// The part holding exactly `leaves`, in order.
    fn find(&mut self, leaves: &[T]) -> Option<&mut Node<T>> {
        if self.leaves() == leaves {
            return Some(self);
        }
        match self {
            Node::Leaf(_) => None,
            Node::Split { first, second, .. } => first.find(leaves).or_else(move || second.find(leaves)),
        }
    }

    /// Where each pane goes in `bounds`, with `gap` points between them.
    pub fn frames(&self, bounds: Rect, gap: f64) -> Vec<(T, Rect)> {
        let mut out = Vec::new();
        self.walk(bounds, gap, &mut Vec::new(), &mut |leaf, rect| out.push((leaf, rect)), &mut |_| {});
        out
    }

    pub fn dividers(&self, bounds: Rect, gap: f64) -> Vec<Divider> {
        let mut out = Vec::new();
        self.walk(bounds, gap, &mut Vec::new(), &mut |_, _| {}, &mut |d| out.push(d));
        out
    }

    fn walk(&self, rect: Rect, gap: f64, path: &mut Vec<bool>, leaf: &mut impl FnMut(T, Rect), divider: &mut impl FnMut(Divider)) {
        let Node::Split { axis, ratio, first, second } = self else {
            if let Node::Leaf(l) = self {
                leaf(*l, rect);
            }
            return;
        };
        let (a, line, b) = divide(rect, *axis, *ratio, gap);
        divider(Divider { path: path.clone(), axis: *axis, rect: line, span: rect });
        path.push(false);
        first.walk(a, gap, path, leaf, divider);
        *path.last_mut().unwrap() = true;
        second.walk(b, gap, path, leaf, divider);
        path.pop();
    }

    /// Sets the ratio of the split at `path` (see [`Divider::path`]).
    pub fn set_ratio(&mut self, path: &[bool], to: f64) {
        match (self, path.split_first()) {
            (Node::Split { ratio, .. }, None) => *ratio = to,
            (Node::Split { first, second, .. }, Some((&side, rest))) => {
                if side { second } else { first }.set_ratio(rest, to);
            }
            (Node::Leaf(_), _) => {}
        }
    }

    /// Moves the divider of the split nearest `leaf` along `axis` by
    /// `points` (toward the right or bottom when positive), keeping each
    /// part at least `min` points. Returns whether it moved.
    pub fn move_divider(&mut self, leaf: T, axis: Axis, points: f64, bounds: Rect, gap: f64, min: f64) -> bool {
        let Some(path) = self.ancestor(leaf, axis) else { return false };
        let Some(divider) = self.dividers(bounds, gap).into_iter().find(|d| d.path == path) else { return false };
        let available = match axis {
            Axis::Row => divider.span.width,
            Axis::Column => divider.span.height,
        } - gap;
        let Some(current) = self.ratio(&path) else { return false };
        let (low, high) = (min / available, 1.0 - min / available);
        if available <= 0.0 || low > high {
            return false;
        }
        let ratio = (current + points / available).clamp(low, high);
        self.set_ratio(&path, ratio);
        ratio != current
    }

    /// The path to the innermost split along `axis` that holds `leaf`.
    fn ancestor(&self, leaf: T, axis: Axis) -> Option<Vec<bool>> {
        let Node::Split { axis: own, first, second, .. } = self else { return None };
        for (side, child) in [(false, first), (true, second)] {
            if child.leaves().contains(&leaf) {
                let inner = child.ancestor(leaf, axis).map(|mut rest| {
                    rest.insert(0, side);
                    rest
                });
                return inner.or_else(|| (*own == axis).then(Vec::new));
            }
        }
        None
    }

    fn ratio(&self, path: &[bool]) -> Option<f64> {
        match (self, path.split_first()) {
            (Node::Split { ratio, .. }, None) => Some(*ratio),
            (Node::Split { first, second, .. }, Some((&side, rest))) => if side { second } else { first }.ratio(rest),
            (Node::Leaf(_), _) => None,
        }
    }

    /// Sizes every pane alike along each direction.
    pub fn equalize(&mut self) {
        if let Node::Split { axis, ratio, first, second } = self {
            first.equalize();
            second.equalize();
            let (a, b) = (first.weight(*axis), second.weight(*axis));
            *ratio = a / (a + b);
        }
    }

    /// How many panes share this node's space along `axis`.
    fn weight(&self, along: Axis) -> f64 {
        match self {
            Node::Leaf(_) => 1.0,
            Node::Split { axis, first, second, .. } if *axis == along => first.weight(along) + second.weight(along),
            Node::Split { first, second, .. } => first.weight(along).max(second.weight(along)),
        }
    }
}

/// `rect` split along `axis` at `ratio`: the first part, the gap between,
/// the second part. Edges land on whole points.
fn divide(rect: Rect, axis: Axis, ratio: f64, gap: f64) -> (Rect, Rect, Rect) {
    match axis {
        Axis::Row => {
            let first = ((rect.width - gap) * ratio).round();
            let a = Rect { width: first, ..rect };
            let line = Rect { x: rect.x + first, width: gap, ..rect };
            let b = Rect { x: rect.x + first + gap, width: rect.width - first - gap, ..rect };
            (a, line, b)
        }
        Axis::Column => {
            let first = ((rect.height - gap) * ratio).round();
            let a = Rect { y: rect.y + rect.height - first, height: first, ..rect };
            let line = Rect { y: rect.y + rect.height - first - gap, height: gap, ..rect };
            let b = Rect { height: rect.height - first - gap, ..rect };
            (a, line, b)
        }
    }
}

/// The ratio that puts `divider` under the point (`x`, `y`), keeping each
/// part at least `min` points.
pub fn ratio_at(divider: &Divider, x: f64, y: f64, gap: f64, min: f64) -> f64 {
    let span = divider.span;
    let (along, length) = match divider.axis {
        Axis::Row => (x - span.x, span.width),
        Axis::Column => (span.y + span.height - y, span.height),
    };
    let available = length - gap;
    let (low, high) = (min / available, 1.0 - min / available);
    if available <= 0.0 || low > high { 0.5 } else { ((along - gap / 2.0) / available).clamp(low, high) }
}

/// The pane next to `from` in `direction`: the nearest one beyond its
/// edge that lies alongside it, the one sharing the most edge if several do.
pub fn neighbor<T: Copy + PartialEq>(frames: &[(T, Rect)], from: T, direction: Direction) -> Option<T> {
    let &(_, f) = frames.iter().find(|(leaf, _)| *leaf == from)?;
    let overlap = |a0: f64, a1: f64, b0: f64, b1: f64| a1.min(b1) - a0.max(b0);
    frames
        .iter()
        .filter(|(leaf, _)| *leaf != from)
        .filter_map(|&(leaf, r)| {
            let (distance, shared) = match direction {
                Direction::Left => (f.x - (r.x + r.width), overlap(f.y, f.y + f.height, r.y, r.y + r.height)),
                Direction::Right => (r.x - (f.x + f.width), overlap(f.y, f.y + f.height, r.y, r.y + r.height)),
                Direction::Up => (r.y - (f.y + f.height), overlap(f.x, f.x + f.width, r.x, r.x + r.width)),
                Direction::Down => (f.y - (r.y + r.height), overlap(f.x, f.x + f.width, r.x, r.x + r.width)),
            };
            (distance >= -0.5 && shared > 0.0).then_some((leaf, distance, shared))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1).then(b.2.total_cmp(&a.2)))
        .map(|(leaf, ..)| leaf)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Rect = Rect { x: 0.0, y: 0.0, width: 801.0, height: 601.0 };

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect { x, y, width, height }
    }

    #[test]
    fn panes_go_back_where_they_were() {
        // 1 | (2 / 3)
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, Axis::Row);
        tree.split(2, 3, Axis::Column);
        let place = tree.place_of(1).unwrap();
        assert_eq!(place, Place { sibling: vec![2, 3], axis: Axis::Row, ratio: 0.5, first: true });
        assert!(tree.remove(1));
        assert!(tree.put_back(1, &place));
        assert_eq!(tree.leaves(), [1, 2, 3]);
        // 3 below 2, as it was.
        let place = tree.place_of(3).unwrap();
        tree.remove(3);
        tree.put_back(3, &place);
        assert_eq!(tree.leaves(), [1, 2, 3]);
        assert!(tree.place_of(3).is_some_and(|p| p.axis == Axis::Column && !p.first && p.sibling == [2]));
        // Its sibling split since: beside the first of its panes.
        let place = tree.place_of(1).unwrap();
        tree.remove(1);
        tree.split(3, 4, Axis::Row);
        assert!(tree.put_back(1, &Place { sibling: vec![2, 9], ..place.clone() }));
        assert_eq!(tree.leaves(), [1, 2, 3, 4]);
        // Nothing of it left: not put back.
        assert!(!tree.put_back(5, &Place { sibling: vec![9], ..place }));
        assert_eq!(Node::Leaf(1).place_of(1), None);
    }

    #[test]
    fn splits_divide_space() {
        let mut tree = Node::Leaf(1);
        assert!(tree.split(1, 2, Axis::Row));
        assert!(tree.split(2, 3, Axis::Column));
        assert!(!tree.split(9, 4, Axis::Row));
        assert_eq!(tree.leaves(), [1, 2, 3]);
        // 1 on the left; 2 above 3 on the right, a 1-point gap between each.
        assert_eq!(tree.frames(BOUNDS, 1.0), [
            (1, rect(0.0, 0.0, 400.0, 601.0)),
            (2, rect(401.0, 301.0, 400.0, 300.0)),
            (3, rect(401.0, 0.0, 400.0, 300.0)),
        ]);
        let dividers = tree.dividers(BOUNDS, 1.0);
        assert_eq!(dividers.len(), 2);
        assert_eq!((dividers[0].path.as_slice(), dividers[0].rect), (&[][..], rect(400.0, 0.0, 1.0, 601.0)));
        assert_eq!((dividers[1].path.as_slice(), dividers[1].rect), (&[true][..], rect(401.0, 300.0, 400.0, 1.0)));
    }

    #[test]
    fn removing_gives_the_space_back() {
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, Axis::Row);
        tree.split(2, 3, Axis::Column);
        assert!(tree.remove(2));
        assert_eq!(tree.frames(BOUNDS, 1.0), [(1, rect(0.0, 0.0, 400.0, 601.0)), (3, rect(401.0, 0.0, 400.0, 601.0))]);
        assert!(tree.remove(1));
        assert_eq!(tree.leaves(), [3]);
        // The last one stays: closing it closes the window.
        assert!(!tree.remove(3));
        assert!(!tree.remove(7));
    }

    #[test]
    fn dragging_and_equalizing() {
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, Axis::Row);
        tree.split(2, 3, Axis::Row);
        let divider = &tree.dividers(BOUNDS, 1.0)[0];
        assert_eq!(ratio_at(divider, 200.5, 0.0, 1.0, 50.0), 0.25);
        // Never smaller than the minimum.
        assert_eq!(ratio_at(divider, 0.0, 0.0, 1.0, 80.0), 0.1);
        tree.set_ratio(&divider.path.clone(), 0.25);
        assert_eq!(tree.frames(BOUNDS, 1.0)[0].1.width, 200.0);
        // Three side by side get a third each.
        tree.equalize();
        let widths: Vec<_> = tree.frames(BOUNDS, 1.0).iter().map(|f| f.1.width).collect();
        assert_eq!(widths, [267.0, 266.0, 266.0]);
        // Stacked: dragging measures from the top.
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, Axis::Column);
        let divider = &tree.dividers(BOUNDS, 1.0)[0];
        assert_eq!(ratio_at(divider, 0.0, 450.5, 1.0, 50.0), 0.25);
    }

    #[test]
    fn dividers_move_by_keyboard() {
        // 1 | (2 / 3)
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, Axis::Row);
        tree.split(2, 3, Axis::Column);
        let width = |t: &Node<i32>, leaf| t.frames(BOUNDS, 1.0).into_iter().find(|f| f.0 == leaf).unwrap().1;
        // From 3, right/left moves the outer divider; down moves the one between 2 and 3.
        assert!(tree.move_divider(3, Axis::Row, 10.0, BOUNDS, 1.0, 50.0));
        assert_eq!(width(&tree, 1).width, 410.0);
        assert!(tree.move_divider(3, Axis::Column, 20.0, BOUNDS, 1.0, 50.0));
        assert_eq!((width(&tree, 2).height, width(&tree, 3).height), (320.0, 280.0));
        // Pane 1 has no split above or below it.
        assert!(!tree.move_divider(1, Axis::Column, 10.0, BOUNDS, 1.0, 50.0));
        // Never past the minimum.
        assert!(tree.move_divider(1, Axis::Row, -1000.0, BOUNDS, 1.0, 50.0));
        assert_eq!(width(&tree, 1).width, 50.0);
        assert!(!tree.move_divider(1, Axis::Row, -10.0, BOUNDS, 1.0, 50.0));
    }

    #[test]
    fn neighbors_by_position() {
        // 1 | 2 / 3, 4 below everything.
        let mut tree = Node::Leaf(1);
        tree.split(1, 4, Axis::Column);
        tree.split(1, 2, Axis::Row);
        tree.split(2, 3, Axis::Column);
        let frames = tree.frames(BOUNDS, 1.0);
        assert_eq!(neighbor(&frames, 1, Direction::Right), Some(2));
        assert_eq!(neighbor(&frames, 3, Direction::Left), Some(1));
        assert_eq!(neighbor(&frames, 3, Direction::Up), Some(2));
        assert_eq!(neighbor(&frames, 3, Direction::Down), Some(4));
        assert_eq!(neighbor(&frames, 4, Direction::Up), Some(1));
        assert_eq!(neighbor(&frames, 1, Direction::Left), None);
    }
}
