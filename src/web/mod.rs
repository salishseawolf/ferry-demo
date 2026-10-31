//! The app in the browser: the globe, its ferries and the panels over
//! it, drawn each frame by `App`.

mod debug;
mod dom;
mod ferries;
mod fetches;
mod fit;
mod hex;
mod labels;
mod measuring;
mod notes;
mod picking;
mod setup;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlCanvasElement, KeyboardEvent, PointerEvent, WheelEvent};

use crate::camera::{OrbitCamera, ray_sphere};
use crate::finder::{self, Finder, Loaded, Scope, Sheet, Target};
use crate::fleet::{Fleet, Vessel, dimensions_from, vessels_from};
use crate::geo::{EARTH_RADIUS_M, LonLat};
use crate::grid::{KITSAP_SLICE, h3_cell};
use crate::hexgrid::boundary;
use crate::legend;
use crate::live::{Feed, Update, VESSELS_MS};
use crate::measure::Measure;
use crate::palette;
use crate::renderer::{Globals, Renderer};
use crate::routes::{self, Leg, ServiceRoute};
use crate::scene::{HOVER_CELL_COLOR, Scene};
use crate::terminals::{Terminals, bulletins_from, sailings_from};
use crate::terrain::GlobeView;
use crate::tiles::TerrainLoader;
use crate::view::{LAYER_CELLS, LAYER_FERRIES, STYLE_CARTOGRAPHIC, STYLE_HYPSOMETRIC};
use ferry_model::api::{
    AdviceCheck, CrossingStats, LiveNotices, LiveSailings, LiveVessels, LoadStats, Network,
    RouteSchedule, TerminalAdvice, VesselLoads, Vessels,
};
use glam::DVec3;

use debug::Perf;
use dom::{clock, date_clock, now_ms, page_hidden, request_frame, show_error};
use fetches::Fetches;
use fit::Flight;
use hex::HexGrid;
use labels::Labels;
use measuring::MeasureBox;
use notes::{Notes, html_escape};
use picking::RouteLine;
use setup::run;

