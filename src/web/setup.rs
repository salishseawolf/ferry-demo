//! Building the app: the GPU, the page's elements and its event handlers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{
    Element, HtmlCanvasElement, HtmlInputElement, KeyboardEvent, PointerEvent, WheelEvent,
};

use crate::camera::{OrbitCamera, parse_view_fragment};
use crate::finder::{Finder, Sheet};
use crate::fleet::Fleet;
use crate::geo::{EARTH_RADIUS_M, LonLat};
use crate::grid::KITSAP_SLICE;
use crate::hexgrid::resolution_for;
use crate::live::{network_feed, notices_feed, sailings_feed, vessel_feed, vessel_info_feed};
use crate::measure::Measure;
use crate::perf::FrameStats;
use crate::renderer::Renderer;
use crate::terminals::Terminals;
use crate::terrain::Manifest;
use crate::tiles::{TerrainLoader, load_index};
use crate::view::STYLE_CARTOGRAPHIC;

use super::debug::Perf;
use super::dom::{listen, now_ms, page_hidden, show_error, sleep_ms};
use super::fetches::Fetches;
use super::hex::HexGrid;
use super::labels::Labels;
use super::measuring::MeasureBox;
use super::notes::Notes;
use super::{App, LAYER_TOGGLES, STYLE_CHOICES, TAB_RETURN_GRACE_MS, animate};

/// Start over Kitsap County, the slice's center, high enough to see most
/// of it.
const START_VIEW: LonLat = KITSAP_SLICE.center;
const START_ALTITUDE_RADII: f64 = 0.045;
/// Crossing times fetched for a route are kept this long (ms) before
/// they're asked for again; schedules (checked by the server every 10
/// minutes) this long.
const CROSSINGS_KEEP_MS: f64 = 5.0 * 60_000.0;
const SCHEDULES_KEEP_MS: f64 = 10.0 * 60_000.0;
/// A boat's latest loads change as it leaves a dock: kept a minute.
const VESSEL_LOADS_KEEP_MS: f64 = 60_000.0;

