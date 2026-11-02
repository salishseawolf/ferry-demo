//! The pane in the map's place when there's no map (#no-map): why, and
//! the boats by route (crate::no_map), kept up to date as they report.

use web_sys::Element;

use super::dom::clock;
use crate::fleet::Fleet;
use crate::routes::ServiceRoute;

pub(super) struct NoMap {
    pub(super) root: Element,
    pub(super) why: Element,
    pub(super) list: Element,
    /// The list last built, to leave it be when unchanged.
    pub(super) built: String,
}

impl NoMap {
    /// Shows the pane, saying why there's no map; `.no-map` on the body
    /// hides the canvas and what only works over it.
    pub(super) fn show(&self, why: &str) {
        if let Some(body) = self.root.owner_document().and_then(|d| d.body()) {
            let _ = body.class_list().add_1("no-map");
        }
        self.why.set_text_content(Some(why));
        let _ = self.root.remove_attribute("hidden");
    }

    pub(super) fn show_list(&mut self, fleet: &Fleet, routes: &[ServiceRoute]) {
        let html = if fleet.vessels.is_empty() {
            "<p class=\"none\">Loading ferry positions…</p>".to_string()
        } else {
            crate::no_map::html(fleet, routes, js_sys::Date::now(), clock)
        };
        if html != self.built {
            self.list.set_inner_html(&html);
            self.built = html;
        }
    }
}
