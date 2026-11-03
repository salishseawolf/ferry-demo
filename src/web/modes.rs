//! Starting the map in either mode (web/map.rs): the GPU's, or flat over
//! the prebaked tiles. At load, the GPU's if the browser has one it can
//! use; after that the debug panel's Mode switches between them in place,
//! keeping the view and everything else.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, HtmlInputElement, PointerEvent, WheelEvent, Window};

use super::App;
use super::TAB_RETURN_GRACE_MS;
use super::dom::{listen, page_hidden, sleep_ms};
use super::map::Map;
use crate::basemap::Grid;
use crate::flat::FlatRenderer;
use crate::geo::EARTH_RADIUS_M;
use crate::grid::KITSAP_SLICE;
use crate::renderer::Renderer;
use crate::terrain::Manifest;
use crate::tiles::{TerrainLoader, load_index};

pub(super) const POSITIONS: &str = "Ferry positions: Washington State Department of Transportation";

/// Where the GPU reports its device lost (see `App::frame`).
pub(super) type Lost = Arc<Mutex<Option<String>>>;

/// The GPU's map on `canvas`, with the elevation tiles there are and the
/// credits for them; or why there's none, as the page says it (the
/// details go to the console).
pub(super) async fn gpu_map(
    window: &Window,
    canvas: &HtmlCanvasElement,
    lost: &Lost,
) -> Result<(Map, Manifest, String), &'static str> {
    let renderer = start_gpu(window, canvas).await?;
    let lost = lost.clone();
    renderer.on_device_lost(move |message| {
        if let Ok(mut lost) = lost.lock() {
            *lost = Some(message);
        }
    });
    // Without tiles the slice still renders, as the plain backdrop.
    let (manifest, credits) = match load_index("elevation").await {
        Ok(index) => {
            log::info!("terrain: {} tiles available", index.manifest.len());
            let credits = format!("Elevation: {} · {POSITIONS}", index.attribution.join(" · "));
            (index.manifest, credits)
        }
        Err(e) => {
            log::warn!("no terrain tiles: {e}");
            let credits =
                "No elevation tiles: run <code>mise run tiles:elevation</code>, then rebuild.";
            (Manifest::new([], KITSAP_SLICE), credits.to_string())
        }
    };
    Ok((Map::Gpu(Box::new(renderer)), manifest, credits))
}

/// The flat map on `canvas`, over the prebaked tiles, with their credits.
/// Without them, the lines and symbols still draw.
pub(super) async fn flat_map(canvas: &HtmlCanvasElement) -> Result<(Map, String), String> {
    let (basemap, credits) = match load_index("basemap").await {
        Ok(index) => {
            let credits = format!("Elevation: {} · {POSITIONS}", index.attribution.join(" · "));
            (index.manifest, credits)
        }
        Err(e) => {
            log::warn!("no basemap: {e}");
            let credits = "No basemap: run <code>mise run tiles:basemap</code>.";
            (Manifest::new([], KITSAP_SLICE), credits.to_string())
        }
    };
    let grid = Grid::of(KITSAP_SLICE, EARTH_RADIUS_M);
    let flat = FlatRenderer::new(canvas.clone(), grid, basemap)?;
    Ok((Map::Flat(Box::new(flat)), credits))
}

/// The GPU's renderer, or why there's none.
async fn start_gpu(window: &Window, canvas: &HtmlCanvasElement) -> Result<Renderer, &'static str> {
    if !window.is_secure_context() {
        return Err("The map needs a secure connection: open this page over https.");
    }
    let has_webgpu = js_sys::Reflect::has(&window.navigator(), &"gpu".into()).unwrap_or(false);
    if !has_webgpu {
        return Err("The map needs WebGPU, which this browser doesn't offer or has turned off.");
    }
    // A page reloaded in a background tab (the dev server does this) waits
    // until it's shown, and a moment more, before touching the GPU.
    if page_hidden() {
        while page_hidden() {
            sleep_ms(100.0).await;
        }
        sleep_ms(TAB_RETURN_GRACE_MS).await;
    }
    Renderer::new(canvas.clone(), &KITSAP_SLICE)
        .await
        .map_err(|e| {
            log::warn!("{e}");
            "The map needs a GPU, and this browser found none it can draw with."
        })
}

/// Whether the page's link asks to start with the flat map though there
/// may be a GPU (`?flat`), to see it as browsers without one do.
pub(super) fn asks_for_flat_map(window: &Window) -> bool {
    let search = window.location().search().unwrap_or_default();
    search
        .trim_start_matches('?')
        .split('&')
        .any(|p| p == "flat")
}

