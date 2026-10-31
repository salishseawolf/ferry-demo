//! Each frame's ferries, terminals and their labels and notice dots, added
//! to the scene.

use crate::finder::Target;
use crate::fleet::Vessel;
use crate::geo::{EARTH_RADIUS_M, LonLat, distance_m};
use crate::live::VESSELS_MS;
use crate::palette;
use crate::placement::{self, Badge, Rect, Side};
use crate::scene::{Marker, MarkerKind, Scene};
use crate::terminals::symbol_scale;
use crate::view::LAYER_FERRIES;
use glam::DVec3;

use super::dom::{clock, date_clock, set_text};
use super::labels::Label;
use super::notes::{Note, NoteItem};
use super::picking::Picked;
use super::{
    ANCHOR_CLEAR_M, ANCHOR_DROP, ANCHOR_HALF_HEIGHT, ANCHOR_HALF_WIDTH, ANCHOR_INLAND, ANCHOR_M,
    App, BOAT_NOTE_REACH, BOAT_NOTE_SHARE, DIM_ALPHA, LABEL_MAX_M_PER_PX, LEG_GLOW, NOTE_M,
    NOTE_MAX_M_PER_PX, NOTE_RAISE, ROUTE_DASH_PX, ROUTE_GLOW, ROUTE_HIGHLIGHT_ALPHA, ROUTE_WIDTH,
    SYMBOL_GLOW_ALPHA, TERMINAL_LABEL_MAX_M_PER_PX, TERMINAL_SAILINGS_SHOWN, VESSEL_HOVER_LINE_PX,
    VESSEL_LINE_OF_BEAM, VESSEL_PICK_PX,
};

/// Ferry hulls are drawn at their real length and beam, to scale with the
/// terrain at every zoom. Outline widths (docked and laid-up boats, and
/// the hovered one) are in CSS px, down to a share of the beam on small
/// hulls. The cursor picks the nearest boat within `VESSEL_PICK_PX`, or
/// within a large hull.
const VESSEL_LINE_PX: f64 = 1.5;
/// Speed vectors and wakes, in standard line widths, and the wake's
/// opacity where it's newest; the tick across a vector's tip, CSS px each
/// side.
const VECTOR_WIDTH: f32 = 0.65;
const WAKE_WIDTH: f32 = 1.5;
const WAKE_ALPHA: f64 = 0.55;
const VECTOR_TICK_PX: f64 = 3.5;
/// A boat within this distance (m) of a terminal would rather be named on
/// the side away from the terminal's anchor, clear of it.
const LABEL_NEAR_TERMINAL_M: f64 = 500.0;
/// A point this far (m) along a direction gives it on screen.
const HEADING_PROBE_M: f64 = 200.0;
/// Moving boats are redrawn once the fastest in view has gone this far on
/// screen (CSS px), but no more often than every `VESSEL_MIN_FRAME_MS`:
/// at the whole-fleet view that's a frame every half minute, not 60 a
/// second.
const VESSEL_STEP_PX: f64 = 0.5;
const VESSEL_MIN_FRAME_MS: f64 = 33.0;