/// A press that moves less than this (CSS px) is a click, not a drag.
const CLICK_SLOP_PX: f64 = 4.0;
const LINE_WIDTH_CSS_PX: f64 = 2.0;
/// Layer tray checkboxes and the layer bit each controls.
const LAYER_TOGGLES: [(&str, u32); 2] = [
    ("layer-ferries", LAYER_FERRIES),
    ("layer-cells", LAYER_CELLS),
];
/// The map style choices, by their radio buttons' `value` in index.html.
const STYLE_CHOICES: [(&str, u32); 2] = [
    ("cartographic", STYLE_CARTOGRAPHIC),
    ("hypsometric", STYLE_HYPSOMETRIC),
];
/// Terrain relief is drawn at this vertical exaggeration.
const RELIEF: f64 = 2.0;
/// How often the performance panel refreshes (ms); updating the page every
/// frame would itself cost time.
const PERF_REFRESH_MS: f64 = 250.0;
/// The map counts as idle after this long (ms) without a redraw.
const IDLE_AFTER_MS: f64 = 500.0;
/// After the tab becomes visible, wait this long (ms) before creating the
/// GPU device (a page reloaded in the background) or drawing again. The
/// browser is rebuilding canvases then (the old page's, our own), and
/// Firefox crashed when our swapchain was created in the same moment.
const TAB_RETURN_GRACE_MS: f64 = 300.0;
/// An outline is at most this share of the hull's beam.
const VESSEL_LINE_OF_BEAM: f64 = 0.3;
const VESSEL_HOVER_LINE_PX: f64 = 2.5;
const VESSEL_PICK_PX: f64 = 14.0;
/// Names show when the map is at least this detailed (ground meters per
/// CSS px), `LABEL_GAP_PX` from their symbol's box (placement.rs picks
/// the side and slot).
const LABEL_MAX_M_PER_PX: f64 = 150.0;
const LABEL_GAP_PX: f64 = 6.0;
/// Terminal anchors: their height on the ground (m), to scale with the
/// terrain like the hulls at full size (shrinking as the view zooms in:
/// terminals::symbol_scale), and how far inland of the slip they stand:
/// `ANCHOR_INLAND` anchor heights, plus `ANCHOR_CLEAR_M` on the ground to
/// clear the boats at the dock (up to about 140 m long) when the anchor
/// is small. Names show from a wider view than
/// boats', and a hovered terminal lists this many upcoming sailings.
const ANCHOR_M: f64 = 720.0;
const ANCHOR_INLAND: f64 = 0.9;
const ANCHOR_CLEAR_M: f64 = 150.0;
/// An anchor with its gauge spans half its height either side, and from
/// 0.535 of its height above its center to 0.92 below (markers.wgsl): a box
/// this many heights across and tall either side of a center this far
/// below the anchor's.
const ANCHOR_HALF_WIDTH: f64 = 0.5;
const ANCHOR_HALF_HEIGHT: f64 = 0.73;
const ANCHOR_DROP: f64 = 0.19;
const TERMINAL_LABEL_MAX_M_PER_PX: f64 = 300.0;
const TERMINAL_SAILINGS_SHOWN: usize = 4;
/// Notice dots (#notes) show when the map is at least this detailed
/// (ground meters per CSS px), overlapping their symbol like a badge, on
/// the side of it with the most room (placement::place_badges): at a
/// terminal's anchor, by a corner (above it raised by `NOTE_RAISE` of
/// their size) or a side; at a boat, either side of the hull
/// `BOAT_NOTE_REACH` of the way from its middle to each end, or just past
/// an end, however it's turned. Like the symbols they're to scale with the
/// terrain: a terminal's `NOTE_M` across on the ground, a boat's
/// `BOAT_NOTE_SHARE` of its length. A note's panel opens away from the window's edge it
/// would run past: leftward within `NOTE_FLIP_PX` of the right, upward in
/// the lower part of the window.
const NOTE_MAX_M_PER_PX: f64 = TERMINAL_LABEL_MAX_M_PER_PX;
const NOTE_M: f64 = 360.0;
const BOAT_NOTE_SHARE: f64 = 0.3;
const BOAT_NOTE_REACH: f64 = 0.7;
const NOTE_RAISE: f64 = 0.3;
const NOTE_FLIP_PX: f64 = 330.0;
const NOTE_UP_FRACTION: f64 = 0.55;
/// Clicking something highlighted flies the view, over `FLIGHT_MS`, to
/// fit everything highlighted in `FIT_SHARE` of the canvas's width or
/// height, whichever binds. Each symbol counts with its size on the
/// ground; the fit settles within `FIT_ITERATIONS` refinements.
const FLIGHT_MS: f64 = 700.0;
const FIT_SHARE: f64 = 0.9;
const FIT_ITERATIONS: usize = 12;
/// Ferry routes: faint dashed ink, `ROUTE_DASH_PX` CSS px per dash and gap.
const ROUTE_ALPHA: f32 = 0.45;
const ROUTE_WIDTH: f32 = 0.75;
const ROUTE_DASH_PX: f32 = 7.0;
/// Highlighting a route or a boat: the cursor picks a route's leg within
/// `ROUTE_PICK_PX` of its line, and the whole route lights up. Highlighted routes glow (two soft passes
/// of the hover color, in standard line widths) under darker dashes, and
/// highlighted boats and terminals get a halo. Hovering a route dims what
/// isn't on it to `DIM_ALPHA`.
const ROUTE_PICK_PX: f64 = 6.0;
const ROUTE_GLOW: [(f32, f32); 2] = [(5.0, 0.18), (2.5, 0.35)];
/// On a route of several legs, the leg under the cursor glows brighter.
const LEG_GLOW: (f32, f32) = (3.5, 0.3);
const ROUTE_HIGHLIGHT_ALPHA: f32 = 0.85;
const SYMBOL_GLOW_ALPHA: f32 = 0.7;
const DIM_ALPHA: f32 = 0.8;
/// The sun, fixed over the slice: from the northwest, 45° up, the usual
/// light for hillshaded relief (slopes facing it read as raised).
const SUN_AZIMUTH_DEG: f64 = 315.0;
const SUN_ELEVATION_DEG: f64 = 45.0;
type FrameCallback = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = run().await {
            log::error!("{e}");
            show_error(&e);
        }
    });
}

struct Press {
    start: (f64, f64),
    last: (f64, f64),
    dragging: bool,
    /// Surface point (unit vector) under the cursor at press time; dragging
    /// keeps it under the cursor. None if the press started off the planet.
    grabbed: Option<DVec3>,
}

