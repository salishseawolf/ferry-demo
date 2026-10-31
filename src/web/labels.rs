//! Placing the map's labels clear of each other, of the panels and of the
//! notice dots.

use std::collections::HashMap;

use web_sys::Element;

use crate::placement::{self, Rect, Side, Wanted};

use super::LABEL_GAP_PX;

/// A name to set beside its boat or terminal's symbol (`symbol`, its box
/// on screen in CSS px), on the side it would rather be (see placement).
/// `key` names the symbol across frames; `class` styles it (index.html);
/// names with a lower `rank` are placed first.
pub(super) struct Label {
    pub(super) key: String,
    pub(super) text: String,
    pub(super) symbol: Rect,
    pub(super) prefer: Side,
    pub(super) rank: u8,
    pub(super) class: &'static str,
    /// Faded back while something else is highlighted.
    pub(super) dim: bool,
    /// Its symbol is highlighted: the name is too.
    pub(super) lit: bool,
}

/// The names over the map (#labels), one element per boat or terminal,
/// with a leader line for each name away from its first choice of place;
/// kept between frames and moved as the map redraws.
pub(super) struct Labels {
    pub(super) root: Element,
    pub(super) spans: HashMap<String, Element>,
    pub(super) leaders: HashMap<String, Element>,
    /// Each name's size on screen (CSS px), measured once for its text
    /// and style.
    pub(super) sizes: HashMap<String, (String, (f64, f64))>,
    /// Each name's slot last frame, which it keeps while it can.
    pub(super) slots: HashMap<String, usize>,
    /// Draws the symbols' boxes, the names' slots and what they avoid
    /// (the debug switch), in #labels-debug.
    pub(super) debug: Option<Element>,
}

impl Labels {
    /// Shows `labels` beside their symbols, clear of `obstacles` and each
    /// other where they can be, and hides the rest. `dots` are the notice
    /// dots' slots and the one each took, for the debug drawing.
    pub(super) fn place(
        &mut self,
        labels: &[Label],
        obstacles: &[Rect],
        dots: &[(Vec<Rect>, usize)],
    ) {
        let document = self.root.owner_document().expect("document");
        let mut order: Vec<&Label> = labels.iter().collect();
        order.sort_by(|a, b| (a.rank, &a.key).cmp(&(b.rank, &b.key)));
        // Each name's element and size: measured when its text or style
        // changes (only then does it force a layout).
        let mut sizes = Vec::with_capacity(order.len());
        for label in &order {
            let span = self.spans.entry(label.key.clone()).or_insert_with(|| {
                let span = document.create_element("span").expect("span");
                let _ = self.root.append_child(&span);
                span
            });
            let style = format!("{}|{}", label.class, label.text);
            let size = match self.sizes.get(&label.key) {
                Some((had, size)) if *had == style => *size,
                _ => {
                    span.set_text_content(Some(&label.text));
                    span.set_class_name(label.class);
                    let _ = span.remove_attribute("hidden");
                    let r = span.get_bounding_client_rect();
                    let size = (r.width(), r.height());
                    self.sizes.insert(label.key.clone(), (style, size));
                    size
                }
            };
            sizes.push(size);
        }
        let wanted: Vec<Wanted> = order
            .iter()
            .zip(&sizes)
            .map(|(label, &size)| Wanted {
                key: &label.key,
                size,
                symbol: label.symbol,
                prefer: label.prefer,
            })
            .collect();
        let placed = placement::place(&wanted, obstacles, &self.slots, LABEL_GAP_PX);
        self.slots.clear();
        for (label, at) in order.iter().zip(&placed) {
            self.slots.insert(label.key.clone(), at.slot);
            let span = &self.spans[&label.key];
            span.set_class_name(label.class);
            let _ = span.class_list().toggle_with_force("dim", label.dim);
            let _ = span.class_list().toggle_with_force("lit", label.lit);
            let _ = span.set_attribute(
                "style",
                &format!(
                    "transform: translate({:.1}px, {:.1}px)",
                    at.rect.x0, at.rect.y0
                ),
            );
            let _ = span.remove_attribute("hidden");
            let leader = self.leaders.entry(label.key.clone()).or_insert_with(|| {
                let line = document.create_element("i").expect("i");
                line.set_class_name("leader");
                let _ = self.root.append_child(&line);
                line
            });
            match at.leader {
                Some([(x0, y0), (x1, y1)]) => {
                    let (length, angle) = ((x1 - x0).hypot(y1 - y0), (y1 - y0).atan2(x1 - x0));
                    let _ = leader.set_attribute(
                        "style",
                        &format!(
                            "width: {length:.1}px; transform: translate({x0:.1}px, {y0:.1}px) rotate({angle:.4}rad)"
                        ),
                    );
                    let _ = leader.class_list().toggle_with_force("dim", label.dim);
                    let _ = leader.class_list().toggle_with_force("lit", label.lit);
                    let _ = leader.remove_attribute("hidden");
                }
                None => {
                    let _ = leader.set_attribute("hidden", "");
                }
            }
        }
        let shown = |key: &String| labels.iter().any(|l| l.key == *key);
        for (key, el) in self.spans.iter().chain(&self.leaders) {
            if !shown(key) {
                let _ = el.set_attribute("hidden", "");
            }
        }
        if let Some(debug) = &self.debug {
            debug.set_text_content(None);
            let add = |r: &Rect, class: &str, text: &str| {
                let el = document.create_element("div").expect("div");
                el.set_class_name(class);
                el.set_text_content((!text.is_empty()).then_some(text));
                let _ = el.set_attribute(
                    "style",
                    &format!(
                        "width: {:.1}px; height: {:.1}px; transform: translate({:.1}px, {:.1}px)",
                        r.x1 - r.x0,
                        r.y1 - r.y0,
                        r.x0,
                        r.y0
                    ),
                );
                let _ = debug.append_child(&el);
            };
            for o in obstacles {
                add(o, "obstacle", "");
            }
            for (w, at) in wanted.iter().zip(&placed) {
                add(&w.symbol, "symbol", "");
                for slot in placement::slots(&w.symbol, w.size, w.prefer, LABEL_GAP_PX) {
                    add(&slot, "slot", "");
                }
                add(&at.rect, "chosen", &at.slot.to_string());
            }
            for (slots, taken) in dots {
                for (k, slot) in slots.iter().enumerate() {
                    let (class, text) = if k == *taken {
                        ("chosen", k.to_string())
                    } else {
                        ("slot", String::new())
                    };
                    add(slot, class, &text);
                }
            }
        }
    }

    /// Shows or hides the debug drawing.
    pub(super) fn set_debug(&mut self, on: bool) {
        let document = self.root.owner_document().expect("document");
        match (&self.debug, on) {
            (None, true) => {
                let el = document.create_element("div").expect("div");
                el.set_id("labels-debug");
                let _ = self.root.append_child(&el);
                self.debug = Some(el);
            }
            (Some(el), false) => {
                el.remove();
                self.debug = None;
            }
            _ => {}
        }
    }
}
