//! Building the app: the GPU (if the browser has one it can draw with),
//! the page's elements and its event handlers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlCanvasElement, HtmlInputElement, KeyboardEvent};

use crate::basemap::Grid;
use crate::camera::{MapCamera, parse_view_fragment};
use crate::finder::{Finder, Sheet, Target};
use crate::fleet::Fleet;
use crate::geo::{EARTH_RADIUS_M, LonLat};
use crate::grid::KITSAP_SLICE;
use crate::hexgrid::resolution_for;
use crate::live::{network_feed, notices_feed, sailings_feed, vessel_feed, vessel_info_feed};
use crate::measure::Measure;
use crate::perf::FrameStats;
use crate::terminals::Terminals;
use crate::terrain::Manifest;
use crate::tiles::TerrainLoader;
use crate::view::STYLE_CARTOGRAPHIC;

use super::bake::{self, Bake};
use super::debug::Perf;
use super::dom::{listen, now_ms, page_hidden};
use super::fetches::Fetches;
use super::hex::HexGrid;
use super::labels::Labels;
use super::map::Map;
use super::measuring::MeasureBox;
use super::modes;
use super::no_map::NoMap;
use super::notes::Notes;
use super::{App, LAYER_TOGGLES, STYLE_CHOICES, TAB_RETURN_GRACE_MS, animate};

/// Start over Kitsap County, the slice's center, high enough to see most
/// of it.
const START_VIEW: LonLat = KITSAP_SLICE.center;
const START_ALTITUDE_M: f64 = 0.045 * EARTH_RADIUS_M;
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
    let mut canvas: HtmlCanvasElement = element("world")?
        .dyn_into()
        .map_err(|_| "#world is not a canvas")?;

    // The GPU's map if it can, else the flat one over the prebaked tiles,
    // else none (the list of boats in its place). The route network's
    // credits are added when it arrives.
    let map_lost = modes::Lost::default();
    let baking = bake::asked(&window);
    let gpu = if modes::asks_for_flat_map(&window) && !baking {
        Err("the page asked for the flat map (?flat)")
    } else {
        modes::gpu_map(&window, &canvas, &map_lost).await
    };
    let empty = || Manifest::new([], KITSAP_SLICE);
    let (renderer, manifest, credits_base, why_no_map) = match gpu {
        Ok((map, manifest, credits)) => (Some(map), manifest, credits, None),
        Err(why) => {
            log::info!("no GPU map ({why}): drawing the prebaked tiles");
            // The GPU may have taken this canvas's context first.
            canvas = modes::fresh_canvas(&canvas)?;
            match modes::flat_map(&canvas).await {
                Ok((map, credits)) => (Some(map), empty(), credits, None),
                Err(e) => {
                    log::warn!("{e}");
                    (None, empty(), modes::POSITIONS.to_string(), Some(why))
                }
            }
        }
    };
    modes::show_mode(matches!(renderer, Some(Map::Flat(_))));
    // The basemap tiles there's terrain for, if baking.
    let bake_plan = match baking {
        true => Grid::of(KITSAP_SLICE, EARTH_RADIUS_M).plan(&manifest),
        false => Vec::new(),
    };
    let credits = element("credits")?;
    credits.set_inner_html(&credits_base);
    let aspect = canvas.client_width().max(1) as f64 / canvas.client_height().max(1) as f64;
    let app = Rc::new(RefCell::new(App {
        canvas: canvas.clone(),
        renderer,
        map_lost,
        overlays: Vec::new(),
        switches: 0,
        no_map: NoMap {
            root: element("no-map")?,
            why: element("no-map-why")?,
            list: element("no-map-list")?,
            built: String::new(),
        },
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
        touch: false,
        empty_tap: None,
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
        bake: baking.then(|| Bake::new(Grid::of(KITSAP_SLICE, EARTH_RADIUS_M))),
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
    match why_no_map {
        None => log::info!("renderer ready"),
        Some(why) => app.borrow_mut().lose_map(why),
    }

    modes::listen_canvas(&canvas, &app);
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
    // Baking: the terrain alone, nothing over it, and the script's handle.
    if baking {
        set_body_class("baking");
        app.borrow_mut().layers = 0;
        bake::expose(&app, &bake_plan);
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
    // The debug panel's mode: the one the map is drawn in (`show_mode`),
    // and the other a click away.
    let modes = document
        .query_selector_all("input[name=mode]")
        .map_err(|_| "no mode choices")?;
    for i in 0..modes.length() {
        let Some(input) = modes
            .item(i)
            .and_then(|n| n.dyn_into::<HtmlInputElement>().ok())
        else {
            continue;
        };
        let app = app.clone();
        let chosen = input.clone();
        let closure = Closure::<dyn FnMut()>::new(move || {
            if chosen.checked() {
                modes::switch(app.clone(), chosen.value() == "flat");
            }
        });
        let _ = input.add_event_listener_with_callback("change", closure.as_ref().unchecked_ref());
        closure.forget();
    }
    // The note when the GPU's map couldn't start: back to the prebaked
    // tiles.
    let back = element("map-error-switch")?;
    let app_back = app.clone();
    let closure = Closure::<dyn FnMut()>::new(move || modes::switch(app_back.clone(), true));
    let _ = back.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref());
    closure.forget();
    // A boat or route in the list in the map's place comes up in the
    // finder, as a click on it on the map would.
    let no_map_list = element("no-map-list")?;
    listen(&no_map_list, "click", &app, |app, e: web_sys::Event| {
        let button = e
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|el| el.closest("button").ok().flatten());
        let number = |name: &str| {
            (button.as_ref())
                .and_then(|b| b.get_attribute(name))
                .and_then(|v| v.parse::<u32>().ok())
        };
        if let Some(id) = number("data-vessel") {
            app.find(Target::Vessel(id));
        } else if let Some(r) = number("data-route") {
            app.find(Target::Route(r as usize));
        }
    });
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

fn set_body_class(class: &str) {
    if let Some(body) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
    {
        let _ = body.class_list().add_1(class);
    }
}

/// The camera to start with: the view in the page's link (`#at=…`, the
/// debug panel's link), or over the slice.
fn start_camera(aspect: f64) -> MapCamera {
    let linked = web_sys::window()
        .and_then(|w| w.location().hash().ok())
        .and_then(|hash| parse_view_fragment(&hash));
    let (center, altitude) = linked.unwrap_or((START_VIEW, START_ALTITUDE_M));
    MapCamera::new(EARTH_RADIUS_M, KITSAP_SLICE, center, altitude, aspect)
}