struct App {
    canvas: HtmlCanvasElement,
    renderer: Renderer,
    camera: OrbitCamera,
    measure: Measure,
    /// Last on-planet cursor position while measuring, so the box keeps
    /// the live reading when the cursor leaves to click the copy button.
    measure_end: Option<LonLat>,
    measure_box: MeasureBox,
    /// Whether the measuring tool is on: clicks then set points to
    /// measure between, rather than fly to what's highlighted.
    measuring: bool,
    hex: HexGrid,
    /// Visible `LAYER_*` bits, mirroring the tray checkboxes.
    layers: u32,
    /// `STYLE_*`, mirroring the "Style" dropdown.
    style: u32,
    /// Cursor in CSS pixels relative to the canvas, if over it.
    cursor: Option<(f64, f64)>,
    press: Option<Press>,
    legend: Element,
    /// The map settings (#settings), with the key, and its button.
    settings: Element,
    settings_toggle: Element,
    perf: Perf,
    terrain: TerrainLoader,
    /// No drawing before this time (ms; see `TAB_RETURN_GRACE_MS`).
    resume_at_ms: f64,
    /// Something changed since the last frame (input, settings): the map
    /// is only redrawn then, or while tiles load or the canvas resizes.
    redraw: bool,
    /// When the map was last drawn (animation frame time, ms), and whether
    /// that's long enough ago to count as idle (`IDLE_AFTER_MS`).
    last_draw_ms: f64,
    idle: bool,
    /// The server's documents (see `live`).
    network_feed: Feed<Network>,
    vessel_info_feed: Feed<Vessels>,
    vessel_feed: Feed<LiveVessels>,
    sailings_feed: Feed<LiveSailings>,
    notices_feed: Feed<LiveNotices>,
    /// The latest live documents, kept to read again as names arrive, and
    /// the boats' names (from their details).
    live_vessels: Option<LiveVessels>,
    live_sailings: Option<LiveSailings>,
    vessel_names: HashMap<u32, String>,
    /// The data credits (#credits): the terrain's, then the network's.
    credits: Element,
    credits_base: String,
    /// Crossing times, today's schedules and car deck use, by route id;
    /// how full boats' latest sailings left, by WSF id.
    crossings: Fetches<CrossingStats>,
    schedules: Fetches<RouteSchedule>,
    route_loads: Fetches<LoadStats>,
    vessel_loads: Fetches<VesselLoads>,
    /// When to arrive by car, by terminal id; and how that advice has
    /// held (the debug panel).
    advice: Fetches<TerminalAdvice>,
    advice_check: Fetches<AdviceCheck>,
    /// The fleet's last report, and a line on how current it is.
    fleet: Fleet,
    terminals: Terminals,
    fleet_status: String,
    labels: Labels,
    /// Each notice dot's slot last frame, which it keeps while it can.
    dot_slots: HashMap<String, usize>,
    notes: Notes,
    /// The route network from the server: each leg's line as drawn, the
    /// legs between terminals, and the routes riders know (with their ids,
    /// alongside), which group the legs.
    routes: Vec<RouteLine>,
    legs: Vec<Leg>,
    /// Points around everything highlighted (unit vectors), which a click
    /// flies to; empty when nothing is. And the flight under way, if any.
    focus_points: Vec<DVec3>,
    /// What the pointer's on, as the finder knows it: a click brings it
    /// up there.
    pointed: Option<Target>,
    /// A click on the map not yet acted on: where (CSS px), and whether
    /// by touch. The next frame finds what's there (as it does for the
    /// pointer) and acts on it; a touch has no pointer to find it by.
    pending_click: Option<((f64, f64), bool)>,
    flight: Option<Flight>,
    /// The finder at the top, and whether the data behind its selection's
    /// details has changed since they were shown.
    finder: Finder,
    finder_stale: bool,
    service_routes: Vec<ServiceRoute>,
    route_ids: Vec<String>,
    /// When moving boats next need a redraw (animation frame time, ms),
    /// and how long after a frame that is, as the last frame found.
    vessels_due_ms: f64,
    vessels_step_ms: f64,
    hud_vessel: Element,
}

impl App {
    /// The point on the slice under the cursor, if any.
    fn hover(&self) -> Option<LonLat> {
        self.surface_at(self.cursor?)
            .filter(|&p| KITSAP_SLICE.contains(p))
            .map(LonLat::from_vec)
    }

