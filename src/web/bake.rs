//! Baking the basemap (`?bake`, for tools/dev/browser/bake.js): the page
//! shows nothing but the terrain, and `window.ferryBake` frames one
//! basemap tile at a time for the script to save as an image.
//!
//! - `ferryBake.plan()`: the tiles to bake, as JSON `[[z, x, y], …]`
//! - `ferryBake.show(z, x, y)`: frame that tile (the page is the tile's
//!   size, at one pixel per CSS pixel)
//! - `ferryBake.ready()`: whether it's drawn, its terrain all in

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;

use super::App;
use crate::basemap::Grid;
use crate::terrain::TileKey;

pub(super) struct Bake {
    grid: Grid,
    /// The tile asked for is drawn with all its terrain.
    pub(super) ready: bool,
}

/// Whether the page's link asks to bake (`?bake`).
pub(super) fn asked(window: &web_sys::Window) -> bool {
    let search = window.location().search().unwrap_or_default();
    search
        .trim_start_matches('?')
        .split('&')
        .any(|p| p == "bake")
}

impl Bake {
    pub(super) fn new(grid: Grid) -> Self {
        Self { grid, ready: false }
    }
}

impl App {
    fn bake_show(&mut self, key: TileKey) {
        let Some(bake) = &mut self.bake else { return };
        let (min, max) = bake.grid.bounds(key);
        self.camera
            .show_square((min + max) / 2.0, bake.grid.tile_m(key.z) / 2.0);
        bake.ready = false;
        self.redraw = true;
    }
}

/// Puts `window.ferryBake` on the page, with `plan` the tiles to bake.
pub(super) fn expose(app: &Rc<RefCell<App>>, plan: &[TileKey]) {
    let json = format!(
        "[{}]",
        plan.iter()
            .map(|k| format!("[{},{},{}]", k.z, k.x, k.y))
            .collect::<Vec<_>>()
            .join(",")
    );
    let api = js_sys::Object::new();
    let plan = Closure::<dyn Fn() -> String>::new(move || json.clone());
    let shown = app.clone();
    let show = Closure::<dyn Fn(u8, u32, u32)>::new(move |z, x, y| {
        shown.borrow_mut().bake_show(TileKey { z, x, y });
    });
    let drawn = app.clone();
    let ready = Closure::<dyn Fn() -> bool>::new(move || {
        drawn.borrow().bake.as_ref().is_some_and(|b| b.ready)
    });
    for (name, f) in [
        ("plan", plan.into_js_value()),
        ("show", show.into_js_value()),
        ("ready", ready.into_js_value()),
    ] {
        let _ = js_sys::Reflect::set(&api, &name.into(), &f);
    }
    if let Some(window) = web_sys::window() {
        let _ = js_sys::Reflect::set(&window, &"ferryBake".into(), &api);
    }
}
