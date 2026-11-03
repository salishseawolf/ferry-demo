//! Dragging a phone's details sheet by its handle: it follows the finger,
//! and on letting go settles at the nearest of its heights (peek, half,
//! full), as far as a flick would carry it. A tap on the handle still
//! steps through them (setup.rs), as does the keyboard.

use web_sys::PointerEvent;

use super::App;
use super::dom::now_ms;
use crate::finder::Sheet;

/// A press on the handle that moves less than this (CSS px) is a tap.
const SHEET_SLOP_PX: f64 = 10.0;
/// How far ahead (ms) a flick's speed carries the sheet in choosing
/// where it settles.
const FLICK_MS: f64 = 200.0;
/// The finger's speed is taken over this long (ms).
const SPEED_OVER_MS: f64 = 80.0;

pub(super) struct SheetDrag {
    id: i32,
    /// Where the finger came down (CSS px from the top), and the sheet's
    /// height then.
    start_y: f64,
    start_height: f64,
    /// The sheet's heights, measured.
    heights: Vec<(Sheet, f64)>,
    dragging: bool,
    /// The sheet's height lately, with when (ms), for its speed.
    trail: Vec<(f64, f64)>,
}

impl SheetDrag {
    fn height_at(&self, y: f64) -> f64 {
        let lowest = self
            .heights
            .iter()
            .map(|h| h.1)
            .fold(f64::INFINITY, f64::min);
        let highest = self.heights.iter().map(|h| h.1).fold(0.0, f64::max);
        (self.start_height + self.start_y - y).clamp(lowest, highest)
    }

    /// Where the sheet settles from `height`: the height nearest where its
    /// speed would carry it.
    fn settle(&self, height: f64) -> Option<Sheet> {
        let speed = match (self.trail.first(), self.trail.last()) {
            (Some(&(t0, h0)), Some(&(t1, h1))) if t1 > t0 => (h1 - h0) / (t1 - t0),
            _ => 0.0,
        };
        let aim = height + speed * FLICK_MS;
        self.heights
            .iter()
            .min_by(|a, b| (a.1 - aim).abs().total_cmp(&(b.1 - aim).abs()))
            .map(|&(sheet, _)| sheet)
    }
}

impl App {
    pub(super) fn sheet_down(&mut self, e: &PointerEvent) {
        if e.button() != 0 {
            return;
        }
        self.sheet_dragged = false;
        let Some(start_height) = self.finder.sheet_height() else {
            return;
        };
        // Held still to measure, and from now while it's dragged.
        self.finder.hold_sheet(true, None);
        let heights = self.finder.sheet_heights();
        self.finder.hold_sheet(true, Some(start_height));
        if let Some(target) = e.target()
            && let Ok(handle) = wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(target)
        {
            let _ = handle.set_pointer_capture(e.pointer_id());
        }
        self.sheet_drag = Some(SheetDrag {
            id: e.pointer_id(),
            start_y: e.client_y() as f64,
            start_height,
            heights,
            dragging: false,
            trail: vec![(now_ms(), start_height)],
        });
    }

    pub(super) fn sheet_move(&mut self, e: &PointerEvent) {
        let Some(drag) = &mut self.sheet_drag else {
            return;
        };
        if e.pointer_id() != drag.id {
            return;
        }
        let y = e.client_y() as f64;
        drag.dragging |= (y - drag.start_y).abs() > SHEET_SLOP_PX;
        if !drag.dragging {
            return;
        }
        let height = drag.height_at(y);
        let now = now_ms();
        drag.trail.push((now, height));
        drag.trail.retain(|&(t, _)| now - t <= SPEED_OVER_MS);
        self.finder.hold_sheet(true, Some(height));
    }

    /// The finger's up (or the browser took it: `cancelled`): the sheet
    /// settles where it's going, and a drag makes no tap.
    pub(super) fn sheet_up(&mut self, e: &PointerEvent, cancelled: bool) {
        let Some(drag) = self.sheet_drag.take_if(|d| d.id == e.pointer_id()) else {
            return;
        };
        self.finder.hold_sheet(false, None);
        if !drag.dragging {
            return;
        }
        self.sheet_dragged = true;
        let height = drag.height_at(e.client_y() as f64);
        let settle = if cancelled {
            SheetDrag {
                trail: Vec::new(),
                ..drag
            }
            .settle(height)
        } else {
            drag.settle(height)
        };
        if let Some(sheet) = settle {
            self.finder.set_sheet(sheet);
        }
        self.redraw = true;
    }
}