    /// Normalized device coordinates for a position in CSS pixels.
    fn ndc(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let (w, h) = (
            self.canvas.client_width() as f64,
            self.canvas.client_height() as f64,
        );
        (2.0 * x / w - 1.0, 1.0 - 2.0 * y / h)
    }

    /// Unit vector of the surface point under a CSS-pixel position, on or
    /// off the slice (drags can grab the planet beyond its edge).
    fn surface_at(&self, pos: (f64, f64)) -> Option<DVec3> {
        let (x, y) = self.ndc(pos);
        ray_sphere(&self.camera.ray(x, y), self.camera.radius).map(DVec3::normalize)
    }

    fn set_style(&mut self, style: u32) {
        self.redraw = true;
        self.style = style;
        self.update_legend();
    }

    /// Redraw the color key for the current style and overlays.
    fn update_legend(&self) {
        self.legend.set_inner_html(&legend::html(
            self.style,
            self.layers,
            self.hex.resolution,
            RELIEF,
            &self.fleet_status,
        ));
    }

    fn set_layer(&mut self, bit: u32, on: bool) {
        self.redraw = true;
        if on {
            self.layers |= bit;
        } else {
            self.layers &= !bit;
        }
        self.update_legend();
    }

    /// One animation frame; `time_ms` is the frame's timestamp.
    fn frame(&mut self, time_ms: f64) {
        let start = now_ms();
        if let Some(flight) = &mut self.flight {
            let started = *flight.started_ms.get_or_insert(time_ms);
            let t = ((time_ms - started) / FLIGHT_MS).clamp(0.0, 1.0);
            let pose = flight.at(t);
            if t >= 1.0 {
                self.flight = None;
            }
            self.set_pose(pose);
            self.redraw = true;
        }
        let paused = page_hidden() || start < self.resume_at_ms;
        if self.layers & LAYER_FERRIES != 0 && !paused {
            self.network_feed.poll(time_ms);
            self.vessel_info_feed.poll(time_ms);
            self.vessel_feed.poll(time_ms);
            self.sailings_feed.poll(time_ms);
            self.notices_feed.poll(time_ms);
        }
        if let Some(Ok(network)) = self.network_feed.take() {
            self.receive_network(&network);
        }
        if let Some(Ok(info)) = self.vessel_info_feed.take() {
            self.fleet.dimensions = dimensions_from(&info);
            self.vessel_names = info.vessels.into_iter().map(|v| (v.id, v.name)).collect();
            self.read_live();
        }
        if let Some(update) = self.vessel_feed.take() {
            self.receive_vessels(update);
        }
        if let Some(Ok(sailings)) = self.sailings_feed.take() {
            self.live_sailings = Some(sailings);
            self.read_live();
        }
        if let Some(Ok(notices)) = self.notices_feed.take() {
            self.terminals.set_bulletins(bulletins_from(&notices));
            self.finder_stale = true;
            self.redraw = true;
        }
        self.receive_fetches();
        if let Some(i) = self.finder.rested(now_ms())
            && self.finder.select(i)
        {
            self.selection_changed();
        }
        if self.finder_stale {
            self.finder_stale = false;
            self.show_selection_info();
        }
        let vessels_due = self.layers & LAYER_FERRIES != 0 && time_ms >= self.vessels_due_ms;
        let wanted =
            self.redraw || vessels_due || self.terrain.busy() || self.renderer.needs_frame();
        if !paused && wanted {
            // After an idle spell, average afresh: the gap isn't a slow
            // frame. Short gaps (between mouse events) are just activity.
            if time_ms - self.last_draw_ms > IDLE_AFTER_MS {
                self.perf.stats.pause();
            }
            self.redraw = false;
            self.draw_frame();
            self.perf.stats.record(time_ms, now_ms() - start);
            self.last_draw_ms = time_ms;
            self.vessels_due_ms = time_ms + self.vessels_step_ms;
        }
        self.idle = time_ms - self.last_draw_ms > IDLE_AFTER_MS;
        if self.perf.visible && time_ms - self.perf.shown_at >= PERF_REFRESH_MS {
            self.perf.shown_at = time_ms;
            self.update_perf_panel();
        }
    }

