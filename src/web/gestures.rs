//! The map's pointer gestures: one pointer drags the map, and a swipe
//! lets it glide on, slowing; two pinch it (zooming about their midpoint
//! and moving with it); a press that stays put is a click, and a double
//! tap on nothing zooms in. Fingers get more room than a mouse: to wobble
//! in a tap, and to pick what they're on.

use std::f64::consts::LN_2;

use glam::{DVec2, DVec3};
use wasm_bindgen::JsCast;
use web_sys::PointerEvent;

use super::App;
use super::dom::{now_ms, prefers_reduced_motion};

/// A press that moves less than this (CSS px) is a click, not a drag:
/// with a mouse, and with a finger.
const CLICK_SLOP_PX: f64 = 4.0;
const TAP_SLOP_PX: f64 = 10.0;
/// A boat or terminal is picked within this (CSS px) of the pointer, a
/// route's line within the other: with a mouse, and with a finger.
const VESSEL_PICK_PX: f64 = 14.0;
const VESSEL_TAP_PX: f64 = 24.0;
const ROUTE_PICK_PX: f64 = 6.0;
const ROUTE_TAP_PX: f64 = 16.0;
/// A second tap on nothing this soon (ms) and this near (CSS px) the
/// first zooms in, twice as far.
const DOUBLE_TAP_MS: f64 = 300.0;
const DOUBLE_TAP_PX: f64 = 24.0;
/// A drag let go of while moving glides on: at the pointer's speed over
/// its last `GLIDE_SPEED_OVER_MS`, unless it rested `GLIDE_REST_MS` before
/// letting go or went slower than `GLIDE_MIN_SPEED` (CSS px/ms), and no
/// faster than `GLIDE_MAX_SPEED`. Its speed falls by e every
/// `GLIDE_DECAY_MS`, so it goes on about that long times its speed, and
/// it stops below `GLIDE_STOP_SPEED`.
const GLIDE_SPEED_OVER_MS: f64 = 80.0;
const GLIDE_REST_MS: f64 = 60.0;
const GLIDE_MIN_SPEED: f64 = 0.3;
const GLIDE_MAX_SPEED: f64 = 3.0;
const GLIDE_DECAY_MS: f64 = 300.0;
const GLIDE_STOP_SPEED: f64 = 0.02;

/// The pointers down on the map (one or two; a third is ignored), and
/// what they hold.
pub(super) struct Press {
    pointers: Vec<(i32, (f64, f64))>,
    /// Where the first came down (CSS px), which a click is at.
    pub(super) start: (f64, f64),
    /// Moved past the slop, ever pinched, or caught the map gliding: not
    /// a click.
    pub(super) dragging: bool,
    pinched: bool,
    hold: Hold,
    /// A lone pointer's latest positions (CSS px), with when (ms), for
    /// its speed when it lets go.
    trail: Vec<(f64, (f64, f64))>,
}

/// The map gliding on after a swipe: its speed (CSS px/ms, right and down
/// as the pointer went), and the time (ms) it was last moved to.
pub(super) struct Glide {
    velocity: DVec2,
    at_ms: f64,
}

enum Hold {
    /// The surface point (a unit vector) under the pointer, kept there;
    /// None if it came down off the planet.
    Drag(Option<DVec3>),
    /// The surface point under the fingers' midpoint, and the altitude and
    /// the fingers' spread (CSS px) when they took hold.
    Pinch {
        grabbed: Option<DVec3>,
        altitude: f64,
        spread: f64,
    },
}

