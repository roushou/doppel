//! The part of the screen a veil leaves lit, as rectangles that never
//! overlap.

use doppel_desktop::geometry::Rect;

/// Where a [`Shape::Veil`](super::Shape::Veil) lets the screen through: one
/// region, kept as rectangles of which no two share any area.
///
/// So a renderer that fills the screen less each rectangle with the even-odd
/// rule leaves exactly the region lit. Elements nest (a row holds a cell,
/// which holds its text), and overlapping rectangles would let even-odd veil
/// each overlap a second time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LitRegion(Vec<Rect>);

impl LitRegion {
    /// Nothing lit: the whole screen veiled.
    #[must_use]
    pub const fn none() -> Self {
        Self(Vec::new())
    }

    /// The union of `rects`, cut into rectangles that don't overlap: one
    /// band per run of left and right edges, each band's spans merged, and a
    /// band joined to the one before it when their spans match. Rectangles
    /// without area are dropped.
    #[must_use]
    pub fn union_of(rects: impl IntoIterator<Item = Rect>) -> Self {
        let rects: Vec<Rect> = rects.into_iter().filter(|rect| rect.has_area()).collect();
        let mut edges: Vec<f64> = rects
            .iter()
            .flat_map(|rect| [rect.left(), rect.right()])
            .collect();
        edges.sort_by(f64::total_cmp);
        edges.dedup();

        let mut done = Vec::new();
        // The previous band's spans, each with where it started: (top, bottom, left).
        let mut open: Vec<(f64, f64, f64)> = Vec::new();
        let mut open_right = f64::NEG_INFINITY;
        for band in edges.windows(2) {
            let &[left, right] = band else { continue };
            let spans = spans_across(&rects, left, right);
            let joins = open_right == left;
            let mut next = Vec::with_capacity(spans.len());
            for (top, bottom) in spans {
                let started = joins
                    .then(|| {
                        open.iter()
                            .position(|&(t, b, _)| t == top && b == bottom)
                            .map(|at| open.swap_remove(at).2)
                    })
                    .flatten()
                    .unwrap_or(left);
                next.push((top, bottom, started));
            }
            done.extend(open.drain(..).filter_map(|span| closed(span, open_right)));
            open = next;
            open_right = right;
        }
        done.extend(open.drain(..).filter_map(|span| closed(span, open_right)));
        Self(done)
    }

    /// The rectangles making up the region, no two overlapping.
    pub fn rects(&self) -> impl Iterator<Item = &Rect> {
        self.0.iter()
    }
}

/// The vertical spans `rects` cover across the band from `left` to `right`,
/// overlapping ones merged, top first.
fn spans_across(rects: &[Rect], left: f64, right: f64) -> Vec<(f64, f64)> {
    let mut spans: Vec<(f64, f64)> = rects
        .iter()
        .filter(|rect| rect.left() <= left && rect.right() >= right)
        .map(|rect| (rect.top(), rect.bottom()))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::with_capacity(spans.len());
    for (top, bottom) in spans {
        match merged.last_mut() {
            Some(last) if top <= last.1 => last.1 = last.1.max(bottom),
            Some(_) | None => merged.push((top, bottom)),
        }
    }
    merged
}

fn closed((top, bottom, left): (f64, f64, f64), right: f64) -> Option<Rect> {
    Rect::new(left, top, right - left, bottom - top).ok()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect::new(x, y, width, height).unwrap()
    }

    fn area(region: &LitRegion) -> f64 {
        region
            .rects()
            .map(|rect| rect.width() * rect.height())
            .sum()
    }

    fn overlap(a: Rect, b: Rect) -> bool {
        a.intersection(b).is_some()
    }

    #[test]
    fn nested_elements_light_as_the_outer_one() {
        let row = rect(10.0, 10.0, 200.0, 40.0);
        let cell = rect(20.0, 15.0, 80.0, 30.0);
        let text = rect(25.0, 20.0, 40.0, 20.0);
        let region = LitRegion::union_of([row, cell, text]);
        assert_eq!(region.rects().copied().collect::<Vec<_>>(), vec![row]);
    }

    #[test]
    fn overlapping_elements_light_their_union_once() {
        let region = LitRegion::union_of([rect(0.0, 0.0, 10.0, 10.0), rect(5.0, 5.0, 10.0, 10.0)]);
        assert_eq!(area(&region), 175.0);
        let all: Vec<Rect> = region.rects().copied().collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(!overlap(*a, *b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn apart_elements_stay_as_they_are() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(50.0, 0.0, 10.0, 10.0);
        let region = LitRegion::union_of([a, b]);
        assert_eq!(region.rects().copied().collect::<Vec<_>>(), vec![a, b]);
    }

    #[test]
    fn nothing_without_area_is_lit() {
        assert_eq!(
            LitRegion::union_of([rect(5.0, 5.0, 0.0, 10.0)]),
            LitRegion::none()
        );
        assert_eq!(LitRegion::union_of([]), LitRegion::none());
    }

    fn rects() -> impl Strategy<Value = Vec<Rect>> {
        prop::collection::vec((0u8..40, 0u8..40, 0u8..20, 0u8..20), 0..12).prop_map(|all| {
            all.into_iter()
                .map(|(x, y, w, h)| rect(f64::from(x), f64::from(y), f64::from(w), f64::from(h)))
                .collect()
        })
    }

    proptest! {
        /// Every point the elements cover is in exactly one rectangle, and
        /// every point they don't is in none: an even-odd veil lights their union.
        #[test]
        fn rects_cover_the_union_once(input in rects()) {
            let region = LitRegion::union_of(input.clone());
            for x in 0..60 {
                for y in 0..60 {
                    let (x, y) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
                    let inside = |r: &Rect| r.left() < x && x < r.right() && r.top() < y && y < r.bottom();
                    let covered = input.iter().any(inside);
                    let count = region.rects().filter(|rect| inside(rect)).count();
                    prop_assert_eq!(count, usize::from(covered), "at ({}, {})", x, y);
                }
            }
        }
    }
}