    /// Takes in the route network: the terminals, the legs and their
    /// lines (drawn as a static overlay), the routes riders know, and the
    /// network's credits.
    fn receive_network(&mut self, network: &Network) {
        self.terminals.set_network(network);
        let names = self.terminals.names();
        let name = |id: u32| names.get(&id).cloned().unwrap_or_else(|| id.to_string());
        let mut scene = Scene::default();
        let ink = palette::linear_rgba(palette::VESSEL_INK, ROUTE_ALPHA);
        self.routes = network
            .legs
            .iter()
            .map(|leg| {
                let route = routes::Route {
                    way: leg.id,
                    name: String::new(),
                    curves: leg.curves.clone(),
                };
                let path = route.path();
                let points: Vec<DVec3> = path.iter().map(|p| p.to_unit()).collect();
                scene.add_dashed_line(&points, EARTH_RADIUS_M, ink, ROUTE_WIDTH, ROUTE_DASH_PX);
                RouteLine {
                    name: leg.ends.map_or_else(
                        || format!("Way {}", leg.id),
                        |[a, b]| format!("{} ↔ {}", name(a), name(b)),
                    ),
                    points,
                }
            })
            .collect();
        self.renderer.set_overlay(LAYER_FERRIES, &scene.segments);
        self.legs = network
            .legs
            .iter()
            .map(|l| Leg {
                ends: l.ends.map(|[a, b]| (a, b)),
                length_m: l.length_m,
            })
            .collect();
        let index: HashMap<u64, usize> = network
            .legs
            .iter()
            .enumerate()
            .map(|(i, l)| (l.id, i))
            .collect();
        self.service_routes = network
            .routes
            .iter()
            .map(|r| ServiceRoute {
                name: r.name.clone(),
                legs: r
                    .legs
                    .iter()
                    .filter_map(|id| index.get(id).copied())
                    .collect(),
                terminals: r.terminals.iter().copied().collect(),
            })
            .collect();
        self.route_ids = network.routes.iter().map(|r| r.id.clone()).collect();
        let credits = network.attribution.join(" · ");
        self.credits.set_inner_html(&if credits.is_empty() {
            self.credits_base.clone()
        } else {
            format!("{} · {}", self.credits_base, html_escape(&credits))
        });
        log::info!(
            "route network: {} terminals, {} legs, {} routes",
            self.terminals.list.len(),
            self.legs.len(),
            self.service_routes.len()
        );
        self.read_live();
    }

    /// Reads the live documents again into boats and sailings, with the
    /// names known now.
    fn read_live(&mut self) {
        let names = self.terminals.names();
        if let Some(doc) = &self.live_vessels {
            self.fleet.vessels = vessels_from(doc, &self.vessel_names, &names);
        }
        if let Some(doc) = &self.live_sailings {
            self.terminals
                .set_space(sailings_from(doc, &self.vessel_names, &names));
        }
        self.finder_stale = true;
        self.redraw = true;
    }

    /// Asks the server when to arrive by car at terminal `id`.
    fn want_advice(&mut self, id: u32) {
        let url = format!("api/v1/advice?terminal={id}");
        self.advice.want(&id.to_string(), url);
    }

    /// Asks the server for route `r`'s crossing times and schedule, and
    /// its terminals' arrival advice.
    fn want_route(&mut self, r: usize) {
        let terminals: Vec<u32> = (self.service_routes.get(r))
            .map(|route| route.terminals.iter().copied().collect())
            .unwrap_or_default();
        for t in terminals {
            self.want_advice(t);
        }
        let Some(id) = self.route_ids.get(r) else {
            return;
        };
        let url = format!("api/v1/stats/crossings?route={id}");
        self.crossings.want(id, url);
        self.schedules
            .want(id, format!("api/v1/schedule?route={id}"));
        self.route_loads
            .want(id, format!("api/v1/stats/loads?route={id}"));
    }

    fn receive_fetches(&mut self) {
        let fresh = [
            self.crossings.receive("crossing times"),
            self.schedules.receive("schedule"),
            self.route_loads.receive("car deck use"),
            self.vessel_loads.receive("boat's loads"),
            self.advice.receive("arrival advice"),
        ];
        if self.advice_check.receive("advice check") && self.perf.visible {
            self.update_perf_panel();
        }
        if fresh.contains(&true) {
            self.finder_stale = true;
        }
    }