impl App {
    /// The ferries and terminals in view. Each boat is a hull, with its
    /// wake and, under way, its speed vector; each terminal an anchor with
    /// a gauge of its next sailing's drive-up space. Names show once zoomed
    /// in. The boat or terminal under the cursor is outlined and described
    /// in the HUD. Also works out when moving boats next need a redraw.
    pub(super) fn add_ferries(&mut self, scene: &mut Scene) {
        self.vessels_step_ms = f64::INFINITY;
        if self.layers & LAYER_FERRIES == 0 {
            set_text(&self.hud_vessel, "");
            self.focus_points.clear();
            self.pointed = None;
            let _ = self
                .canvas
                .class_list()
                .toggle_with_force("pickable", false);
            self.labels.place(&[], &[], &[]);
            self.notes.place(&[], (0.0, 0.0));
            return;
        }
        let now = js_sys::Date::now();
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        let (w, h) = (
            self.canvas.client_width() as f64,
            self.canvas.client_height() as f64,
        );
        let (_, m_per_px, reach_m) = self.view_scale();
        // Symbol sizes in CSS px, to scale with the terrain; terminals'
        // shrink as the view zooms in (terminals::symbol_scale).
        let to_px = |m: f64| m / m_per_px;
        let terminal_scale = symbol_scale(2.0 * reach_m);
        let anchor_px = to_px(ANCHOR_M * terminal_scale);
        let hull_px = |v: &Vessel| {
            let (length, beam) = self.fleet.size_m(v);
            (to_px(length), to_px(beam))
        };
        // The longest boat is about 140 m.
        let margin = to_px(140.0).max(anchor_px);
        let in_view = |p: DVec3| {
            self.screen_px(p).filter(|s| {
                (-margin..w + margin).contains(&s.0) && (-margin..h + margin).contains(&s.1)
            })
        };
        let vessels = &self.fleet.vessels;
        let mut shown: Vec<(usize, DVec3, (f64, f64))> = vessels
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let p = v.position_at(now);
                in_view(p).map(|s| (i, p, s))
            })
            .collect();
        let terminals = &self.terminals;
        let slips: Vec<(usize, DVec3, (f64, f64))> = terminals
            .list
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let p = t.position.to_unit();
                in_view(p).map(|s| (i, p, s))
            })
            .collect();

        // Moving boats in view: redraw once the fastest moves a step.
        let fastest = shown
            .iter()
            .map(|&(i, ..)| &vessels[i])
            .filter(|v| v.is_moving())
            .map(Vessel::speed_mps)
            .fold(0.0, f64::max);
        if fastest > 0.0 {
            let step_ms = 1000.0 * VESSEL_STEP_PX * m_per_px / fastest;
            self.vessels_step_ms = step_ms.clamp(VESSEL_MIN_FRAME_MS, VESSELS_MS);
        }

        // Anchors stand a little inland of their slips, so boats at the
        // dock don't cover them; boats are drawn over them anyway.
        let facing = |p: DVec3, deg: f64| {
            let (east, north) = LonLat::from_vec(p).east_north();
            let d = deg.to_radians();
            east * d.sin() + north * d.cos()
        };
        let inland = (ANCHOR_INLAND * anchor_px * m_per_px + ANCHOR_CLEAR_M) / self.camera.radius;
        let anchor_at = |i: usize, p: DVec3| {
            terminals
                .facing_deg(&terminals.list[i])
                .map_or(p, |deg| (p - facing(p, deg) * inland).normalize())
        };

        // Whatever is nearest the cursor, boat or terminal.
        let dragging = self.press.as_ref().is_some_and(|p| p.dragging);
        let hovered = self.cursor.filter(|_| !dragging).and_then(|c| {
            // Distance to the cursor, less the symbol's reach beyond the
            // pick radius, so large symbols are picked anywhere on them.
            let near = |s: (f64, f64), reach: f64| {
                (s.0 - c.0).hypot(s.1 - c.1) - (reach - VESSEL_PICK_PX).max(0.0)
            };
            let boats = shown
                .iter()
                .map(|&(i, _, s)| (Picked::Vessel(i), near(s, hull_px(&vessels[i]).0 / 2.0)));
            // Terminals where their anchors are drawn: within the box of
            // the anchor and the gauge beneath it, grown to the pick
            // radius if smaller, ranked as boats at the box's edge and
            // nearer within it (so a boat over an anchor wins).
            let docks = slips.iter().filter_map(|&(i, p, _)| {
                let (x, y) = self.screen_px(anchor_at(i, p))?;
                let half = (
                    (anchor_px * ANCHOR_HALF_WIDTH).max(VESSEL_PICK_PX),
                    (anchor_px * ANCHOR_HALF_HEIGHT).max(VESSEL_PICK_PX),
                );
                let center = (x, y + anchor_px * ANCHOR_DROP);
                let q = (
                    (c.0 - center.0).abs() - half.0,
                    (c.1 - center.1).abs() - half.1,
                );
                let outside = q.0.max(0.0).hypot(q.1.max(0.0));
                let inside = q.0.max(q.1).min(0.0);
                Some((Picked::Terminal(i), VESSEL_PICK_PX + outside + inside))
            });
            boats
                .chain(docks)
                .filter(|&(_, d)| d <= VESSEL_PICK_PX)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(picked, _)| picked)
        });
        // Failing a boat or terminal, the route line under the cursor.
        let hovered_route = if hovered.is_none() && !dragging {
            self.cursor.and_then(|c| self.route_at(c))
        } else {
            None
        };
        // What the pointer's on, or else the finder's selection.
        let pointing = hovered.is_some() || hovered_route.is_some();
        self.pointed = match hovered {
            Some(Picked::Vessel(i)) => Some(Target::Vessel(vessels[i].id)),
            Some(Picked::Terminal(i)) => Some(Target::Terminal(self.terminals.list[i].id)),
            None => hovered_route
                .and_then(|r| self.service_routes.iter().position(|s| s.legs.contains(&r)))
                .map(Target::Route),
        };
        let lit = match self.finder.entry() {
            Some(e) if !pointing => self.highlight_target(e.target),
            _ => self.highlight(hovered, hovered_route),
        };
        self.focus_points = if pointing {
            self.points_around(&lit, now)
        } else {
            Vec::new()
        };
        let pickable = !self.focus_points.is_empty();
        let _ = self
            .canvas
            .class_list()
            .toggle_with_force("pickable", pickable);
        // What isn't highlighted fades back while a route is.
        let dim = |on: bool| {
            if lit.dim && !on { DIM_ALPHA } else { 1.0 }
        };
        let fade = |mut c: [f32; 4], k: f32| {
            c[3] *= k;
            c
        };
        let glow = |on: bool| {
            palette::linear_rgba(
                palette::VESSEL_HOVER,
                if on { SYMBOL_GLOW_ALPHA } else { 0.0 },
            )
        };
        for &r in &lit.routes {
            let points = &self.routes[r].points;
            let leg = (lit.leg == Some(r) && lit.routes.len() > 1).then_some(LEG_GLOW);
            for (width, alpha) in ROUTE_GLOW.into_iter().chain(leg) {
                let color = palette::linear_rgba(palette::VESSEL_HOVER, alpha);
                scene.add_line(points, self.camera.radius, color, width);
            }
            let ink = palette::linear_rgba(palette::VESSEL_INK, ROUTE_HIGHLIGHT_ALPHA);
            scene.add_dashed_line(points, self.camera.radius, ink, ROUTE_WIDTH, ROUTE_DASH_PX);
        }

        // Out of service at the bottom, then docked, then under way, and
        // the hovered boat over all.
        let rank = |i: usize| {
            let v = &vessels[i];
            (
                hovered == Some(Picked::Vessel(i)),
                v.in_service,
                v.is_moving(),
            )
        };
        shown.sort_by_key(|&(i, ..)| rank(i));

        let radius = self.camera.radius;
        let ink = |alpha: f32| palette::linear_rgba(palette::VESSEL_INK, alpha);
        for &(i, p, _) in &shown {
            let v = &vessels[i];
            let k = dim(lit.vessels.contains(&i));
            let ink = |alpha: f32| ink(alpha * k);
            // The wake fades with age, segment by segment.
            for pair in self.fleet.wake(v, now).windows(2) {
                let age = (pair[0].1 + pair[1].1) / 2.0;
                let alpha = (WAKE_ALPHA * (1.0 - age)) as f32;
                scene.add_line(&[pair[0].0, pair[1].0], radius, ink(alpha), WAKE_WIDTH);
            }
            if v.is_moving() {
                // To where it will be in 3 minutes, with a tick across the tip.
                let tip = v.ahead_of(p);
                let across = tip.cross(v.heading_from(tip)).normalize_or_zero();
                let tick = VECTOR_TICK_PX * m_per_px / radius;
                let ends = [tip + across * tick, tip - across * tick].map(DVec3::normalize);
                scene.add_line(&[p, tip], radius, ink(0.9), VECTOR_WIDTH);
                scene.add_line(&ends, radius, ink(0.9), VECTOR_WIDTH);
            }
        }

        scene.markers.extend(slips.iter().map(|&(i, p, _)| {
            let t = &terminals.list[i];
            let next = terminals.next_sailing(t, now);
            let gauge = if next.is_some_and(|s| s.nearly_full()) {
                palette::TERMINAL_NEARLY_FULL
            } else {
                palette::VESSEL_INK
            };
            let ink = if hovered == Some(Picked::Terminal(i)) {
                palette::VESSEL_HOVER
            } else {
                palette::VESSEL_INK
            };
            let on = lit.terminals.contains(&t.id);
            let k = dim(on);
            Marker {
                position: (anchor_at(i, p) * radius).as_vec3().to_array(),
                heading: [0.0; 3],
                size: [(anchor_px * dpr) as f32, (anchor_px * dpr) as f32],
                style: [
                    0.0,
                    0.0,
                    MarkerKind::Anchor as u8 as f32,
                    next.and_then(|s| s.space_left()).map_or(-1.0, |f| f as f32),
                ],
                fill: palette::linear_rgba(gauge, k),
                outline: palette::linear_rgba(ink, k),
                glow: glow(on),
            }
        }));
        scene.markers.extend(shown.iter().map(|&(i, p, _)| {
            let v = &vessels[i];
            let (fill, outline, line, dashed) = if !v.in_service {
                (ink_paper(0.6), palette::VESSEL_IDLE, VESSEL_LINE_PX, true)
            } else if v.is_moving() {
                (ink(1.0), palette::VESSEL_INK, 0.0, false)
            } else {
                (ink_paper(1.0), palette::VESSEL_INK, VESSEL_LINE_PX, false)
            };
            let (outline, line, dashed) = if hovered == Some(Picked::Vessel(i)) {
                (palette::VESSEL_HOVER, VESSEL_HOVER_LINE_PX, false)
            } else {
                (outline, line, dashed)
            };
            let (length, beam) = hull_px(v);
            // The outline scales down with a small hull, not past it.
            let line = line.min(beam * VESSEL_LINE_OF_BEAM);
            let on = lit.vessels.contains(&i);
            let k = dim(on);
            Marker {
                position: (p * radius).as_vec3().to_array(),
                heading: v.heading_from(p).as_vec3().to_array(),
                size: [(length * dpr) as f32, (beam * dpr) as f32],
                style: [
                    (line * dpr) as f32,
                    f32::from(u8::from(dashed)),
                    MarkerKind::Hull as u8 as f32,
                    -1.0,
                ],
                fill: fade(fill, k),
                outline: palette::linear_rgba(outline, k),
                glow: glow(on),
            }
        }));

        // What names keep clear of: each hull in view (its box on screen,
        // turned to its heading) and each anchor with its gauge. Notice
        // dots are added below, as they're placed.
        // A hull's two ends on screen, as it's turned to its heading.
        let hull_ends = |v: &Vessel, p: DVec3| {
            let half = facing(p, v.heading_deg) * (self.fleet.size_m(v).0 / 2.0 / radius);
            match [p + half, p - half].map(|e| self.screen_px(e.normalize())) {
                [Some(a), Some(b)] => Some([a, b]),
                _ => None,
            }
        };
        let hull_box = |v: &Vessel, p: DVec3, s: (f64, f64)| {
            let (length, beam) = self.fleet.size_m(v);
            match hull_ends(v, p) {
                Some(ends) => Rect::bounding(&ends, to_px(beam) / 2.0),
                None => Rect::around(s, (to_px(length) / 2.0, to_px(length) / 2.0)),
            }
        };
        let anchor_box = |i: usize, p: DVec3, slip: (f64, f64)| {
            let s = self.screen_px(anchor_at(i, p)).unwrap_or(slip);
            Rect::around(
                (s.0, s.1 + anchor_px * ANCHOR_DROP),
                (
                    anchor_px * ANCHOR_HALF_WIDTH,
                    anchor_px * ANCHOR_HALF_HEIGHT,
                ),
            )
        };
        let mut obstacles: Vec<Rect> = shown
            .iter()
            .map(|&(i, p, s)| hull_box(&vessels[i], p, s))
            .chain(slips.iter().map(|&(i, p, slip)| anchor_box(i, p, slip)))
            .collect();

        // Names once zoomed in enough for them to sit beside their symbols
        // (placement), preferring the side away from `from` if given: a
        // boat's terminal when it's near one, otherwise its speed vector;
        // or, for a terminal, the water its slip opens onto. What's
        // highlighted is named first, then terminals, then boats.
        let away = |s: (f64, f64), from: Option<(f64, f64)>| {
            if from.is_some_and(|f| f.0 > s.0) {
                Side::Left
            } else {
                Side::Right
            }
        };
        let any_lit = !lit.vessels.is_empty() || !lit.terminals.is_empty();
        let rank = |is_lit: bool, kind: u8| if any_lit && is_lit { 0 } else { kind };
        let mut named = Vec::new();
        if m_per_px <= TERMINAL_LABEL_MAX_M_PER_PX {
            for &(i, p, slip) in &slips {
                let t = &terminals.list[i];
                let water = terminals.facing_deg(t).and_then(|deg| {
                    let probe = HEADING_PROBE_M / radius;
                    self.screen_px((p + facing(p, deg) * probe).normalize())
                });
                let symbol = anchor_box(i, p, slip);
                let is_lit = lit.terminals.contains(&t.id);
                named.push(Label {
                    key: format!("t{}", t.id),
                    text: t.name.clone(),
                    symbol,
                    prefer: away(symbol.center(), water),
                    rank: rank(is_lit, 1),
                    class: "terminal",
                    dim: dim(is_lit) < 1.0,
                    lit: is_lit,
                });
            }
        }
        if m_per_px <= LABEL_MAX_M_PER_PX {
            // A boat near a terminal (docked, arriving or leaving) would
            // rather be named on the side away from the terminal's anchor,
            // over the water.
            let near_anchor = |p: DVec3| {
                let at = LonLat::from_vec(p);
                let (i, t) = (terminals.list.iter().enumerate())
                    .map(|(i, t)| (i, t, distance_m(at, t.position, EARTH_RADIUS_M)))
                    .filter(|&(.., d)| d <= LABEL_NEAR_TERMINAL_M)
                    .min_by(|a, b| a.2.total_cmp(&b.2))
                    .map(|(i, t, _)| (i, t))?;
                self.screen_px(anchor_at(i, t.position.to_unit()))
            };
            for &(i, p, s) in &shown {
                let v = &vessels[i];
                let from = near_anchor(p).or_else(|| {
                    v.is_moving()
                        .then(|| self.screen_px(v.ahead_of(p)))
                        .flatten()
                });
                let is_lit = lit.vessels.contains(&i);
                named.push(Label {
                    key: format!("v{}", v.id),
                    text: v.name.clone(),
                    symbol: hull_box(v, p, s),
                    prefer: away(s, from),
                    rank: rank(is_lit, 2),
                    class: if v.in_service { "" } else { "idle" },
                    dim: dim(is_lit) < 1.0,
                    lit: is_lit,
                });
            }
        }

        // A dot on each boat or terminal with a notice in effect, which
        // opens to the notice on hover or click. Placed before the names
        // (placement::place_badges): overlapping its own symbol, on the
        // side of it with the most room, upper right where there's room
        // all round.
        let mut notes = Vec::new();
        let mut badges = Vec::new();
        if m_per_px <= NOTE_MAX_M_PER_PX {
            let dot_px = to_px(NOTE_M * terminal_scale);
            // Upper first, then right.
            let by_preference = |mut slots: Vec<(f64, f64)>| {
                slots.sort_by(|a, b| (a.1 - 0.5 * a.0).total_cmp(&(b.1 - 0.5 * b.0)));
                slots
            };
            for &(i, p, slip) in &slips {
                let t = &terminals.list[i];
                let bulletins = terminals.bulletins(t);
                if bulletins.is_empty() {
                    continue;
                }
                // The anchor's corners and sides.
                let s = self.screen_px(anchor_at(i, p)).unwrap_or(slip);
                let half = anchor_px / 2.0;
                let (up, out) = (half + dot_px * NOTE_RAISE, half + 0.3 * dot_px);
                badges.push(Badge {
                    key: format!("t{}", t.id),
                    size: dot_px,
                    symbol: anchor_box(i, p, slip),
                    slots: by_preference(vec![
                        (s.0 + half, s.1 - up),
                        (s.0 - half, s.1 - up),
                        (s.0 + out, s.1),
                        (s.0 - out, s.1),
                        (s.0 + half, s.1 + half),
                        (s.0 - half, s.1 + half),
                    ]),
                });
                notes.push(Note {
                    key: format!("t{}", t.id),
                    at: s,
                    size: dot_px,
                    dim: dim(lit.terminals.contains(&t.id)) < 1.0,
                    heading: format!("{} terminal", t.name),
                    items: bulletins
                        .iter()
                        .map(|b| NoteItem {
                            title: b.title.clone(),
                            text: b.text.clone(),
                            when: (b.updated_ms > 0.0)
                                .then(|| format!("Updated {}", date_clock(b.updated_ms))),
                        })
                        .collect(),
                });
            }
            for &(i, p, s) in &shown {
                let v = &vessels[i];
                let Some(note) = &v.note else {
                    continue;
                };
                let (length, beam) = hull_px(v);
                let boat_dot = length * BOAT_NOTE_SHARE;
                // Either side of each end of the hull, and just past each
                // end, however it's turned.
                let [a, b] = hull_ends(v, p)
                    .unwrap_or([(s.0 + length / 2.0, s.1), (s.0 - length / 2.0, s.1)]);
                let span = (a.0 - b.0).hypot(a.1 - b.1).max(1e-6);
                let along = ((a.0 - b.0) / span, (a.1 - b.1) / span);
                let across = (-along.1, along.0);
                let side = beam / 2.0 + 0.35 * boat_dot;
                let mut slots = Vec::new();
                for (end, sign) in [(a, 1.0), (b, -1.0)] {
                    let base = (
                        s.0 + (end.0 - s.0) * BOAT_NOTE_REACH,
                        s.1 + (end.1 - s.1) * BOAT_NOTE_REACH,
                    );
                    for k in [1.0, -1.0] {
                        slots.push((base.0 + across.0 * side * k, base.1 + across.1 * side * k));
                    }
                    let past = 0.4 * boat_dot * sign;
                    slots.push((end.0 + along.0 * past, end.1 + along.1 * past));
                }
                badges.push(Badge {
                    key: format!("v{}", v.id),
                    size: boat_dot,
                    symbol: hull_box(v, p, s),
                    slots: by_preference(slots),
                });
                notes.push(Note {
                    key: format!("v{}", v.id),
                    at: s,
                    size: boat_dot,
                    dim: dim(lit.vessels.contains(&i)) < 1.0,
                    heading: v.name.clone(),
                    items: vec![NoteItem {
                        title: note.clone(),
                        text: String::new(),
                        when: None,
                    }],
                });
            }
        }
        let dots = placement::place_badges(&badges, &obstacles, &self.dot_slots);
        self.dot_slots.clear();
        for ((note, b), (rects, slot)) in notes.iter_mut().zip(&badges).zip(&dots) {
            if let Some(r) = rects.get(*slot) {
                note.at = r.center();
            }
            self.dot_slots.insert(b.key.clone(), *slot);
        }
        self.notes.place(&notes, (w, h));
        obstacles.extend((notes.iter()).map(|n| Rect::around(n.at, (n.size / 2.0, n.size / 2.0))));
        self.labels.place(&named, &obstacles, &dots);

        let text = match hovered {
            Some(Picked::Vessel(i)) => vessels[i].describe(now, clock).join("\n"),
            Some(Picked::Terminal(i)) => terminals
                .describe(&terminals.list[i], now, TERMINAL_SAILINGS_SHOWN, clock)
                .join("\n"),
            None => hovered_route
                .map(|r| self.describe_route(r, &lit))
                .unwrap_or_default(),
        };
        set_text(&self.hud_vessel, &text);
    }
}

/// Map paper, for docked and laid-up hulls, as linear RGBA.
fn ink_paper(alpha: f32) -> [f32; 4] {
    palette::linear_rgba(palette::VESSEL_PAPER, alpha)
}