/// A fresh copy of `canvas` in its place: a canvas keeps the first kind
/// of drawing context it gave out (WebGPU or 2D) for good.
pub(super) fn fresh_canvas(canvas: &HtmlCanvasElement) -> Result<HtmlCanvasElement, String> {
    let fresh: HtmlCanvasElement = canvas
        .clone_node()
        .ok()
        .and_then(|n| n.dyn_into().ok())
        .ok_or("can't copy the canvas")?;
    canvas
        .replace_with_with_node_1(&fresh)
        .map_err(|_| "can't replace the canvas")?;
    Ok(fresh)
}

/// The map's handlers on `canvas`: dragging, pinching, clicking, the wheel.
pub(super) fn listen_canvas(canvas: &HtmlCanvasElement, app: &Rc<RefCell<App>>) {
    listen(canvas, "pointerdown", app, |app, e: PointerEvent| {
        app.pointer_down(&e)
    });
    listen(canvas, "pointermove", app, |app, e: PointerEvent| {
        app.pointer_move(&e)
    });
    listen(canvas, "pointerup", app, |app, e: PointerEvent| {
        app.pointer_up(&e)
    });
    listen(canvas, "pointercancel", app, |app, e: PointerEvent| {
        app.pointer_cancel(&e)
    });
    listen(canvas, "pointerleave", app, |app, _: PointerEvent| {
        app.cursor = None;
        app.redraw = true;
    });
    listen(canvas, "wheel", app, |app, e: WheelEvent| app.wheel(&e));
}

/// Marks the page with the mode the map is in: `.flat-map` on the body
/// (index.html hides what only the GPU's map has), and the debug panel's
/// Mode checked to match.
pub(super) fn show_mode(flat: bool) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    if let Some(body) = document.body() {
        let _ = body.class_list().toggle_with_force("flat-map", flat);
    }
    let Ok(modes) = document.query_selector_all("input[name=mode]") else {
        return;
    };
    for i in 0..modes.length() {
        if let Some(input) = modes
            .item(i)
            .and_then(|n| n.dyn_into::<HtmlInputElement>().ok())
        {
            input.set_checked((input.value() == "flat") == flat);
        }
    }
}

/// Shows why the GPU's map couldn't start, with the button back to the
/// prebaked tiles, or hides that note (None).
pub(super) fn show_map_error(why: Option<&str>) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let (Some(note), Some(text)) = (
        document.get_element_by_id("map-error"),
        document.get_element_by_id("map-error-text"),
    ) else {
        return;
    };
    if let Some(why) = why {
        text.set_text_content(Some(&format!(
            "{why} Switch back to the prebaked tiles to keep using the map."
        )));
    }
    let _ = note.toggle_attribute_with_force("hidden", why.is_none());
}

/// Switches the map to the flat one or the GPU's, in place: a fresh
/// canvas, the new renderer on it, and the lines it was showing. If the
/// GPU's can't start, no map, and a note (#map-error) offering the
/// prebaked tiles. A switch made while another is starting overrides it:
/// the earlier one's outcome is dropped.
pub(super) fn switch(app: Rc<RefCell<App>>, flat: bool) {
    show_map_error(None);
    let (canvas, ticket) = {
        let mut a = app.borrow_mut();
        a.switches += 1;
        // The old device may report itself lost as it goes: to a place no
        // one looks any more.
        a.map_lost = Arc::default();
        a.renderer = None;
        a.press = None;
        a.cursor = None;
        match fresh_canvas(&a.canvas) {
            Ok(canvas) => (canvas, a.switches),
            Err(e) => {
                log::error!("{e}");
                return;
            }
        }
    };
    listen_canvas(&canvas, &app);
    app.borrow_mut().canvas = canvas.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let Some(window) = web_sys::window() else {
            return;
        };
        let current = |app: &Rc<RefCell<App>>| app.borrow().switches == ticket;
        let lost = app.borrow().map_lost.clone();
        let started = if flat {
            flat_map(&canvas).await.map(|(map, _)| (map, None))
        } else {
            match gpu_map(&window, &canvas, &lost).await {
                Ok((map, manifest, _)) => Ok((map, Some(manifest))),
                // Asked for by hand: say why, and leave going back to the
                // prebaked tiles to the reader rather than do it unasked.
                Err(why) if current(&app) => {
                    log::info!("no GPU map: {why}");
                    show_mode(false);
                    show_map_error(Some(why));
                    return;
                }
                Err(_) => return,
            }
        };
        if !current(&app) {
            return;
        }
        let mut a = app.borrow_mut();
        match started {
            Ok((map, manifest)) => {
                show_mode(matches!(map, Map::Flat(_)));
                a.renderer = Some(map);
                // A new GPU holds none of the old one's tiles.
                if let Some(manifest) = manifest {
                    a.terrain = TerrainLoader::new(manifest);
                }
                a.replay_overlays();
                a.redraw = true;
            }
            Err(e) => {
                log::warn!("{e}");
                a.lose_map("The map couldn't start again. Reload the page to try once more.");
            }
        }
    });
}