    /// Takes in the latest boats: new positions, or why there are none.
    fn receive_vessels(&mut self, update: Update<LiveVessels>) {
        self.finder_stale = true;
        let as_of = |vessels: &[Vessel]| {
            let newest = vessels.iter().map(|v| v.reported_ms).fold(0.0, f64::max);
            clock(newest)
        };
        self.fleet_status = match update {
            Ok(doc) => {
                let source = doc.source.clone();
                self.live_vessels = Some(doc);
                self.read_live();
                let vessels = &self.fleet.vessels;
                let in_service = vessels.iter().filter(|v| v.in_service).count();
                let moving = vessels.iter().filter(|v| v.is_moving()).count();
                let mut status = format!(
                    "{in_service} boats in service, {moving} under way, as of {}. \
                     Updated every {:.0} s.",
                    as_of(vessels),
                    VESSELS_MS / 1000.0
                );
                if source.stale {
                    status += &format!(
                        " WSF hasn't answered since {}.",
                        clock(source.fetched_ms as f64)
                    );
                }
                status
            }
            Err(e) if self.fleet.vessels.is_empty() => {
                format!("Ferry positions unavailable ({e}).")
            }
            Err(e) => format!(
                "Couldn't refresh ({e}); positions as of {}.",
                as_of(&self.fleet.vessels)
            ),
        };
        self.redraw = true;
        self.update_legend();
    }

    /// The finder's query changed: lists the best matches, the best
    /// selected.
    fn search(&mut self) {
        let query = self.finder.input.value();
        let all = finder::entries(&self.fleet, &self.terminals, &self.service_routes);
        let found = finder::matches(&query, &all);
        let before = self.finder.entry().map(|e| e.target);
        self.finder.show(found);
        if self.finder.entry().map(|e| e.target) != before {
            self.selection_changed();
        } else {
            self.show_selection_info();
        }
    }

    /// Brings `target` up in the finder, as if searched for by name: its
    /// name in the search field, the best matches with it first, and its
    /// details open (half a phone's screen; the keyboard stays down).
    fn find(&mut self, target: Target) {
        let all = finder::entries(&self.fleet, &self.terminals, &self.service_routes);
        let Some(entry) = all.iter().find(|e| e.target == target).cloned() else {
            return;
        };
        self.finder.input.set_value(&entry.name);
        let mut found = finder::matches(&entry.name, &all);
        found.retain(|e| e.target != target);
        found.insert(0, entry);
        found.truncate(finder::SHOWN);
        self.finder.show(found);
        self.finder.set_sheet(Sheet::Half);
        self.set_settings_open(false);
        self.show_selection_info();
    }

    /// Keys in the finder: Tab and the arrow keys move the selection,
    /// Enter puts the list away, Escape clears the search.
    fn search_key(&mut self, e: &KeyboardEvent) {
        let step = match (e.key().as_str(), e.shift_key()) {
            ("Tab", false) | ("ArrowDown", _) => 1,
            ("Tab", true) | ("ArrowUp", _) => -1,
            ("Enter", _) => {
                self.finder.set_open(false);
                self.finder.set_sheet(Sheet::Half);
                return;
            }
            ("Escape", _) => {
                self.finder.input.set_value("");
                self.search();
                let _ = self.finder.input.blur();
                return;
            }
            _ => return,
        };
        // Tab cycles while there are matches; otherwise it moves focus on.
        if self.finder.entries.is_empty() {
            return;
        }
        e.prevent_default();
        self.finder.set_open(true);
        if self.finder.cycle(step) {
            self.selection_changed();
        }
    }

    /// A new selection: shows its details and flies to it.
    fn selection_changed(&mut self) {
        self.redraw = true;
        self.show_selection_info();
        let Some(target) = self.finder.entry().map(|e| e.target) else {
            return;
        };
        let lit = self.highlight_target(target);
        let points = self.points_around(&lit, js_sys::Date::now());
        self.fly_to(&points);
    }