fn midpoint(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

fn spread(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

impl App {
    /// The pick radius for boats and terminals, for the pointer in use.
    pub(super) fn vessel_pick_px(&self) -> f64 {
        if self.touch {
            VESSEL_TAP_PX
        } else {
            VESSEL_PICK_PX
        }
    }

    /// The pick radius for route lines, for the pointer in use.
    pub(super) fn route_pick_px(&self) -> f64 {
        if self.touch {
            ROUTE_TAP_PX
        } else {
            ROUTE_PICK_PX
        }
    }

    /// What the press's pointers hold, taken afresh where they are now:
    /// when one comes down or lifts, so the map doesn't jump.
    fn take_hold(&self, press: &mut Press) {
        press.hold = match press.pointers.as_slice() {
            [(_, a), (_, b)] => Hold::Pinch {
                grabbed: self.surface_at(midpoint(*a, *b)),
                altitude: self.camera.altitude,
                spread: spread(*a, *b).max(1.0),
            },
            [(_, a), ..] => Hold::Drag(self.surface_at(*a)),
            [] => Hold::Drag(None),
        };
    }

    pub(super) fn pointer_down(&mut self, e: &PointerEvent) {
        self.redraw = true;
        self.flight = None;
        // A press stops a glide, and is no tap: as a phone's lists do.
        let caught = self.glide.take().is_some();
        if e.button() != 0 {
            return;
        }
        self.touch = e.pointer_type() == "touch";
        let id = e.pointer_id();
        let pos = (e.offset_x() as f64, e.offset_y() as f64);
        // Where a tap lands, which a still tap gives no move to say.
        self.cursor = Some(pos);
        let mut press = match self.press.take() {
            Some(press) if press.pointers.len() >= 2 => {
                self.press = Some(press);
                return;
            }
            Some(mut press) => {
                press.pointers.retain(|&(p, _)| p != id);
                press.pointers.push((id, pos));
                // Two fingers make no click, nor glide.
                press.pinched |= press.pointers.len() > 1;
                press.dragging |= press.pinched;
                press
            }
            None => Press {
                pointers: vec![(id, pos)],
                start: pos,
                dragging: caught,
                pinched: false,
                hold: Hold::Drag(None),
                trail: Vec::new(),
            },
        };
        let _ = self.canvas.set_pointer_capture(id);
        self.take_hold(&mut press);
        self.press = Some(press);
    }

    pub(super) fn pointer_move(&mut self, e: &PointerEvent) {
        self.redraw = true;
        self.touch = e.pointer_type() == "touch";
        let pos = (e.offset_x() as f64, e.offset_y() as f64);
        self.cursor = Some(pos);
        let slop = if self.touch {
            TAP_SLOP_PX
        } else {
            CLICK_SLOP_PX
        };
        let Some(press) = &mut self.press else {
            return;
        };
        let Some(pointer) = press
            .pointers
            .iter_mut()
            .find(|(p, _)| *p == e.pointer_id())
        else {
            return;
        };
        pointer.1 = pos;
        if press.pointers.len() == 1 {
            // Each move since the last event: a busy page gets them merged
            // into one per frame.
            let moves = e.get_coalesced_events();
            let merged =
                (0..moves.length()).filter_map(|i| moves.get(i).dyn_into::<PointerEvent>().ok());
            for m in merged.chain((moves.length() == 0).then(|| e.clone())) {
                press
                    .trail
                    .push((m.time_stamp(), (m.offset_x() as f64, m.offset_y() as f64)));
            }
            // The last over the time its speed is taken, and one before.
            let now = e.time_stamp();
            let recent = press
                .trail
                .iter()
                .filter(|&&(t, _)| now - t <= GLIDE_SPEED_OVER_MS)
                .count();
            let excess = press.trail.len().saturating_sub(recent + 1);
            press.trail.drain(..excess);
        }
        press.dragging |= spread(pos, press.start) > slop;
        if !press.dragging {
            return;
        }
        match (&press.hold, press.pointers.as_slice()) {
            (&Hold::Drag(Some(grabbed)), &[(_, at)]) => {
                let (x, y) = self.ndc(at);
                self.camera.drag_to(grabbed, x, y);
            }
            (
                &Hold::Pinch {
                    grabbed,
                    altitude,
                    spread: from,
                },
                &[(_, a), (_, b)],
            ) => {
                let altitude = altitude * from / spread(a, b).max(1.0);
                let (x, y) = self.ndc(midpoint(a, b));
                match grabbed {
                    Some(grabbed) => self.camera.pinch(grabbed, altitude, x, y),
                    None => self.camera.zoom((altitude / self.camera.altitude).ln()),
                }
            }
            _ => {}
        }
    }

    /// A pointer lifted. The last one up ends the press: a click if it
    /// never dragged, or else maybe a glide.
    pub(super) fn pointer_up(&mut self, e: &PointerEvent) {
        self.redraw = true;
        if e.button() != 0 {
            return;
        }
        let Some(press) = self.lift(e.pointer_id()) else {
            return;
        };
        if press.dragging {
            self.glide = glide_from(&press, e.time_stamp());
            return;
        }
        // A click on the map first closes an open note.
        if self.notes.close() {
            return;
        }
        // Measuring, a click sets a point; otherwise a click on something
        // flies there and brings it up in the finder (next frame), and a
        // second tap on nothing zooms in.
        if !self.measuring {
            let touch = e.pointer_type() == "touch";
            let at = press.start;
            match self.empty_tap.take() {
                Some((first, time))
                    if touch
                        && now_ms() - time < DOUBLE_TAP_MS
                        && spread(first, at) < DOUBLE_TAP_PX =>
                {
                    let (x, y) = self.ndc(at);
                    self.camera.zoom_at(-LN_2, x, y);
                    self.cursor = None;
                }
                _ => self.pending_click = Some((at, touch)),
            }
            return;
        }
        if let Some(p) = self.hover() {
            if self.measure.is_finished() || self.measure.source.is_none() {
                self.measure_end = None;
            }
            self.measure.click(p);
        }
    }

    /// The browser took a pointer over (a system gesture, say): it's
    /// gone, and makes no click.
    pub(super) fn pointer_cancel(&mut self, e: &PointerEvent) {
        self.redraw = true;
        let _ = self.lift(e.pointer_id());
        if self.touch {
            self.cursor = None;
        }
    }

    /// Takes pointer `id` off the press. If others are still down, they
    /// take hold where they are; if it was the last, the press is over and
    /// returned. None if it wasn't on the press.
    fn lift(&mut self, id: i32) -> Option<Press> {
        let mut press = self.press.take()?;
        let before = press.pointers.len();
        press.pointers.retain(|&(p, _)| p != id);
        if press.pointers.len() == before {
            self.press = Some(press);
            return None;
        }
        if press.pointers.is_empty() {
            return Some(press);
        }
        self.take_hold(&mut press);
        self.press = Some(press);
        None
    }

    /// Moves a glide on to frame time `time_ms`, ending it once it's
    /// slow, or the view reaches the edge of the area.
    pub(super) fn glide_on(&mut self, time_ms: f64) {
        let Some(glide) = &mut self.glide else {
            return;
        };
        // A frame long late (a tab hidden, say) doesn't leap.
        let dt = (time_ms - glide.at_ms).clamp(0.0, 100.0);
        glide.at_ms = time_ms;
        let decay = (-dt / GLIDE_DECAY_MS).exp();
        let px = glide.velocity * GLIDE_DECAY_MS * (1.0 - decay);
        glide.velocity *= decay;
        let slow = glide.velocity.length() < GLIDE_STOP_SPEED;
        let m_per_px = self
            .camera
            .m_per_px(f64::from(self.canvas.client_height().max(1)));
        // The map moves with the pointer: right and down on the screen,
        // so the view goes west and north.
        let whole = self.camera.pan_by(DVec2::new(-px.x, px.y) * m_per_px);
        if slow || !whole {
            self.glide = None;
        }
        self.redraw = true;
    }

    /// After a tap is acted on (next frame): one on nothing may be the
    /// first of a double tap.
    pub(super) fn tapped(&mut self, at: (f64, f64), on_something: bool) {
        self.empty_tap = (!on_something).then(|| (at, now_ms()));
    }
}

/// The glide a drag lets go into at `up_ms`, if it was moving.
fn glide_from(press: &Press, up_ms: f64) -> Option<Glide> {
    if press.pinched || prefers_reduced_motion() {
        return None;
    }
    // Over the last `GLIDE_SPEED_OVER_MS`, or from the move before if
    // that has only one (as on a busy page).
    let &(t1, b) = press.trail.last()?;
    let recent = press
        .trail
        .iter()
        .filter(|&&(t, _)| up_ms - t <= GLIDE_SPEED_OVER_MS)
        .count();
    let first = press.trail.len().saturating_sub(recent.max(2));
    let &(t0, a) = press.trail.get(first)?;
    if up_ms - t1 > GLIDE_REST_MS || t1 - t0 < 1.0 {
        return None;
    }
    let velocity = (DVec2::new(b.0, b.1) - DVec2::new(a.0, a.1)) / (t1 - t0);
    (velocity.length() >= GLIDE_MIN_SPEED).then(|| Glide {
        velocity: velocity.clamp_length_max(GLIDE_MAX_SPEED),
        at_ms: up_ms,
    })
}
