//! Where names go beside their symbols on the map, as cartographers place
//! point labels: each name has a few slots around its symbol, in order of
//! preference, and takes the first one clear of everything already on
//! the map (symbols, notice dots, names placed before it). A name keeps
//! last frame's slot while it stays clear, so names don't flip as boats
//! move; and one away from its first choice gets a leader line back to
//! its symbol, so it's clear whose name it is.
//!
//! Notice dots are placed first (`place_badges`), each overlapping its own
//! symbol like a badge, and on the side of it with the most room, so a
//! dot between two boats doesn't seem to belong to either.

use std::collections::HashMap;

/// A box on screen (CSS px), `x0` left of `x1` and `y0` above `y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn at(x0: f64, y0: f64, w: f64, h: f64) -> Self {
        Self {
            x0,
            y0,
            x1: x0 + w,
            y1: y0 + h,
        }
    }

    /// The box `half` either side of `center`.
    pub fn around(center: (f64, f64), half: (f64, f64)) -> Self {
        Self::at(
            center.0 - half.0,
            center.1 - half.1,
            2.0 * half.0,
            2.0 * half.1,
        )
    }

    /// The smallest box holding `points`, grown by `pad`.
    pub fn bounding(points: &[(f64, f64)], pad: f64) -> Self {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, y) in points {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        Self {
            x0: x0 - pad,
            y0: y0 - pad,
            x1: x1 + pad,
            y1: y1 + pad,
        }
    }

    pub fn center(&self) -> (f64, f64) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }

    fn grow(&self, by: f64) -> Self {
        Self {
            x0: self.x0 - by,
            y0: self.y0 - by,
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
    }

    /// The area (px²) this box shares with `other`.
    fn overlap(&self, other: &Rect) -> f64 {
        let w = self.x1.min(other.x1) - self.x0.max(other.x0);
        let h = self.y1.min(other.y1) - self.y0.max(other.y0);
        w.max(0.0) * h.max(0.0)
    }

    /// The point in this box nearest `p`.
    fn nearest(&self, p: (f64, f64)) -> (f64, f64) {
        (p.0.clamp(self.x0, self.x1), p.1.clamp(self.y0, self.y1))
    }

    /// How far (px) this box is from `other`, 0 if they touch.
    fn gap(&self, other: &Rect) -> f64 {
        let dx = (other.x0 - self.x1).max(self.x0 - other.x1).max(0.0);
        let dy = (other.y0 - self.y1).max(self.y0 - other.y1).max(0.0);
        dx.hypot(dy)
    }
}

/// Which side of its symbol a name would rather be on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Right,
    Left,
}

/// A name to place: its size, its symbol's box, and its preferred side.
pub struct Wanted<'a> {
    pub key: &'a str,
    pub size: (f64, f64),
    pub symbol: Rect,
    pub prefer: Side,
}

/// Where a name went: its box, which of its slots that is (0 its first
/// choice), and, away from its first choice, a leader line from the name
/// to its symbol.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub rect: Rect,
    pub slot: usize,
    pub leader: Option<[(f64, f64); 2]>,
}

/// Names keep this far (px) from anything else.
const PAD_PX: f64 = 1.0;
/// A name off its first choice goes back only once that's clear by this
/// much more (px), so it doesn't flip back and forth at the edge.
const RETURN_PX: f64 = 4.0;
/// Leader lines shorter than this (px) aren't drawn.
const LEADER_MIN_PX: f64 = 3.0;

pub const SLOTS: usize = 8;

/// A name's slots around its symbol, best first: beside it on the
/// preferred side, then on that side raised and lowered, above, below,
/// the other side raised and lowered, and the other side. `gap` (px)
/// separates name and symbol.
pub fn slots(symbol: &Rect, size: (f64, f64), prefer: Side, gap: f64) -> [Rect; SLOTS] {
    let (w, h) = size;
    let (cx, cy) = symbol.center();
    let right = |y0: f64| Rect::at(symbol.x1 + gap, y0, w, h);
    let left = |y0: f64| Rect::at(symbol.x0 - gap - w, y0, w, h);
    let (mid, raised, lowered) = (cy - h / 2.0, symbol.y0 - 0.75 * h, symbol.y1 - 0.25 * h);
    let above = Rect::at(cx - w / 2.0, symbol.y0 - gap / 2.0 - h, w, h);
    let below = Rect::at(cx - w / 2.0, symbol.y1 + gap / 2.0, w, h);
    match prefer {
        Side::Right => [
            right(mid),
            right(raised),
            right(lowered),
            above,
            below,
            left(raised),
            left(lowered),
            left(mid),
        ],
        Side::Left => [
            left(mid),
            left(raised),
            left(lowered),
            above,
            below,
            right(raised),
            right(lowered),
            right(mid),
        ],
    }
}