    fn show_selection_info(&mut self) {
        let Some(target) = self.finder.entry().map(|e| e.target) else {
            return;
        };
        match target {
            Target::Route(r) => self.want_route(r),
            // A terminal's schedule is its routes'.
            Target::Terminal(id) => {
                self.want_advice(id);
                let serving: Vec<usize> = (self.service_routes.iter().enumerate())
                    .filter(|(_, r)| r.terminals.contains(&id))
                    .map(|(i, _)| i)
                    .collect();
                for r in serving {
                    self.want_route(r);
                }
            }
            Target::Vessel(id) => {
                let url = format!("api/v1/loads?vessel={id}");
                self.vessel_loads.want(&id.to_string(), url);
            }
        }
        let ids = &self.route_ids;
        let (crossings, schedules) = (&self.crossings, &self.schedules);
        let crossing = |r: usize| ids.get(r).and_then(|id| crossings.get(id));
        let schedule = |r: usize| {
            ids.get(r)
                .map_or(Loaded::Missing, |id| schedules.loaded(id))
        };
        let (route_loads, vessel_loads) = (&self.route_loads, &self.vessel_loads);
        let route_load = |r: usize| {
            ids.get(r)
                .map_or(Loaded::Missing, |id| route_loads.loaded(id))
        };
        let vessel_load = |id: u32| vessel_loads.loaded(&id.to_string());
        let advice = &self.advice;
        let terminal_advice = |id: u32| advice.loaded(&id.to_string());
        let scope = Scope {
            fleet: &self.fleet,
            terminals: &self.terminals,
            routes: &self.service_routes,
            legs: &self.legs,
            crossings: &crossing,
            schedules: &schedule,
            vessel_loads: &vessel_load,
            route_loads: &route_load,
            advice: &terminal_advice,
            now_ms: js_sys::Date::now(),
            clock: &clock,
            date_clock: &date_clock,
        };
        let info = scope.info(target);
        self.finder.set_info(&info);
    }

    fn draw_frame(&mut self) {
        let (width, height) = self.renderer.sync_size();
        self.camera.aspect = width as f64 / height as f64;
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());

