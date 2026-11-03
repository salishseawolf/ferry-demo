//! The map's pointer gestures: one pointer drags the map, two pinch it
//! (zooming about their midpoint and moving with it), a press that stays
//! put is a click, and a double tap on nothing zooms in. Fingers get more
//! room than a mouse: to wobble in a tap, and to pick what they're on.

use std::f64::consts::LN_2;

use glam::DVec3;
use web_sys::PointerEvent;

use super::App;
use super::dom::now_ms;

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

/// The pointers down on the map (one or two; a third is ignored), and
/// what they hold.
pub(super) struct Press {
    pointers: Vec<(i32, (f64, f64))>,
    /// Where the first came down (CSS px), which a click is at.
    pub(super) start: (f64, f64),
    /// Moved past the slop, or ever pinched: not a click.
    pub(super) dragging: bool,
    hold: Hold,
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
                // Two fingers make no click.
                press.dragging |= press.pointers.len() > 1;
                press
            }
            None => Press {
                pointers: vec![(id, pos)],
                start: pos,
                dragging: false,
                hold: Hold::Drag(None),
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
    /// never dragged.
    pub(super) fn pointer_up(&mut self, e: &PointerEvent) {
        self.redraw = true;
        if e.button() != 0 {
            return;
        }
        let Some(press) = self.lift(e.pointer_id()) else {
            return;
        };
        if press.dragging {
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

    /// After a tap is acted on (next frame): one on nothing may be the
    /// first of a double tap.
    pub(super) fn tapped(&mut self, at: (f64, f64), on_something: bool) {
        self.empty_tap = (!on_something).then(|| (at, now_ms()));
    }
}