/// Places `wanted`, in order (the first have first pick), clear of
/// `obstacles` and each other where they can be; where a name can't be,
/// it takes the slot overlapping least. `last` is each name's slot last
/// time, by key.
pub fn place(
    wanted: &[Wanted],
    obstacles: &[Rect],
    last: &HashMap<String, usize>,
    gap: f64,
) -> Vec<Placed> {
    let mut taken: Vec<Rect> = Vec::with_capacity(wanted.len());
    let mut out = Vec::with_capacity(wanted.len());
    for w in wanted {
        let slots = slots(&w.symbol, w.size, w.prefer, gap);
        let cost = |r: &Rect, pad: f64| -> f64 {
            let r = r.grow(pad);
            obstacles.iter().chain(&taken).map(|o| r.overlap(o)).sum()
        };
        let first_clear = || {
            (0..SLOTS)
                .find(|&i| cost(&slots[i], PAD_PX) == 0.0)
                .unwrap_or_else(|| {
                    (0..SLOTS)
                        .min_by(|&a, &b| {
                            cost(&slots[a], PAD_PX).total_cmp(&cost(&slots[b], PAD_PX))
                        })
                        .unwrap_or(0)
                })
        };
        let slot = match last.get(w.key) {
            Some(&k) if k != 0 && k < SLOTS => {
                if cost(&slots[0], PAD_PX + RETURN_PX) == 0.0 {
                    0
                } else if cost(&slots[k], PAD_PX) == 0.0 {
                    k
                } else {
                    first_clear()
                }
            }
            _ => first_clear(),
        };
        let rect = slots[slot];
        let leader = (slot != 0)
            .then(|| {
                let from = rect.nearest(w.symbol.center());
                let to = w.symbol.nearest(from);
                [from, to]
            })
            .filter(|[a, b]| (a.0 - b.0).hypot(a.1 - b.1) >= LEADER_MIN_PX);
        taken.push(rect);
        out.push(Placed { rect, slot, leader });
    }
    out
}

/// A notice dot to place on its symbol: its diameter (px), its symbol's
/// box (which it may overlap, and nothing else), and where its middle may
/// go, best first.
pub struct Badge {
    pub key: String,
    pub size: f64,
    pub symbol: Rect,
    pub slots: Vec<(f64, f64)>,
}

/// Room (px) beyond which a dot has enough: past it, the slots' order
/// decides, so a dot doesn't wander for room it doesn't need.
const ROOM_ENOUGH: f64 = 2.0;
/// A dot moves from last frame's slot only for this much more room (px).
const ROOM_GAIN_PX: f64 = 4.0;