        if let Some((at, _)) = self.pending_click {
            self.cursor = Some(at);
        }
        let hover = self.hover();
        let (_, m_per_px, _) = self.view_scale();
        let globals = Globals {
            view_proj: self.camera.view_proj().as_mat4().to_cols_array_2d(),
            light_dir: sun_direction().as_vec3().extend(0.0).to_array(),
            params: [
                width as f32,
                height as f32,
                self.camera.radius as f32,
                (LINE_WIDTH_CSS_PX * dpr) as f32,
            ],
            flags: [self.layers, 0, self.style, 0],
            slice: KITSAP_SLICE
                .center
                .to_unit()
                .as_vec3()
                .extend(KITSAP_SLICE.angle().cos() as f32)
                .to_array(),
            terrain: [RELIEF as f32, m_per_px as f32, 0.0, 0.0],
        };
        let marker_scale = self.camera.altitude.min(self.camera.radius);
        if !self.measure.is_finished() && hover.is_some() {
            self.measure_end = hover;
        }
        let mut scene = Scene::build(&self.measure, hover, self.camera.radius, marker_scale);
        self.add_ferries(&mut scene);
        self.update_hex_grid();
        if let Some(h) = hover.filter(|_| self.layers & LAYER_CELLS != 0) {
            let cell = h3_cell(h, self.hex.resolution);
            scene.add_outline(&boundary(cell), self.camera.radius, HOVER_CELL_COLOR);
        }
        // Tile detail follows CSS pixels: finer on high-DPI screens would
        // quadruple the tiles for little visible gain.
        let height_px = self.canvas.client_height().max(1) as f64;
        let view = GlobeView::new(&self.camera, height_px);
        let regions = self.terrain.update(&mut self.renderer, &view);
        self.renderer.render(&globals, &scene, &regions);
        self.update_measure_box();
        // A click: fly to what it was on, and bring that up in the finder.
        if let Some((_, touch)) = self.pending_click.take() {
            // The finder first, so the fit leaves room for its details.
            if let Some(target) = self.pointed {
                self.find(target);
            }
            self.fly_to_focus();
            if touch {
                self.cursor = None;
            }
            self.redraw = true;
        }
    }

    /// Switches the measuring tool on or off; off clears the measurement.
    /// Opens or closes the map settings.
    fn set_settings_open(&self, open: bool) {
        let _ = self.settings.toggle_attribute_with_force("hidden", !open);
        let _ = self
            .settings_toggle
            .set_attribute("aria-expanded", if open { "true" } else { "false" });
    }

    fn pointer_down(&mut self, e: &PointerEvent) {
        self.redraw = true;
        self.flight = None;
        if e.button() != 0 {
            return;
        }
        let _ = self.canvas.set_pointer_capture(e.pointer_id());
        let pos = (e.offset_x() as f64, e.offset_y() as f64);
        self.press = Some(Press {
            start: pos,
            last: pos,
            dragging: false,
            grabbed: self.surface_at(pos),
        });
    }

    fn pointer_move(&mut self, e: &PointerEvent) {
        self.redraw = true;
        let pos = (e.offset_x() as f64, e.offset_y() as f64);
        self.cursor = Some(pos);
        let (x, y) = self.ndc(pos);
        if let Some(press) = &mut self.press {
            let moved = (pos.0 - press.start.0).hypot(pos.1 - press.start.1);
            press.dragging |= moved > CLICK_SLOP_PX;
            if press.dragging {
                // Push the globe by the grabbed point; fall back to pixel
                // rotation when the press or the cursor is off the globe.
                let grabbed = press.grabbed.is_some_and(|g| self.camera.drag_to(g, x, y));
                if !grabbed {
                    self.camera.drag(pos.0 - press.last.0, pos.1 - press.last.1);
                }
            }
            press.last = pos;
        }
    }

    fn pointer_up(&mut self, e: &PointerEvent) {
        self.redraw = true;
        if e.button() != 0 {
            return;
        }
        let press = self.press.take();
        // A click on the map first closes an open note.
        if press.as_ref().is_some_and(|p| !p.dragging) && self.notes.close() {
            return;
        }
        // Measuring, a click sets a point; otherwise a click on something
        // flies there and brings it up in the finder (next frame).
        if !self.measuring {
            if let Some(press) = press.filter(|p| !p.dragging) {
                self.pending_click = Some((press.start, e.pointer_type() == "touch"));
            }
            return;
        }
        if let Some(press) = press
            && !press.dragging
            && let Some(p) = self.hover()
        {
            if self.measure.is_finished() || self.measure.source.is_none() {
                self.measure_end = None;
            }
            self.measure.click(p);
        }
    }

    fn key_down(&mut self, e: &KeyboardEvent) {
        self.redraw = true;
        // Leave browser shortcuts alone, and keys aimed at controls.
        let on_control = e
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .is_some_and(|el| matches!(el.tag_name().as_str(), "INPUT" | "BUTTON"));
        if on_control || e.ctrl_key() || e.meta_key() || e.alt_key() {
            return;
        }
        match e.key().as_str() {
            "Escape" if !self.settings.has_attribute("hidden") => {
                self.set_settings_open(false);
                e.prevent_default();
            }
            "Escape" if self.notes.close() => e.prevent_default(),
            "Escape" if self.measuring => {
                self.set_measuring(false);
                e.prevent_default();
            }
            "m" | "M" => {
                self.set_measuring(!self.measuring);
                e.prevent_default();
            }
            _ => {}
        }
    }

    fn wheel(&mut self, e: &WheelEvent) {
        self.redraw = true;
        self.flight = None;
        e.prevent_default();
        // deltaMode 1 is lines; treat a line as about 16 px.
        let px = e.delta_y() * if e.delta_mode() == 1 { 16.0 } else { 1.0 };
        self.camera.zoom(px * 0.001);
    }
}

/// Drive `frame` from requestAnimationFrame. The closure re-schedules itself.
/// Toward the sun (unit vector), from the slice center's horizon.
fn sun_direction() -> DVec3 {
    let center = KITSAP_SLICE.center;
    let (east, north) = center.east_north();
    let (az, el) = (SUN_AZIMUTH_DEG.to_radians(), SUN_ELEVATION_DEG.to_radians());
    let horizontal = north * az.cos() + east * az.sin();
    (center.to_unit() * el.sin() + horizontal * el.cos()).normalize()
}

fn animate(app: Rc<RefCell<App>>) {
    let callback: FrameCallback = Rc::new(RefCell::new(None));
    let next = callback.clone();
    *callback.borrow_mut() = Some(Closure::new(move |time_ms: f64| {
        app.borrow_mut().frame(time_ms);
        request_frame(next.borrow().as_ref().unwrap());
    }));
    request_frame(callback.borrow().as_ref().unwrap());
}
