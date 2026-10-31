//! The distance measuring tool's box.

use web_sys::Element;

use crate::measure::Reading;

use super::App;
use super::dom::{copy_to_clipboard, set_text};

impl App {
    /// The current measurement, finished or still following the cursor.
    pub(super) fn reading(&self) -> Option<Reading> {
        let (a, b) = self.measure.span(self.measure_end)?;
        Some(Reading::new(a, b, self.camera.radius))
    }

    pub(super) fn set_measuring(&mut self, on: bool) {
        self.measuring = on;
        self.redraw = true;
        // Out of the way of the map, which measuring is done on.
        if on {
            self.set_settings_open(false);
        }
        if !on {
            self.measure.clear();
            self.measure_end = None;
        }
        let b = &self.measure_box;
        let _ = b
            .tool
            .set_attribute("aria-pressed", if on { "true" } else { "false" });
        let _ = self.canvas.class_list().toggle_with_force("measuring", on);
        self.update_measure_box();
    }

    pub(super) fn update_measure_box(&self) {
        let b = &self.measure_box;
        if !self.measuring {
            let _ = b.card.set_attribute("hidden", "");
            return;
        }
        let _ = b.card.remove_attribute("hidden");
        let Some(source) = self.measure.source else {
            let _ = b.readings.set_attribute("hidden", "");
            set_text(
                &b.status,
                "Click a point to measure from · Esc: stop measuring",
            );
            return;
        };
        let _ = b.readings.remove_attribute("hidden");
        set_text(&b.source, &source.display());
        let reading = self.reading();
        set_text(
            &b.bearing,
            &reading
                .as_ref()
                .map_or_else(String::new, Reading::bearing_text),
        );
        set_text(
            &b.distance,
            &reading
                .as_ref()
                .map_or_else(String::new, Reading::distance_text),
        );
        set_text(
            &b.status,
            if self.measure.is_finished() {
                "Click to start a new measurement · Esc: stop measuring"
            } else {
                "Click to set the end point · Esc: stop measuring"
            },
        );
    }

    pub(super) fn copy_measurement(&self) {
        if let Some(r) = self.reading() {
            copy_to_clipboard(&r.clipboard_text(), &self.measure_box.copy);
        }
    }
}

/// The measurement info box.
pub(super) struct MeasureBox {
    pub(super) card: Element,
    pub(super) readings: Element,
    pub(super) tool: Element,
    pub(super) source: Element,
    pub(super) bearing: Element,
    pub(super) distance: Element,
    pub(super) status: Element,
    pub(super) copy: Element,
}