/// Places `badges` in order: each takes the slot overlapping nothing but
/// its own symbol, or least; then the one with the most room from the
/// other symbols (up to `ROOM_ENOUGH` diameters); then the first. It keeps
/// last frame's slot (`last`, by key) unless another has clearly more
/// room. Each slot as a box, with the index of the one taken.
pub fn place_badges(
    badges: &[Badge],
    obstacles: &[Rect],
    last: &HashMap<String, usize>,
) -> Vec<(Vec<Rect>, usize)> {
    let mut taken: Vec<Rect> = Vec::with_capacity(badges.len());
    let mut out = Vec::with_capacity(badges.len());
    for b in badges {
        let half = b.size / 2.0;
        let rects: Vec<Rect> = b
            .slots
            .iter()
            .map(|&c| Rect::around(c, (half, half)))
            .collect();
        let others = || (obstacles.iter()).filter(|o| **o != b.symbol).chain(&taken);
        let enough = ROOM_ENOUGH * b.size;
        // (overlap, room) for each slot.
        let score: Vec<(f64, f64)> = rects
            .iter()
            .map(|r| {
                let grown = r.grow(PAD_PX);
                let overlap = others().map(|o| grown.overlap(o)).sum();
                let room = others().map(|o| r.gap(o)).fold(enough, f64::min);
                (overlap, room)
            })
            .collect();
        let better = |a: usize, b: usize| {
            let (sa, sb) = (score[a], score[b]);
            sa.0.total_cmp(&sb.0)
                .then(sb.1.total_cmp(&sa.1))
                .then(a.cmp(&b))
        };
        let best = (0..rects.len()).min_by(|&a, &b| better(a, b)).unwrap_or(0);
        let slot = match last.get(&b.key) {
            Some(&k)
                if k < rects.len()
                    && score[k].0 == 0.0
                    && score[best].0 == 0.0
                    && score[best].1 < score[k].1 + ROOM_GAIN_PX =>
            {
                k
            }
            _ => best,
        };
        if let Some(r) = rects.get(slot) {
            taken.push(*r);
        }
        out.push((rects, slot));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAP: f64 = 6.0;

    fn hull(x: f64, y: f64) -> Rect {
        Rect::around((x, y), (10.0, 30.0))
    }

    #[test]
    fn a_lone_name_sits_beside_its_symbol_on_its_side() {
        let boat = hull(100.0, 100.0);
        let wanted = [Wanted {
            key: "v1",
            size: (50.0, 14.0),
            symbol: boat,
            prefer: Side::Right,
        }];
        let placed = place(&wanted, &[boat], &HashMap::new(), GAP);
        assert_eq!(placed[0].slot, 0);
        assert_eq!(placed[0].rect, Rect::at(116.0, 93.0, 50.0, 14.0));
        assert_eq!(placed[0].leader, None);
        let left = Wanted {
            prefer: Side::Left,
            ..wanted.into_iter().next().unwrap()
        };
        let placed = place(&[left], &[boat], &HashMap::new(), GAP);
        assert_eq!(placed[0].rect.x1, 84.0);
    }

    #[test]
    fn boats_side_by_side_take_a_side_each() {
        // Kitsap and Cathlamet at Vashon, both wanting the water (right):
        // the west one's right is the east one's hull.
        let (west, east) = (hull(100.0, 100.0), hull(125.0, 100.0));
        let wanted = [
            Wanted {
                key: "east",
                size: (50.0, 14.0),
                symbol: east,
                prefer: Side::Right,
            },
            Wanted {
                key: "west",
                size: (60.0, 14.0),
                symbol: west,
                prefer: Side::Right,
            },
        ];
        let placed = place(&wanted, &[west, east], &HashMap::new(), GAP);
        assert_eq!(placed[0].slot, 0);
        let mine = &placed[1];
        assert!(mine.slot > 0);
        // Clear of both hulls and the other name, and led back to its boat.
        for other in [west, east, placed[0].rect] {
            assert_eq!(mine.rect.overlap(&other), 0.0);
        }
        let [_, to] = mine.leader.expect("a leader line");
        assert!(west.nearest(to) == to);
    }

    #[test]
    fn names_keep_their_slot_until_their_first_choice_is_well_clear() {
        let boat = hull(100.0, 100.0);
        let wanted = [Wanted {
            key: "v1",
            size: (50.0, 14.0),
            symbol: boat,
            prefer: Side::Right,
        }];
        let last = HashMap::from([("v1".to_string(), 7)]);
        // Something just clear of the first choice: stays on the left.
        let near = Rect::at(167.5, 90.0, 20.0, 20.0);
        assert_eq!(place(&wanted, &[boat, near], &last, GAP)[0].slot, 7);
        // Well clear: back to the right.
        let far = Rect::at(190.0, 90.0, 20.0, 20.0);
        assert_eq!(place(&wanted, &[boat, far], &last, GAP)[0].slot, 0);
    }

    #[test]
    fn a_dot_between_two_boats_goes_to_its_own_boat_s_far_side() {
        // Cathlamet with a notice, Kitsap close on its left: of the dot's
        // spots on Cathlamet's ends, the right-hand ones have the room.
        let (kitsap, cathlamet) = (hull(100.0, 100.0), hull(140.0, 100.0));
        let slots = vec![(130.0, 70.0), (150.0, 70.0), (130.0, 130.0), (150.0, 130.0)];
        let badge = Badge {
            key: "v2".into(),
            size: 16.0,
            symbol: cathlamet,
            slots,
        };
        let placed = place_badges(&[badge], &[kitsap, cathlamet], &HashMap::new());
        assert_eq!(placed[0].1, 1);
        // Alone, the first slot does.
        let alone = Badge {
            key: "v2".into(),
            size: 16.0,
            symbol: cathlamet,
            slots: vec![(130.0, 70.0), (150.0, 70.0)],
        };
        assert_eq!(
            place_badges(&[alone], &[cathlamet], &HashMap::new())[0].1,
            0
        );
    }

    #[test]
    fn a_dot_stays_put_unless_another_slot_is_clearly_roomier() {
        let (kitsap, cathlamet) = (hull(100.0, 100.0), hull(140.0, 100.0));
        let badge = |slots| Badge {
            key: "v2".into(),
            size: 16.0,
            symbol: cathlamet,
            slots,
        };
        // Last on slot 0, 22 px of room; slot 1 has 24: not enough more.
        let last = HashMap::from([("v2".to_string(), 0)]);
        let near = badge(vec![(140.0, 70.0), (142.0, 70.0)]);
        assert_eq!(place_badges(&[near], &[kitsap, cathlamet], &last)[0].1, 0);
        // Slot 1 with plenty more: it moves.
        let far = badge(vec![(140.0, 70.0), (160.0, 70.0)]);
        assert_eq!(place_badges(&[far], &[kitsap, cathlamet], &last)[0].1, 1);
    }

    #[test]
    fn a_boxed_in_name_takes_the_slot_overlapping_least() {
        let boat = hull(100.0, 100.0);
        let wanted = [Wanted {
            key: "v1",
            size: (50.0, 14.0),
            symbol: boat,
            prefer: Side::Right,
        }];
        // Everything around is covered, least so just under its side.
        let cover = [
            Rect::at(0.0, 0.0, 200.0, 125.0),
            Rect::at(0.0, 136.0, 200.0, 100.0),
        ];
        let placed = place(&wanted, &cover, &HashMap::new(), GAP);
        assert_eq!(placed[0].slot, 2);
    }
}