pub(super) async fn run() -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let element = |id: &str| {
        document
            .get_element_by_id(id)
            .ok_or_else(|| format!("missing #{id}"))
    };
    let canvas: HtmlCanvasElement = element("world")?
        .dyn_into()
        .map_err(|_| "#world is not a canvas")?;

    if !window.is_secure_context() {
        return Err(
            "WebGPU needs a secure context: open this page over https or on localhost.".into(),
        );
    }
    let has_webgpu = js_sys::Reflect::has(&window.navigator(), &"gpu".into()).unwrap_or(false);
    if !has_webgpu {
        return Err(
            "This browser has WebGPU disabled or unsupported (navigator.gpu is missing).".into(),
        );
    }

    // A page reloaded in a background tab (the dev server does this) waits
    // until it's shown, and a moment more, before touching the GPU.
    if page_hidden() {
        while page_hidden() {
            sleep_ms(100.0).await;
        }
        sleep_ms(TAB_RETURN_GRACE_MS).await;
    }
    let renderer = Renderer::new(canvas.clone(), &KITSAP_SLICE).await?;
    renderer.on_device_lost(|message| {
        log::error!("{message}");
        show_error(&format!("{message}. Reload the page to start again."));
    });
    // Without tiles the slice still renders, as the plain backdrop. The
    // route network's credits are added when it arrives.
    let credits = element("credits")?;
    let (manifest, credits_base) = match load_index("elevation").await {
        Ok(index) => {
            log::info!("terrain: {} tiles available", index.manifest.len());
            let base = format!(
                "Elevation: {} · Ferry positions: Washington State Department of Transportation",
                index.attribution.join(" · ")
            );
            (index.manifest, base)
        }
        Err(e) => {
            log::warn!("no terrain tiles: {e}");
            let base =
                "No elevation tiles: run <code>mise run tiles:elevation</code>, then rebuild."
                    .to_string();
            (Manifest::new([], KITSAP_SLICE), base)
        }
    };
    credits.set_inner_html(&credits_base);
    let aspect = canvas.client_width().max(1) as f64 / canvas.client_height().max(1) as f64;
    let app = Rc::new(RefCell::new(App {
        canvas: canvas.clone(),
        renderer,
        camera: start_camera(aspect),
        measure: Measure::default(),
        measuring: false,
        measure_end: None,
        measure_box: MeasureBox {
            card: element("measure")?,
            readings: element("measure-readings")?,
            tool: element("measure-tool")?,
            source: element("measure-source")?,
            bearing: element("measure-bearing")?,
            distance: element("measure-distance")?,
            status: element("measure-status")?,
            copy: element("measure-copy")?,
        },
        hex: HexGrid {
            built_for: None,
            built_m_per_px: 0.0,
            resolution: resolution_for(f64::MAX),
            cells: 0,
        },
        layers: 0,
        style: STYLE_CARTOGRAPHIC,
        cursor: None,
        press: None,
        legend: element("legend")?,
        settings: element("settings")?,
        settings_toggle: element("settings-toggle")?,
        perf: Perf {
            stats: FrameStats::default(),
            card: element("perf")?,
            body: element("perf-body")?,
            copy: element("perf-copy")?,
            visible: false,
            shown_at: 0.0,
        },
        terrain: TerrainLoader::new(manifest),
        resume_at_ms: 0.0,
        redraw: true,
        last_draw_ms: 0.0,
        idle: false,
        network_feed: network_feed(),
        vessel_info_feed: vessel_info_feed(),
        vessel_feed: vessel_feed(),
        sailings_feed: sailings_feed(),
        notices_feed: notices_feed(),
        live_vessels: None,
        live_sailings: None,
        vessel_names: HashMap::new(),
        credits,
        credits_base,
        crossings: Fetches::new(CROSSINGS_KEEP_MS),
        schedules: Fetches::new(SCHEDULES_KEEP_MS),
        sailings: Fetches::new(VESSEL_LOADS_KEEP_MS),
        older_sailings: Fetches::new(SCHEDULES_KEEP_MS),
        older: None,
        route_loads: Fetches::new(CROSSINGS_KEEP_MS),
        vessel_loads: Fetches::new(VESSEL_LOADS_KEEP_MS),
        advice: Fetches::new(VESSEL_LOADS_KEEP_MS),
        advice_check: Fetches::new(SCHEDULES_KEEP_MS),
        fleet: Fleet::default(),
        terminals: Terminals::default(),
        dot_slots: HashMap::new(),
        labels: Labels {
            root: element("labels")?,
            spans: HashMap::new(),
            leaders: HashMap::new(),
            sizes: HashMap::new(),
            slots: HashMap::new(),
            debug: None,
        },
        notes: Notes {
            root: element("notes")?,
            items: HashMap::new(),
            open: None,
        },
        routes: Vec::new(),
        legs: Vec::new(),
        focus_points: Vec::new(),
        pointed: None,
        pending_click: None,
        flight: None,
        finder: Finder::new(
            element("finder-input")?
                .dyn_into()
                .map_err(|_| "#finder-input is not an input")?,
            element("finder-list")?,
            element("finder-page")?,
        ),
        finder_stale: false,
        service_routes: Vec::new(),
        route_ids: Vec::new(),
        fleet_status: "Loading ferry positions…".into(),
        vessels_due_ms: f64::INFINITY,
        vessels_step_ms: f64::INFINITY,
        hud_vessel: element("hud-vessel")?,
    }));
    log::info!("renderer ready");

    listen(&canvas, "pointerdown", &app, |app, e: PointerEvent| {
        app.pointer_down(&e)
    });
    listen(&canvas, "pointermove", &app, |app, e: PointerEvent| {
        app.pointer_move(&e)
    });
    listen(&canvas, "pointerup", &app, |app, e: PointerEvent| {
        app.pointer_up(&e)
    });
    listen(&canvas, "pointerleave", &app, |app, _: PointerEvent| {
        app.cursor = None;
        app.redraw = true;
    });
    listen(&canvas, "wheel", &app, |app, e: WheelEvent| app.wheel(&e));
    // A click on a notice dot opens or closes its note.
    let notes = element("notes")?;
    listen(&notes, "click", &app, |app, e: web_sys::Event| {
        let dot = e
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|el| el.closest(".dot").ok().flatten());
        let key = dot
            .and_then(|d| d.parent_element())
            .and_then(|note| note.get_attribute("data-key"));
        if let Some(key) = key {
            app.notes.toggle(&key);
        }
    });
    listen(&window, "keydown", &app, |app, e: KeyboardEvent| {
        app.key_down(&e)
    });
    listen(
        &document,
        "visibilitychange",
        &app,
        |app, _: web_sys::Event| {
            if !page_hidden() {
                app.resume_at_ms = now_ms() + TAB_RETURN_GRACE_MS;
                app.redraw = true;
            }
        },
    );

    // The checkboxes' `checked` attributes in index.html set the defaults.
    for (id, bit) in LAYER_TOGGLES {
        let input: HtmlInputElement = element(id)?
            .dyn_into()
            .map_err(|_| format!("#{id} is not an input"))?;
        app.borrow_mut().set_layer(bit, input.checked());
        listen(
            &input.clone(),
            "change",
            &app,
            move |app, _: web_sys::Event| app.set_layer(bit, input.checked()),
        );
    }
    // The map style's radio buttons (index.html, their values in
    // STYLE_CHOICES); the one checked there is the default.
    let style_of = |value: String| {
        STYLE_CHOICES
            .iter()
            .find(|(v, _)| *v == value)
            .map_or(STYLE_CARTOGRAPHIC, |&(_, style)| style)
    };
    let styles = document
        .query_selector_all("input[name=style]")
        .map_err(|_| "no style choices")?;
    for i in 0..styles.length() {
        let Some(input) = styles
            .item(i)
            .and_then(|n| n.dyn_into::<HtmlInputElement>().ok())
        else {
            continue;
        };
        if input.checked() {
            app.borrow_mut().set_style(style_of(input.value()));
        }
        listen(
            &input.clone(),
            "change",
            &app,
            move |app, _: web_sys::Event| {
                if input.checked() {
                    app.set_style(style_of(input.value()));
                }
            },
        );
    }
    let debug_toggle: HtmlInputElement = element("debug")?
        .dyn_into()
        .map_err(|_| "#debug is not an input")?;
    app.borrow_mut().set_debug(debug_toggle.checked());
    listen(
        &debug_toggle.clone(),
        "change",
        &app,
        move |app, _: web_sys::Event| app.set_debug(debug_toggle.checked()),
    );
    let label_toggle: HtmlInputElement = element("debug-labels")?
        .dyn_into()
        .map_err(|_| "#debug-labels is not an input")?;
    app.borrow_mut().set_label_debug(label_toggle.checked());
    listen(
        &label_toggle.clone(),
        "change",
        &app,
        move |app, _: web_sys::Event| app.set_label_debug(label_toggle.checked()),
    );
    let finder_input = element("finder-input")?;
    listen(&finder_input, "input", &app, |app, _: web_sys::Event| {
        app.search()
    });
    listen(&finder_input, "keydown", &app, |app, e: KeyboardEvent| {
        app.search_key(&e)
    });
    // Searching on a phone takes the whole sheet, clear of the keyboard.
    listen(&finder_input, "focus", &app, |app, _: web_sys::Event| {
        app.finder.set_open(true);
        app.finder.set_sheet(Sheet::Full);
        app.set_settings_open(false);
    });
    app.borrow_mut().finder.set_sheet(Sheet::Half);
    // A press anywhere outside the finder folds its matches up (leaving a
    // phone's sheet at a peek), and one outside the settings closes them.
    listen(&document, "pointerdown", &app, |app, e: web_sys::Event| {
        let target = e.target().and_then(|t| t.dyn_into::<Element>().ok());
        let within = |selector: &str| {
            target
                .as_ref()
                .is_some_and(|el| el.closest(selector).ok().flatten().is_some())
        };
        if !within("#finder") {
            app.finder.set_open(false);
            app.finder.set_sheet(Sheet::Peek);
        }
        if !within("#settings") && !within("#settings-toggle") {
            app.set_settings_open(false);
        }
    });
    let handle = element("finder-handle")?;
    listen(&handle, "click", &app, |app, _: web_sys::Event| {
        app.finder.cycle_sheet()
    });
    // The pointer resting on a match opens it after a moment; a click on
    // its name opens it at once.
    let finder_list = element("finder-list")?;
    let index_of = |e: &web_sys::Event| {
        e.target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|el| el.closest(".hit").ok().flatten())
            .and_then(|hit| hit.get_attribute("data-index"))
            .and_then(|i| i.parse::<usize>().ok())
    };
    listen(
        &finder_list,
        "mouseover",
        &app,
        move |app, e: web_sys::Event| app.finder.rest(index_of(&e), now_ms()),
    );
    listen(
        &finder_list,
        "mouseleave",
        &app,
        |app, _: web_sys::Event| app.finder.rest(None, now_ms()),
    );
    listen(
        &finder_list,
        "click",
        &app,
        move |app, e: web_sys::Event| {
            let clicked = e.target().and_then(|t| t.dyn_into::<Element>().ok());
            if let Some(button) =
                (clicked.as_ref()).and_then(|el| el.closest(".older-link").ok().flatten())
            {
                app.open_older(&button);
                return;
            }
            // Other clicks within an open match's details (scrolling
            // notices) stay.
            let on_row = (clicked.as_ref())
                .and_then(|el| el.closest(".row").ok().flatten())
                .is_some();
            if !on_row {
                return;
            }
            app.finder.set_sheet(Sheet::Half);
            if let Some(i) = index_of(&e)
                && app.finder.select(i)
            {
                app.selection_changed();
            }
        },
    );
    // The page of older sailings: back, more, and Escape.
    listen(
        &element("finder-page-back")?.into(),
        "click",
        &app,
        |app, _: web_sys::Event| app.close_older(),
    );
    listen(
        &element("finder-page-more")?.into(),
        "click",
        &app,
        |app, _: web_sys::Event| app.more_older(),
    );
    listen(
        &element("finder-page")?.into(),
        "keydown",
        &app,
        |app, e: KeyboardEvent| {
            if e.key() == "Escape" {
                app.close_older();
            }
        },
    );
    // The map settings, with the key, open from the button beside the
    // search.
    // On phones the credits show two lines; a tap shows them all.
    let credits = element("credits")?;
    listen(
        &credits.clone(),
        "click",
        &app,
        move |_, _: web_sys::Event| {
            let _ = credits.class_list().toggle("open");
        },
    );
    let settings_toggle = element("settings-toggle")?;
    listen(&settings_toggle, "click", &app, |app, _: web_sys::Event| {
        app.set_settings_open(app.settings.has_attribute("hidden"))
    });
    let tool = element("measure-tool")?;
    listen(&tool, "click", &app, |app, _: web_sys::Event| {
        app.set_measuring(!app.measuring)
    });
    let copy = element("measure-copy")?;
    listen(&copy, "click", &app, |app, _: web_sys::Event| {
        app.copy_measurement()
    });
    let perf_copy = element("perf-copy")?;
    listen(&perf_copy, "click", &app, |app, _: web_sys::Event| {
        app.copy_perf()
    });

    animate(app);
    Ok(())
}

/// The camera to start with: the view in the page's link (`#at=…`, the
/// debug panel's link), or over the slice.
fn start_camera(aspect: f64) -> OrbitCamera {
    let linked = web_sys::window()
        .and_then(|w| w.location().hash().ok())
        .and_then(|hash| parse_view_fragment(&hash, EARTH_RADIUS_M));
    match linked {
        Some((center, altitude)) => {
            OrbitCamera::over(EARTH_RADIUS_M, center, altitude / EARTH_RADIUS_M, aspect)
        }
        None => OrbitCamera::over(EARTH_RADIUS_M, START_VIEW, START_ALTITUDE_RADII, aspect),
    }
}
