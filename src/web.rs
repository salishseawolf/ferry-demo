use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Element, HtmlCanvasElement, HtmlInputElement, KeyboardEvent, PointerEvent, WheelEvent,
};

use crate::camera::{OrbitCamera, parse_view_fragment, ray_sphere, view_fragment};
use crate::finder::{self, Finder, Loaded, Scope, Sheet, Target};
use crate::fleet::{Fleet, Vessel, dimensions_from, vessels_from};
use crate::geo::{EARTH_RADIUS_M, LonLat, distance_m, group_thousands};
use crate::grid::{KITSAP_SLICE, h3_cell};
use crate::hexgrid::{boundary, cells_around, edge_m, outlines, resolution_for};
use crate::legend;
use crate::live::{
    Feed, Update, VESSELS_MS, get_json, network_feed, notices_feed, sailings_feed, vessel_feed,
    vessel_info_feed,
};
use crate::measure::{Measure, Reading};
use crate::palette;
use crate::perf::{FrameStats, format_bytes};
use crate::placement::{self, Badge, Rect, Side, Wanted};
use crate::renderer::{Globals, Renderer, TERRAIN_SLOTS};
use crate::routes::{self, Leg, ServiceRoute, legs_between};
use crate::scene::{GRID_COLOR, HOVER_CELL_COLOR, Marker, MarkerKind, Scene};
use crate::terminals::{Terminals, bulletins_from, sailings_from, symbol_scale};
use crate::terrain::{GlobeView, Manifest};
use crate::tiles::{TerrainLoader, load_index};
use crate::view::{LAYER_CELLS, LAYER_FERRIES, STYLE_CARTOGRAPHIC, STYLE_HYPSOMETRIC};
use ferry_model::api::{
    AdviceCheck, CrossingStats, LiveNotices, LiveSailings, LiveVessels, LoadStats, Network,
    RouteSchedule, TerminalAdvice, VesselLoads, Vessels,
};
use glam::DVec3;
use h3o::{CellIndex, Resolution};
use serde::de::DeserializeOwned;

/// Start over Kitsap County, the slice's center, high enough to see most
/// of it.
const START_VIEW: LonLat = KITSAP_SLICE.center;
const START_ALTITUDE_RADII: f64 = 0.045;
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
/// The sun, fixed over the slice: from the northwest, 45° up, the usual
/// light for hillshaded relief (slopes facing it read as raised).
const SUN_AZIMUTH_DEG: f64 = 315.0;
const SUN_ELEVATION_DEG: f64 = 45.0;
/// Terrain relief is drawn at this vertical exaggeration.
const RELIEF: f64 = 2.0;
/// How often the performance panel refreshes (ms); updating the page every
/// frame would itself cost time.
const PERF_REFRESH_MS: f64 = 250.0;
/// How long the copy button shows its check mark.
const COPIED_MS: i32 = 1500;
/// The map counts as idle after this long (ms) without a redraw.
const IDLE_AFTER_MS: f64 = 500.0;
/// After the tab becomes visible, wait this long (ms) before creating the
/// GPU device (a page reloaded in the background) or drawing again. The
/// browser is rebuilding canvases then (the old page's, our own), and
/// Firefox crashed when our swapchain was created in the same moment.
const TAB_RETURN_GRACE_MS: f64 = 300.0;
/// The hex grid covers this many times the view, so it can zoom out this
/// far before rebuilding (see `HexGrid`).
const HEX_COVER: f64 = 1.25;
/// Ferry hulls are drawn at their real length and beam, to scale with the
/// terrain at every zoom. Outline widths (docked and laid-up boats, and
/// the hovered one) are in CSS px, down to a share of the beam on small
/// hulls. The cursor picks the nearest boat within `VESSEL_PICK_PX`, or
/// within a large hull.
const VESSEL_LINE_PX: f64 = 1.5;
/// An outline is at most this share of the hull's beam.
const VESSEL_LINE_OF_BEAM: f64 = 0.3;
const VESSEL_HOVER_LINE_PX: f64 = 2.5;
const VESSEL_PICK_PX: f64 = 14.0;
/// Speed vectors and wakes, in standard line widths, and the wake's
/// opacity where it's newest; the tick across a vector's tip, CSS px each
/// side.
const VECTOR_WIDTH: f32 = 0.65;
const WAKE_WIDTH: f32 = 1.5;
const WAKE_ALPHA: f64 = 0.55;
const VECTOR_TICK_PX: f64 = 3.5;
/// Names show when the map is at least this detailed (ground meters per
/// CSS px), `LABEL_GAP_PX` from their symbol's box (placement.rs picks
/// the side and slot).
const LABEL_MAX_M_PER_PX: f64 = 150.0;
const LABEL_GAP_PX: f64 = 6.0;
/// A boat within this distance (m) of a terminal would rather be named on
/// the side away from the terminal's anchor, clear of it.
const LABEL_NEAR_TERMINAL_M: f64 = 500.0;
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
/// A fit keeps this far (CSS px) from the panels around the free part
/// of the map. On phones the details sheet is, at its peek and half
/// heights, this tall (index.html's .sheet-peek and .sheet-half).
const FIT_PANEL_GAP_PX: f64 = 8.0;
const SHEET_PEEK_PX: f64 = 124.0;
const SHEET_HALF_SHARE: f64 = 0.52;
/// A lone boat or terminal is fitted as if it reached this far (m).
const FIT_MIN_REACH_M: f64 = 1500.0;
/// Crossing times fetched for a route are kept this long (ms) before
/// they're asked for again; schedules (checked by the server every 10
/// minutes) this long.
const CROSSINGS_KEEP_MS: f64 = 5.0 * 60_000.0;
const SCHEDULES_KEEP_MS: f64 = 10.0 * 60_000.0;
/// A boat's latest loads change as it leaves a dock: kept a minute.
const VESSEL_LOADS_KEEP_MS: f64 = 60_000.0;
/// A fetch that failed is tried again, when next wanted, after this long.
const FAILED_KEEP_MS: f64 = 30_000.0;
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
/// A point this far (m) along a direction gives it on screen.
const HEADING_PROBE_M: f64 = 200.0;
/// Moving boats are redrawn once the fastest in view has gone this far on
/// screen (CSS px), but no more often than every `VESSEL_MIN_FRAME_MS`:
/// at the whole-fleet view that's a frame every half minute, not 60 a
/// second.
const VESSEL_STEP_PX: f64 = 0.5;
const VESSEL_MIN_FRAME_MS: f64 = 33.0;
/// Ferry times are shown in the ferries' own time zone, wherever the
/// viewer is.
const FERRY_TIME_ZONE: &str = "America/Los_Angeles";

/// Answers fetched in the background, by what they were for, waiting to be
/// taken in by the next frame.
type Arrivals<T> = Rc<RefCell<Vec<(String, Update<T>)>>>;

type FrameCallback = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

/// Documents fetched when they're wanted, by key (a route id): each
/// asked for once at a time, and kept `keep_ms` before it's asked for
/// again.
struct Fetches<T> {
    /// What's come, or why it didn't, with when (ms). A failure is kept
    /// `FAILED_KEEP_MS`.
    got: HashMap<String, (Update<T>, f64)>,
    pending: HashSet<String>,
    arrived: Arrivals<T>,
    keep_ms: f64,
}

impl<T: DeserializeOwned + 'static> Fetches<T> {
    fn new(keep_ms: f64) -> Self {
        Self {
            got: HashMap::new(),
            pending: HashSet::new(),
            arrived: Rc::default(),
            keep_ms,
        }
    }

    /// Asks for `key`'s document at `url`, unless it's here and recent,
    /// or already asked for.
    fn want(&mut self, key: &str, url: String) {
        let recent = self.got.get(key).is_some_and(|(got, at)| {
            now_ms() - at
                < if got.is_ok() {
                    self.keep_ms
                } else {
                    FAILED_KEEP_MS
                }
        });
        if recent || !self.pending.insert(key.to_string()) {
            return;
        }
        let (key, arrived) = (key.to_string(), self.arrived.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let got = get_json::<T>(&url).await;
            arrived.borrow_mut().push((key, got));
        });
    }

    /// Takes in what's come back; says whether anything new did. Failures
    /// are logged as `what` (asked for again when next wanted).
    fn receive(&mut self, what: &str) -> bool {
        let arrived: Vec<_> = self.arrived.borrow_mut().drain(..).collect();
        let mut fresh = false;
        for (key, got) in arrived {
            self.pending.remove(&key);
            if let Err(e) = &got {
                log::warn!("{what} for {key}: {e}");
            }
            self.got.insert(key, (got, now_ms()));
            fresh = true;
        }
        fresh
    }

    fn get(&self, key: &str) -> Option<&T> {
        self.got.get(key).and_then(|(got, _)| got.as_ref().ok())
    }

    fn loaded(&self, key: &str) -> Loaded<'_, T> {
        match self.got.get(key) {
            None => Loaded::Pending,
            Some((Ok(doc), _)) => Loaded::Here(doc),
            Some((Err(_), _)) => Loaded::Missing,
        }
    }
}

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

/// The performance panel and the numbers behind it.
struct Perf {
    stats: FrameStats,
    card: Element,
    body: Element,
    copy: Element,
    visible: bool,
    /// When the panel last refreshed (animation frame time, ms).
    shown_at: f64,
}

/// The hex grid on screen. It covers a little more than the view, with
/// cells only where they can be seen, and is rebuilt only when the view
/// outgrows that: another resolution or window size, the
/// view's center moving into another cell a level up, or zooming out past
/// the margin. Zooming in rebuilds it only if the last build had to settle
/// for a coarser resolution than the zoom asked for.
struct HexGrid {
    /// What it was built for: the resolution asked for, the cell a level
    /// coarser under the view's center, and the canvas size.
    built_for: Option<(Resolution, CellIndex, (i32, i32))>,
    /// Ground meters per CSS pixel when built.
    built_m_per_px: f64,
    /// The resolution drawn (coarser than asked if the view would need too
    /// many cells), and how many cells.
    resolution: Resolution,
    cells: usize,
}

/// The measurement info box.
struct MeasureBox {
    card: Element,
    readings: Element,
    tool: Element,
    source: Element,
    bearing: Element,
    distance: Element,
    status: Element,
    copy: Element,
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

    /// The ferries and terminals in view. Each boat is a hull, with its
    /// wake and, under way, its speed vector; each terminal an anchor with
    /// a gauge of its next sailing's drive-up space. Names show once zoomed
    /// in. The boat or terminal under the cursor is outlined and described
    /// in the HUD. Also works out when moving boats next need a redraw.
    fn add_ferries(&mut self, scene: &mut Scene) {
        self.vessels_step_ms = f64::INFINITY;
        if self.layers & LAYER_FERRIES == 0 {
            set_text(&self.hud_vessel, "");
            self.focus_points.clear();
            self.pointed = None;
            let _ = self
                .canvas
                .class_list()
                .toggle_with_force("pickable", false);
            self.labels.place(&[], &[], &[]);
            self.notes.place(&[], (0.0, 0.0));
            return;
        }
        let now = js_sys::Date::now();
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        let (w, h) = (
            self.canvas.client_width() as f64,
            self.canvas.client_height() as f64,
        );
        let (_, m_per_px, reach_m) = self.view_scale();
        // Symbol sizes in CSS px, to scale with the terrain; terminals'
        // shrink as the view zooms in (terminals::symbol_scale).
        let to_px = |m: f64| m / m_per_px;
        let terminal_scale = symbol_scale(2.0 * reach_m);
        let anchor_px = to_px(ANCHOR_M * terminal_scale);
        let hull_px = |v: &Vessel| {
            let (length, beam) = self.fleet.size_m(v);
            (to_px(length), to_px(beam))
        };
        // The longest boat is about 140 m.
        let margin = to_px(140.0).max(anchor_px);
        let in_view = |p: DVec3| {
            self.screen_px(p).filter(|s| {
                (-margin..w + margin).contains(&s.0) && (-margin..h + margin).contains(&s.1)
            })
        };
        let vessels = &self.fleet.vessels;
        let mut shown: Vec<(usize, DVec3, (f64, f64))> = vessels
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let p = v.position_at(now);
                in_view(p).map(|s| (i, p, s))
            })
            .collect();
        let terminals = &self.terminals;
        let slips: Vec<(usize, DVec3, (f64, f64))> = terminals
            .list
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let p = t.position.to_unit();
                in_view(p).map(|s| (i, p, s))
            })
            .collect();

        // Moving boats in view: redraw once the fastest moves a step.
        let fastest = shown
            .iter()
            .map(|&(i, ..)| &vessels[i])
            .filter(|v| v.is_moving())
            .map(Vessel::speed_mps)
            .fold(0.0, f64::max);
        if fastest > 0.0 {
            let step_ms = 1000.0 * VESSEL_STEP_PX * m_per_px / fastest;
            self.vessels_step_ms = step_ms.clamp(VESSEL_MIN_FRAME_MS, VESSELS_MS);
        }

        // Anchors stand a little inland of their slips, so boats at the
        // dock don't cover them; boats are drawn over them anyway.
        let facing = |p: DVec3, deg: f64| {
            let (east, north) = LonLat::from_vec(p).east_north();
            let d = deg.to_radians();
            east * d.sin() + north * d.cos()
        };
        let inland = (ANCHOR_INLAND * anchor_px * m_per_px + ANCHOR_CLEAR_M) / self.camera.radius;
        let anchor_at = |i: usize, p: DVec3| {
            terminals
                .facing_deg(&terminals.list[i])
                .map_or(p, |deg| (p - facing(p, deg) * inland).normalize())
        };

        // Whatever is nearest the cursor, boat or terminal.
        let dragging = self.press.as_ref().is_some_and(|p| p.dragging);
        let hovered = self.cursor.filter(|_| !dragging).and_then(|c| {
            // Distance to the cursor, less the symbol's reach beyond the
            // pick radius, so large symbols are picked anywhere on them.
            let near = |s: (f64, f64), reach: f64| {
                (s.0 - c.0).hypot(s.1 - c.1) - (reach - VESSEL_PICK_PX).max(0.0)
            };
            let boats = shown
                .iter()
                .map(|&(i, _, s)| (Picked::Vessel(i), near(s, hull_px(&vessels[i]).0 / 2.0)));
            // Terminals where their anchors are drawn: within the box of
            // the anchor and the gauge beneath it, grown to the pick
            // radius if smaller, ranked as boats at the box's edge and
            // nearer within it (so a boat over an anchor wins).
            let docks = slips.iter().filter_map(|&(i, p, _)| {
                let (x, y) = self.screen_px(anchor_at(i, p))?;
                let half = (
                    (anchor_px * ANCHOR_HALF_WIDTH).max(VESSEL_PICK_PX),
                    (anchor_px * ANCHOR_HALF_HEIGHT).max(VESSEL_PICK_PX),
                );
                let center = (x, y + anchor_px * ANCHOR_DROP);
                let q = (
                    (c.0 - center.0).abs() - half.0,
                    (c.1 - center.1).abs() - half.1,
                );
                let outside = q.0.max(0.0).hypot(q.1.max(0.0));
                let inside = q.0.max(q.1).min(0.0);
                Some((Picked::Terminal(i), VESSEL_PICK_PX + outside + inside))
            });
            boats
                .chain(docks)
                .filter(|&(_, d)| d <= VESSEL_PICK_PX)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(picked, _)| picked)
        });
        // Failing a boat or terminal, the route line under the cursor.
        let hovered_route = if hovered.is_none() && !dragging {
            self.cursor.and_then(|c| self.route_at(c))
        } else {
            None
        };
        // What the pointer's on, or else the finder's selection.
        let pointing = hovered.is_some() || hovered_route.is_some();
        self.pointed = match hovered {
            Some(Picked::Vessel(i)) => Some(Target::Vessel(vessels[i].id)),
            Some(Picked::Terminal(i)) => Some(Target::Terminal(self.terminals.list[i].id)),
            None => hovered_route
                .and_then(|r| self.service_routes.iter().position(|s| s.legs.contains(&r)))
                .map(Target::Route),
        };
        let lit = match self.finder.entry() {
            Some(e) if !pointing => self.highlight_target(e.target),
            _ => self.highlight(hovered, hovered_route),
        };
        self.focus_points = if pointing {
            self.points_around(&lit, now)
        } else {
            Vec::new()
        };
        let pickable = !self.focus_points.is_empty();
        let _ = self
            .canvas
            .class_list()
            .toggle_with_force("pickable", pickable);
        // What isn't highlighted fades back while a route is.
        let dim = |on: bool| {
            if lit.dim && !on { DIM_ALPHA } else { 1.0 }
        };
        let fade = |mut c: [f32; 4], k: f32| {
            c[3] *= k;
            c
        };
        let glow = |on: bool| {
            palette::linear_rgba(
                palette::VESSEL_HOVER,
                if on { SYMBOL_GLOW_ALPHA } else { 0.0 },
            )
        };
        for &r in &lit.routes {
            let points = &self.routes[r].points;
            let leg = (lit.leg == Some(r) && lit.routes.len() > 1).then_some(LEG_GLOW);
            for (width, alpha) in ROUTE_GLOW.into_iter().chain(leg) {
                let color = palette::linear_rgba(palette::VESSEL_HOVER, alpha);
                scene.add_line(points, self.camera.radius, color, width);
            }
            let ink = palette::linear_rgba(palette::VESSEL_INK, ROUTE_HIGHLIGHT_ALPHA);
            scene.add_dashed_line(points, self.camera.radius, ink, ROUTE_WIDTH, ROUTE_DASH_PX);
        }

        // Out of service at the bottom, then docked, then under way, and
        // the hovered boat over all.
        let rank = |i: usize| {
            let v = &vessels[i];
            (
                hovered == Some(Picked::Vessel(i)),
                v.in_service,
                v.is_moving(),
            )
        };
        shown.sort_by_key(|&(i, ..)| rank(i));

        let radius = self.camera.radius;
        let ink = |alpha: f32| palette::linear_rgba(palette::VESSEL_INK, alpha);
        for &(i, p, _) in &shown {
            let v = &vessels[i];
            let k = dim(lit.vessels.contains(&i));
            let ink = |alpha: f32| ink(alpha * k);
            // The wake fades with age, segment by segment.
            for pair in self.fleet.wake(v, now).windows(2) {
                let age = (pair[0].1 + pair[1].1) / 2.0;
                let alpha = (WAKE_ALPHA * (1.0 - age)) as f32;
                scene.add_line(&[pair[0].0, pair[1].0], radius, ink(alpha), WAKE_WIDTH);
            }
            if v.is_moving() {
                // To where it will be in 3 minutes, with a tick across the tip.
                let tip = v.ahead_of(p);
                let across = tip.cross(v.heading_from(tip)).normalize_or_zero();
                let tick = VECTOR_TICK_PX * m_per_px / radius;
                let ends = [tip + across * tick, tip - across * tick].map(DVec3::normalize);
                scene.add_line(&[p, tip], radius, ink(0.9), VECTOR_WIDTH);
                scene.add_line(&ends, radius, ink(0.9), VECTOR_WIDTH);
            }
        }

        scene.markers.extend(slips.iter().map(|&(i, p, _)| {
            let t = &terminals.list[i];
            let next = terminals.next_sailing(t, now);
            let gauge = if next.is_some_and(|s| s.nearly_full()) {
                palette::TERMINAL_NEARLY_FULL
            } else {
                palette::VESSEL_INK
            };
            let ink = if hovered == Some(Picked::Terminal(i)) {
                palette::VESSEL_HOVER
            } else {
                palette::VESSEL_INK
            };
            let on = lit.terminals.contains(&t.id);
            let k = dim(on);
            Marker {
                position: (anchor_at(i, p) * radius).as_vec3().to_array(),
                heading: [0.0; 3],
                size: [(anchor_px * dpr) as f32, (anchor_px * dpr) as f32],
                style: [
                    0.0,
                    0.0,
                    MarkerKind::Anchor as u8 as f32,
                    next.and_then(|s| s.space_left()).map_or(-1.0, |f| f as f32),
                ],
                fill: palette::linear_rgba(gauge, k),
                outline: palette::linear_rgba(ink, k),
                glow: glow(on),
            }
        }));
        scene.markers.extend(shown.iter().map(|&(i, p, _)| {
            let v = &vessels[i];
            let (fill, outline, line, dashed) = if !v.in_service {
                (ink_paper(0.6), palette::VESSEL_IDLE, VESSEL_LINE_PX, true)
            } else if v.is_moving() {
                (ink(1.0), palette::VESSEL_INK, 0.0, false)
            } else {
                (ink_paper(1.0), palette::VESSEL_INK, VESSEL_LINE_PX, false)
            };
            let (outline, line, dashed) = if hovered == Some(Picked::Vessel(i)) {
                (palette::VESSEL_HOVER, VESSEL_HOVER_LINE_PX, false)
            } else {
                (outline, line, dashed)
            };
            let (length, beam) = hull_px(v);
            // The outline scales down with a small hull, not past it.
            let line = line.min(beam * VESSEL_LINE_OF_BEAM);
            let on = lit.vessels.contains(&i);
            let k = dim(on);
            Marker {
                position: (p * radius).as_vec3().to_array(),
                heading: v.heading_from(p).as_vec3().to_array(),
                size: [(length * dpr) as f32, (beam * dpr) as f32],
                style: [
                    (line * dpr) as f32,
                    f32::from(u8::from(dashed)),
                    MarkerKind::Hull as u8 as f32,
                    -1.0,
                ],
                fill: fade(fill, k),
                outline: palette::linear_rgba(outline, k),
                glow: glow(on),
            }
        }));

        // What names keep clear of: each hull in view (its box on screen,
        // turned to its heading) and each anchor with its gauge. Notice
        // dots are added below, as they're placed.
        // A hull's two ends on screen, as it's turned to its heading.
        let hull_ends = |v: &Vessel, p: DVec3| {
            let half = facing(p, v.heading_deg) * (self.fleet.size_m(v).0 / 2.0 / radius);
            match [p + half, p - half].map(|e| self.screen_px(e.normalize())) {
                [Some(a), Some(b)] => Some([a, b]),
                _ => None,
            }
        };
        let hull_box = |v: &Vessel, p: DVec3, s: (f64, f64)| {
            let (length, beam) = self.fleet.size_m(v);
            match hull_ends(v, p) {
                Some(ends) => Rect::bounding(&ends, to_px(beam) / 2.0),
                None => Rect::around(s, (to_px(length) / 2.0, to_px(length) / 2.0)),
            }
        };
        let anchor_box = |i: usize, p: DVec3, slip: (f64, f64)| {
            let s = self.screen_px(anchor_at(i, p)).unwrap_or(slip);
            Rect::around(
                (s.0, s.1 + anchor_px * ANCHOR_DROP),
                (
                    anchor_px * ANCHOR_HALF_WIDTH,
                    anchor_px * ANCHOR_HALF_HEIGHT,
                ),
            )
        };
        let mut obstacles: Vec<Rect> = shown
            .iter()
            .map(|&(i, p, s)| hull_box(&vessels[i], p, s))
            .chain(slips.iter().map(|&(i, p, slip)| anchor_box(i, p, slip)))
            .collect();

        // Names once zoomed in enough for them to sit beside their symbols
        // (placement), preferring the side away from `from` if given: a
        // boat's terminal when it's near one, otherwise its speed vector;
        // or, for a terminal, the water its slip opens onto. What's
        // highlighted is named first, then terminals, then boats.
        let away = |s: (f64, f64), from: Option<(f64, f64)>| {
            if from.is_some_and(|f| f.0 > s.0) {
                Side::Left
            } else {
                Side::Right
            }
        };
        let any_lit = !lit.vessels.is_empty() || !lit.terminals.is_empty();
        let rank = |is_lit: bool, kind: u8| if any_lit && is_lit { 0 } else { kind };
        let mut named = Vec::new();
        if m_per_px <= TERMINAL_LABEL_MAX_M_PER_PX {
            for &(i, p, slip) in &slips {
                let t = &terminals.list[i];
                let water = terminals.facing_deg(t).and_then(|deg| {
                    let probe = HEADING_PROBE_M / radius;
                    self.screen_px((p + facing(p, deg) * probe).normalize())
                });
                let symbol = anchor_box(i, p, slip);
                let is_lit = lit.terminals.contains(&t.id);
                named.push(Label {
                    key: format!("t{}", t.id),
                    text: t.name.clone(),
                    symbol,
                    prefer: away(symbol.center(), water),
                    rank: rank(is_lit, 1),
                    class: "terminal",
                    dim: dim(is_lit) < 1.0,
                    lit: is_lit,
                });
            }
        }
        if m_per_px <= LABEL_MAX_M_PER_PX {
            // A boat near a terminal (docked, arriving or leaving) would
            // rather be named on the side away from the terminal's anchor,
            // over the water.
            let near_anchor = |p: DVec3| {
                let at = LonLat::from_vec(p);
                let (i, t) = (terminals.list.iter().enumerate())
                    .map(|(i, t)| (i, t, distance_m(at, t.position, EARTH_RADIUS_M)))
                    .filter(|&(.., d)| d <= LABEL_NEAR_TERMINAL_M)
                    .min_by(|a, b| a.2.total_cmp(&b.2))
                    .map(|(i, t, _)| (i, t))?;
                self.screen_px(anchor_at(i, t.position.to_unit()))
            };
            for &(i, p, s) in &shown {
                let v = &vessels[i];
                let from = near_anchor(p).or_else(|| {
                    v.is_moving()
                        .then(|| self.screen_px(v.ahead_of(p)))
                        .flatten()
                });
                let is_lit = lit.vessels.contains(&i);
                named.push(Label {
                    key: format!("v{}", v.id),
                    text: v.name.clone(),
                    symbol: hull_box(v, p, s),
                    prefer: away(s, from),
                    rank: rank(is_lit, 2),
                    class: if v.in_service { "" } else { "idle" },
                    dim: dim(is_lit) < 1.0,
                    lit: is_lit,
                });
            }
        }

        // A dot on each boat or terminal with a notice in effect, which
        // opens to the notice on hover or click. Placed before the names
        // (placement::place_badges): overlapping its own symbol, on the
        // side of it with the most room, upper right where there's room
        // all round.
        let mut notes = Vec::new();
        let mut badges = Vec::new();
        if m_per_px <= NOTE_MAX_M_PER_PX {
            let dot_px = to_px(NOTE_M * terminal_scale);
            // Upper first, then right.
            let by_preference = |mut slots: Vec<(f64, f64)>| {
                slots.sort_by(|a, b| (a.1 - 0.5 * a.0).total_cmp(&(b.1 - 0.5 * b.0)));
                slots
            };
            for &(i, p, slip) in &slips {
                let t = &terminals.list[i];
                let bulletins = terminals.bulletins(t);
                if bulletins.is_empty() {
                    continue;
                }
                // The anchor's corners and sides.
                let s = self.screen_px(anchor_at(i, p)).unwrap_or(slip);
                let half = anchor_px / 2.0;
                let (up, out) = (half + dot_px * NOTE_RAISE, half + 0.3 * dot_px);
                badges.push(Badge {
                    key: format!("t{}", t.id),
                    size: dot_px,
                    symbol: anchor_box(i, p, slip),
                    slots: by_preference(vec![
                        (s.0 + half, s.1 - up),
                        (s.0 - half, s.1 - up),
                        (s.0 + out, s.1),
                        (s.0 - out, s.1),
                        (s.0 + half, s.1 + half),
                        (s.0 - half, s.1 + half),
                    ]),
                });
                notes.push(Note {
                    key: format!("t{}", t.id),
                    at: s,
                    size: dot_px,
                    dim: dim(lit.terminals.contains(&t.id)) < 1.0,
                    heading: format!("{} terminal", t.name),
                    items: bulletins
                        .iter()
                        .map(|b| NoteItem {
                            title: b.title.clone(),
                            text: b.text.clone(),
                            when: (b.updated_ms > 0.0)
                                .then(|| format!("Updated {}", date_clock(b.updated_ms))),
                        })
                        .collect(),
                });
            }
            for &(i, p, s) in &shown {
                let v = &vessels[i];
                let Some(note) = &v.note else {
                    continue;
                };
                let (length, beam) = hull_px(v);
                let boat_dot = length * BOAT_NOTE_SHARE;
                // Either side of each end of the hull, and just past each
                // end, however it's turned.
                let [a, b] = hull_ends(v, p)
                    .unwrap_or([(s.0 + length / 2.0, s.1), (s.0 - length / 2.0, s.1)]);
                let span = (a.0 - b.0).hypot(a.1 - b.1).max(1e-6);
                let along = ((a.0 - b.0) / span, (a.1 - b.1) / span);
                let across = (-along.1, along.0);
                let side = beam / 2.0 + 0.35 * boat_dot;
                let mut slots = Vec::new();
                for (end, sign) in [(a, 1.0), (b, -1.0)] {
                    let base = (
                        s.0 + (end.0 - s.0) * BOAT_NOTE_REACH,
                        s.1 + (end.1 - s.1) * BOAT_NOTE_REACH,
                    );
                    for k in [1.0, -1.0] {
                        slots.push((base.0 + across.0 * side * k, base.1 + across.1 * side * k));
                    }
                    let past = 0.4 * boat_dot * sign;
                    slots.push((end.0 + along.0 * past, end.1 + along.1 * past));
                }
                badges.push(Badge {
                    key: format!("v{}", v.id),
                    size: boat_dot,
                    symbol: hull_box(v, p, s),
                    slots: by_preference(slots),
                });
                notes.push(Note {
                    key: format!("v{}", v.id),
                    at: s,
                    size: boat_dot,
                    dim: dim(lit.vessels.contains(&i)) < 1.0,
                    heading: v.name.clone(),
                    items: vec![NoteItem {
                        title: note.clone(),
                        text: String::new(),
                        when: None,
                    }],
                });
            }
        }
        let dots = placement::place_badges(&badges, &obstacles, &self.dot_slots);
        self.dot_slots.clear();
        for ((note, b), (rects, slot)) in notes.iter_mut().zip(&badges).zip(&dots) {
            if let Some(r) = rects.get(*slot) {
                note.at = r.center();
            }
            self.dot_slots.insert(b.key.clone(), *slot);
        }
        self.notes.place(&notes, (w, h));
        obstacles.extend((notes.iter()).map(|n| Rect::around(n.at, (n.size / 2.0, n.size / 2.0))));
        self.labels.place(&named, &obstacles, &dots);

        let text = match hovered {
            Some(Picked::Vessel(i)) => vessels[i].describe(now, clock).join("\n"),
            Some(Picked::Terminal(i)) => terminals
                .describe(&terminals.list[i], now, TERMINAL_SAILINGS_SHOWN, clock)
                .join("\n"),
            None => hovered_route
                .map(|r| self.describe_route(r, &lit))
                .unwrap_or_default(),
        };
        set_text(&self.hud_vessel, &text);
    }

    /// The route whose line passes within `ROUTE_PICK_PX` of `cursor`
    /// (CSS px), the nearest if several. Slip approaches aren't picked.
    fn route_at(&self, cursor: (f64, f64)) -> Option<usize> {
        let (cx, cy) = cursor;
        let to_segment = |a: (f64, f64), b: (f64, f64)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let t = (((cx - a.0) * dx + (cy - a.1) * dy) / (dx * dx + dy * dy).max(1e-9))
                .clamp(0.0, 1.0);
            (cx - a.0 - dx * t).hypot(cy - a.1 - dy * t)
        };
        self.routes
            .iter()
            .enumerate()
            .filter(|&(r, _)| {
                self.legs
                    .get(r)
                    .is_none_or(|l| l.ends.is_none_or(|(a, b)| a != b))
            })
            .filter_map(|(r, route)| {
                let screen: Vec<Option<(f64, f64)>> =
                    route.points.iter().map(|&p| self.screen_px(p)).collect();
                let d = screen
                    .windows(2)
                    .filter_map(|w| Some(to_segment(w[0]?, w[1]?)))
                    .fold(f64::INFINITY, f64::min);
                (d <= ROUTE_PICK_PX).then_some((r, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(r, _)| r)
    }

    /// What to highlight. A hovered boat: itself, the legs of its trip and
    /// the terminal it's bound for. A hovered leg: the route riders know
    /// it as, with all its legs and terminals and the boats sailing
    /// between them (or, until the terminals arrive, just the leg).
    fn highlight(&self, hovered: Option<Picked>, route: Option<usize>) -> Highlight {
        if let Some(Picked::Vessel(i)) = hovered {
            return self.lit_vessel(i);
        }
        let Some(r) = route else {
            return Highlight::default();
        };
        let service = self.service_routes.iter().position(|s| s.legs.contains(&r));
        let mut lit = service.map_or_else(
            || Highlight {
                routes: vec![r],
                dim: true,
                ..Highlight::default()
            },
            |s| self.lit_service(s),
        );
        lit.leg = Some(r);
        lit
    }

    /// What to highlight for the finder's selection: a boat or route as
    /// when hovered, and a terminal with the legs from it.
    fn highlight_target(&self, target: Target) -> Highlight {
        match target {
            Target::Vessel(id) => self
                .fleet
                .vessels
                .iter()
                .position(|v| v.id == id)
                .map(|i| self.lit_vessel(i))
                .unwrap_or_default(),
            Target::Route(s) if s < self.service_routes.len() => self.lit_service(s),
            Target::Route(_) => Highlight::default(),
            Target::Terminal(id) => Highlight {
                terminals: BTreeSet::from([id]),
                routes: (0..self.legs.len())
                    .filter(|&i| self.legs[i].ends.is_some_and(|(a, b)| a == id || b == id))
                    .collect(),
                ..Highlight::default()
            },
        }
    }

    /// Boat `i` (in the fleet) with, under way, the legs of its trip and
    /// the terminal it's bound for, or at the dock, the terminal it's at.
    fn lit_vessel(&self, i: usize) -> Highlight {
        let v = &self.fleet.vessels[i];
        let mut lit = Highlight {
            vessels: BTreeSet::from([i]),
            ..Highlight::default()
        };
        if v.at_dock {
            // WSF gives a docked boat's terminal as the one it departs
            // from next; laid up elsewhere (Eagle Harbor), it has none.
            lit.terminals.extend(v.departing_id);
        } else if let Some((from, to)) = v.departing_id.zip(v.arriving_id) {
            lit.routes = legs_between(&self.legs, from, to);
            lit.terminals.insert(to);
        }
        lit
    }

    /// Route `s` (of the routes riders know): its legs and terminals and
    /// the boats sailing between them, with everything else faded.
    fn lit_service(&self, s: usize) -> Highlight {
        let service = &self.service_routes[s];
        let serves = |id: Option<u32>| id.is_some_and(|id| service.terminals.contains(&id));
        Highlight {
            routes: service.legs.clone(),
            leg: None,
            service: Some(s),
            vessels: (0..self.fleet.vessels.len())
                .filter(|&i| {
                    let v = &self.fleet.vessels[i];
                    serves(v.departing_id) && serves(v.arriving_id)
                })
                .collect(),
            terminals: service.terminals.clone(),
            dim: true,
        }
    }

    /// Points around everything in `lit` (unit vectors), to fit the view
    /// to: route lines, and each boat and terminal as far as it's drawn
    /// out to. Empty if nothing's lit.
    fn points_around(&self, lit: &Highlight, now: f64) -> Vec<DVec3> {
        let radius = self.camera.radius;
        let mut points = Vec::new();
        // With no route lit, a lone boat or terminal is fitted with some
        // water around it rather than filling the canvas.
        let least = if lit.routes.is_empty() {
            FIT_MIN_REACH_M
        } else {
            0.0
        };
        let mut around = |p: DVec3, reach_m: f64| {
            let (east, north) = LonLat::from_vec(p).east_north();
            let k = reach_m.max(least) / radius;
            for d in [east, -east, north, -north] {
                points.push((p + d * k).normalize());
            }
        };
        for &i in &lit.vessels {
            let v = &self.fleet.vessels[i];
            around(v.position_at(now), self.fleet.size_m(v).0 / 2.0);
        }
        let terminals = &self.terminals;
        let inland = (ANCHOR_INLAND * ANCHOR_M + ANCHOR_CLEAR_M) / radius;
        for t in terminals
            .list
            .iter()
            .filter(|t| lit.terminals.contains(&t.id))
        {
            let p = t.position.to_unit();
            // Where the anchor stands, as add_ferries places it; its gauge
            // hangs below, so it reaches about 3/4 of its height around.
            let at = terminals.facing_deg(t).map_or(p, |deg| {
                let (east, north) = t.position.east_north();
                let d = deg.to_radians();
                (p - (east * d.sin() + north * d.cos()) * inland).normalize()
            });
            around(at, 0.75 * ANCHOR_M);
        }
        for &r in &lit.routes {
            points.extend_from_slice(&self.routes[r].points);
        }
        points
    }

    /// Where the view looks and how far in.
    fn pose(&self) -> Pose {
        Pose {
            focus: self.camera.eye_dir(),
            scale: self.camera.altitude,
        }
    }

    /// Looks at `pose`, within the zoom limits.
    fn set_pose(&mut self, pose: Pose) {
        self.camera.set_eye_dir(pose.focus);
        self.camera.altitude = pose.scale;
        self.camera.zoom(0.0);
    }

    /// The part of the canvas the panels leave clear (CSS px): below the
    /// search bar; on a phone, above the details sheet; on a wider
    /// screen, beside the finder's card and the debug panel when they're
    /// open.
    fn free_area(&self) -> Rect {
        let (w, h) = (
            self.canvas.client_width() as f64,
            self.canvas.client_height() as f64,
        );
        let mut free = Rect::at(0.0, 0.0, w, h);
        let rect = |el: &Element| {
            let r = el.get_bounding_client_rect();
            (r.width() > 0.0 && r.height() > 0.0)
                .then(|| Rect::at(r.x(), r.y(), r.width(), r.height()))
        };
        let gap = FIT_PANEL_GAP_PX;
        let finder = self.finder.input.closest("#finder").ok().flatten();
        if let Some(bar) = finder
            .as_ref()
            .and_then(|f| f.query_selector(".bar").ok().flatten())
            && let Some(r) = rect(&bar)
        {
            free.y0 = free.y0.max(r.y1 + gap);
        }
        if phone_layout() {
            if self.finder.matched() {
                let sheet = match self.finder.sheet() {
                    Sheet::Peek => SHEET_PEEK_PX,
                    Sheet::Half | Sheet::Full => SHEET_HALF_SHARE * h,
                };
                free.y1 = free.y1.min(h - sheet - gap);
            }
        } else {
            let open = finder
                .as_ref()
                .is_some_and(|f| f.class_list().contains("open"));
            if open
                && self.finder.matched()
                && let Some(r) = finder.as_ref().and_then(rect)
            {
                free.x0 = free.x0.max(r.x1 + gap);
            }
            if self.perf.visible
                && let Some(r) = rect(&self.perf.card)
            {
                free.x1 = free.x1.min(r.x0 - gap);
            }
        }
        // Never less than a third of the canvas either way.
        if free.x1 - free.x0 < w / 3.0 {
            (free.x0, free.x1) = (0.0, w);
        }
        if free.y1 - free.y0 < h / 3.0 {
            (free.y0, free.y1) = (0.0, h);
        }
        free
    }

    /// The pose that fits `points` (unit vectors) in `FIT_SHARE` of the
    /// free part of the canvas (`free_area`), centered in it. Sizes on
    /// screen go about as 1/scale, so each pass rescales by how far off
    /// the fit is and moves the points' box toward the free part's
    /// middle, until it settles. None without points.
    fn fit_pose(&mut self, points: &[DVec3]) -> Option<Pose> {
        let focus = points.iter().sum::<DVec3>().try_normalize()?;
        let start = self.pose();
        let (w, h) = (
            self.canvas.client_width() as f64,
            self.canvas.client_height() as f64,
        );
        let free = self.free_area();
        let (fw, fh) = (free.x1 - free.x0, free.y1 - free.y0);
        let aim = free.center();
        let mut pose = Pose {
            focus,
            scale: start.scale,
        };
        for _ in 0..FIT_ITERATIONS {
            self.set_pose(pose);
            pose = self.pose();
            // Some points beyond the horizon or off the map: back out.
            let Some(screen) = points
                .iter()
                .map(|&p| self.screen_px(p))
                .collect::<Option<Vec<_>>>()
            else {
                pose.scale *= 2.0;
                continue;
            };
            let (mut min, mut max) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
            for (x, y) in screen {
                min = (min.0.min(x), min.1.min(y));
                max = (max.0.max(x), max.1.max(y));
            }
            let mid = ((min.0 + max.0) / 2.0, (min.1 + max.1) / 2.0);
            let factor = ((max.0 - min.0) / (FIT_SHARE * fw))
                .max((max.1 - min.1) / (FIT_SHARE * fh))
                .max(1e-3);
            let settled =
                (factor - 1.0).abs() < 0.005 && (mid.0 - aim.0).hypot(mid.1 - aim.1) < 0.5;
            // Centering the view on what's under `under` moves the points'
            // middle onto `aim`.
            let under = (mid.0 + w / 2.0 - aim.0, mid.1 + h / 2.0 - aim.1);
            pose = Pose {
                focus: self.surface_at(under).unwrap_or(pose.focus),
                scale: pose.scale * factor,
            };
            if settled {
                break;
            }
        }
        self.set_pose(pose);
        let fitted = self.pose();
        self.set_pose(start);
        Some(fitted)
    }

    /// Flies the view to fit what the pointer has highlighted; says
    /// whether anything is.
    fn fly_to_focus(&mut self) -> bool {
        let points = std::mem::take(&mut self.focus_points);
        self.fly_to(&points)
    }

    /// Flies the view to fit `points` (unit vectors); says whether there
    /// were any. With reduced motion asked for, it goes there at once.
    fn fly_to(&mut self, points: &[DVec3]) -> bool {
        let Some(to) = self.fit_pose(points) else {
            return false;
        };
        if prefers_reduced_motion() {
            self.set_pose(to);
        } else {
            self.flight = Some(Flight {
                from: self.pose(),
                to,
                started_ms: None,
            });
        }
        self.redraw = true;
        true
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

    /// HUD lines for the route of leg `r`: its name, the leg if it has
    /// several, and the boats on it.
    fn describe_route(&self, r: usize, lit: &Highlight) -> String {
        let name_of = |id: u32| {
            let t = self.terminals.list.iter().find(|t| t.id == id);
            t.map_or_else(|| id.to_string(), |t| t.name.clone())
        };
        let service = lit.service.map(|s| &self.service_routes[s]);
        let mut lines = vec![match service {
            Some(s) => format!("{} route", s.name),
            None => self.routes[r].name.clone(),
        }];
        let joins = |i: &usize| self.legs[*i].joins().is_some();
        let several = service.is_some_and(|s| s.legs.iter().filter(|i| joins(i)).count() > 1);
        if several && let Some((a, b)) = self.legs[r].joins() {
            lines.push(format!("This leg: {} ↔ {}", name_of(a), name_of(b)));
        }
        let mut boats: Vec<&str> = lit
            .vessels
            .iter()
            .map(|&i| self.fleet.vessels[i].name.as_str())
            .collect();
        boats.sort_unstable();
        lines.push(match boats.len() {
            0 => "No boats on it now".to_string(),
            _ => format!("On it now: {}", boats.join(", ")),
        });
        lines.join("\n")
    }

    /// Where unit vector `p` is drawn, in CSS px from the canvas's top
    /// left, if it's on the visible side of the planet.
    fn screen_px(&self, p: DVec3) -> Option<(f64, f64)> {
        let (w, h) = (
            self.canvas.client_width() as f64,
            self.canvas.client_height() as f64,
        );
        let radius = self.camera.radius;
        if p.dot(self.camera.eye()) <= radius {
            return None;
        }
        let clip = self.camera.view_proj() * (p * radius).extend(1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let (x, y) = (clip.x / clip.w, clip.y / clip.w);
        Some(((x + 1.0) / 2.0 * w, (1.0 - y) / 2.0 * h))
    }

    /// The debug switch in the settings: the debug panel.
    fn set_debug(&mut self, on: bool) {
        if on {
            self.advice_check
                .want("check", "api/v1/stats/advice".to_string());
        }
        self.set_perf_visible(on);
        self.redraw = true;
    }

    /// The debug panel's switch: the names' placement drawn over the map.
    fn set_label_debug(&mut self, on: bool) {
        self.labels.set_debug(on);
        self.redraw = true;
    }

    fn set_perf_visible(&mut self, visible: bool) {
        self.perf.visible = visible;
        if visible {
            let _ = self.perf.card.remove_attribute("hidden");
            self.update_perf_panel();
        } else {
            let _ = self.perf.card.set_attribute("hidden", "");
        }
    }

    /// The performance panel's rows, as (label, value).
    fn perf_rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        let mut row = |label, value: String| rows.push((label, value));
        match self.perf.stats.summary() {
            _ if self.idle => row("FPS", "idle: redraws only when something changes".into()),
            Some(s) => {
                row("FPS", format!("{:.1}", s.fps));
                row(
                    "Frame time",
                    format!("{:.1} ms (worst {:.1})", s.frame_ms, s.worst_frame_ms),
                );
                row(
                    "CPU per frame",
                    format!("{:.2} ms (worst {:.2})", s.cpu_ms, s.worst_cpu_ms),
                );
            }
            None => row("FPS", "measuring".into()),
        }
        // What's needed to see what was seen: where the view is (its
        // middle, how far it spans corner to corner, as the terminal
        // symbols scale by, and the camera's height), a link that opens
        // it, the window, what's shown and selected, and when.
        let (center, m_per_px, reach_m) = self.view_scale();
        let km = |m: f64| {
            if m >= 10_000.0 {
                format!("{:.0} km", m / 1000.0)
            } else {
                format!("{:.2} km", m / 1000.0)
            }
        };
        row(
            "View",
            format!(
                "{}; {} corner to corner, {:.1} m/px",
                center.display(),
                km(2.0 * reach_m),
                m_per_px
            ),
        );
        row(
            "Altitude",
            format!(
                "{}; terminal symbols at {:.0}%",
                km(self.camera.altitude),
                100.0 * symbol_scale(2.0 * reach_m)
            ),
        );
        let fragment = view_fragment(center, self.camera.altitude);
        let page = web_sys::window()
            .and_then(|w| w.location().href().ok())
            .map(|href| href.split('#').next().unwrap_or_default().to_string())
            .unwrap_or_default();
        row("Link", format!("{page}#{fragment}"));
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        row(
            "Window",
            format!(
                "{} × {} CSS px, DPR {dpr}",
                self.canvas.client_width(),
                self.canvas.client_height()
            ),
        );
        let style = STYLE_CHOICES
            .iter()
            .find(|&&(_, s)| s == self.style)
            .map_or("?", |&(name, _)| name);
        let layers: Vec<&str> = LAYER_TOGGLES
            .iter()
            .filter(|&&(_, bit)| self.layers & bit != 0)
            .map(|&(id, _)| id.trim_start_matches("layer-"))
            .collect();
        row(
            "Map",
            format!(
                "{style}; layers: {}",
                if layers.is_empty() {
                    "none".to_string()
                } else {
                    layers.join(", ")
                }
            ),
        );
        row(
            "Selected",
            self.finder.entry().map_or_else(
                || "nothing".to_string(),
                |e| format!("{} {}", e.kind, e.name),
            ),
        );
        row("Data", self.fleet_status.clone());
        // How the arrival advice held lately, over all terminals.
        if let Some(check) = self.advice_check.get("check") {
            let n: u32 = check.terminals.iter().map(|t| t.sailings).sum();
            let weighted = |f: fn(&ferry_model::api::TerminalCheck) -> f64| {
                check
                    .terminals
                    .iter()
                    .map(|t| f(t) * f64::from(t.sailings))
                    .sum::<f64>()
                    / f64::from(n.max(1))
            };
            row(
                "Advice check",
                if n == 0 {
                    format!("no sailings to check in {} days", check.days)
                } else {
                    format!(
                        "held {:.0}% of {n} sailings (gave {:.0}%), {} days",
                        100.0 * weighted(|t| t.held),
                        100.0 * weighted(|t| t.predicted),
                        check.days
                    )
                },
            );
        }
        row(
            "Time",
            js_sys::Date::new_0()
                .to_iso_string()
                .as_string()
                .unwrap_or_default(),
        );
        if self.layers & LAYER_CELLS != 0 {
            row(
                "Hex grid",
                format!(
                    "resolution {}, {} cells",
                    self.hex.resolution,
                    group_thousands(self.hex.cells as u64)
                ),
            );
        }
        row(
            "Canvas",
            format!(
                "{} × {} device px",
                self.canvas.width(),
                self.canvas.height()
            ),
        );
        let t = &self.terrain;
        row(
            "Terrain",
            format!(
                "{} regions ({} on stand-ins) · {}/{TERRAIN_SLOTS} tiles cached · {} loading · {} \
                 available",
                t.drawn,
                t.stand_ins,
                t.cached(),
                t.loading(),
                group_thousands(t.available() as u64)
            ),
        );
        let c = self.renderer.counts;
        row("Draw calls", c.draw_calls.to_string());
        row("Triangles", group_thousands(c.triangles));
        row("Line segments", group_thousands(c.line_segments));
        row(
            "GPU memory",
            format!("≈ {}", format_bytes(self.renderer.gpu_bytes())),
        );
        rows
    }

    fn update_perf_panel(&self) {
        let html: String = self
            .perf_rows()
            .iter()
            .map(|(label, value)| format!("<dt>{label}</dt><dd>{value}</dd>"))
            .collect();
        self.perf.body.set_inner_html(&format!("<dl>{html}</dl>"));
    }

    /// Copies the performance panel as text, with the browser it ran in.
    fn copy_perf(&self) {
        let mut text = String::from("ferry-demo debug\n");
        for (label, value) in self.perf_rows() {
            text += &format!("{label}: {value}\n");
        }
        if let Some(agent) = web_sys::window().and_then(|w| w.navigator().user_agent().ok()) {
            text += &format!("Browser: {agent}\n");
        }
        copy_to_clipboard(&text, &self.perf.copy);
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

    /// The view's center on the ground, the ground meters across one CSS
    /// pixel there, and about how far (m) the view reaches from its center.
    fn view_scale(&self) -> (LonLat, f64, f64) {
        let height_px = self.canvas.client_height().max(1) as f64;
        let corner = (1.0 + self.camera.aspect * self.camera.aspect).sqrt();
        let center = LonLat::from_vec(self.camera.eye_dir());
        let half_m = self.camera.altitude * (self.camera.fov_y / 2.0).tan();
        (center, 2.0 * half_m / height_px, half_m * corner)
    }

    /// Rebuilds the hex grid's lines if the view calls for it (see
    /// `HexGrid`), and keeps the resolution current for the HUD.
    fn update_hex_grid(&mut self) {
        let (center, m_per_px, reach) = self.view_scale();
        let wanted = resolution_for(m_per_px);
        if self.layers & LAYER_CELLS == 0 {
            self.hex.built_for = None;
            self.set_hex_resolution(wanted);
            return;
        }
        let anchor = wanted.pred().unwrap_or(wanted);
        let size = (self.canvas.client_width(), self.canvas.client_height());
        let built_for = (wanted, h3_cell(center, anchor), size);
        // Zooming out past the margin needs more cells; zooming in after
        // a build that had to settle for a coarser resolution than wanted
        // may now fit the wanted one.
        let zoomed_out = m_per_px > self.hex.built_m_per_px * HEX_COVER;
        let settled = self.hex.resolution != wanted;
        let zoomed_in = m_per_px < self.hex.built_m_per_px / HEX_COVER;
        if self.hex.built_for == Some(built_for) && !zoomed_out && !(settled && zoomed_in) {
            return;
        }
        // Cover HEX_COVER times the view, plus room to pan anywhere in the
        // anchor cell, but no further than the slice's far edge.
        let pan_m = 2.0 * edge_m(anchor);
        let to_far_edge = center
            .to_unit()
            .angle_between(KITSAP_SLICE.center.to_unit())
            * self.camera.radius
            + KITSAP_SLICE.radius_m;
        let reach = (reach * HEX_COVER + pan_m).min(to_far_edge);
        let visible = self.screen_test(HEX_COVER, pan_m / m_per_px);
        let (res, cells) = cells_around(center, reach, wanted, &KITSAP_SLICE, visible);
        let mut lines = Scene::lines(
            &outlines(&cells, &KITSAP_SLICE),
            self.camera.radius,
            GRID_COLOR,
        );
        // Cells along the rim reach past it; keep only the lines inside.
        let inside = |p: [f32; 3]| KITSAP_SLICE.contains(glam::Vec3::from_array(p).as_dvec3());
        lines.retain(|l| inside(l.a) && inside(l.b));
        self.renderer.set_overlay(LAYER_CELLS, &lines);
        self.hex.built_for = Some(built_for);
        self.hex.built_m_per_px = m_per_px;
        self.hex.cells = cells.len();
        self.set_hex_resolution(res);
    }

    /// A test for whether a point (unit vector) is on screen, with the
    /// screen scaled by `scale` about its center and grown by `margin_px`
    /// CSS pixels on every side.
    fn screen_test(&self, scale: f64, margin_px: f64) -> impl Fn(DVec3) -> bool + use<> {
        let (w, h) = (
            self.canvas.client_width().max(1) as f64,
            self.canvas.client_height().max(1) as f64,
        );
        let x_limit = scale + 2.0 * margin_px / w;
        let y_limit = scale + 2.0 * margin_px / h;
        let radius = self.camera.radius;
        let eye = self.camera.eye();
        let view_proj = self.camera.view_proj();
        // On the near side, and inside the scaled screen.
        move |p: DVec3| {
            let clip = view_proj * (p * radius).extend(1.0);
            p.dot(eye) > radius
                && clip.w > 0.0
                && (clip.x / clip.w).abs() <= x_limit
                && (clip.y / clip.w).abs() <= y_limit
        }
    }

    fn set_hex_resolution(&mut self, res: Resolution) {
        if self.hex.resolution != res {
            self.hex.resolution = res;
            self.update_legend();
        }
    }

    /// The current measurement, finished or still following the cursor.
    fn reading(&self) -> Option<Reading> {
        let (a, b) = self.measure.span(self.measure_end)?;
        Some(Reading::new(a, b, self.camera.radius))
    }

    /// Switches the measuring tool on or off; off clears the measurement.
    /// Opens or closes the map settings.
    fn set_settings_open(&self, open: bool) {
        let _ = self.settings.toggle_attribute_with_force("hidden", !open);
        let _ = self
            .settings_toggle
            .set_attribute("aria-expanded", if open { "true" } else { "false" });
    }

    fn set_measuring(&mut self, on: bool) {
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

    fn update_measure_box(&self) {
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

    fn copy_measurement(&self) {
        if let Some(r) = self.reading() {
            copy_to_clipboard(&r.clipboard_text(), &self.measure_box.copy);
        }
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

/// Toward the sun (unit vector), from the slice center's horizon.
fn sun_direction() -> DVec3 {
    let center = KITSAP_SLICE.center;
    let (east, north) = center.east_north();
    let (az, el) = (SUN_AZIMUTH_DEG.to_radians(), SUN_ELEVATION_DEG.to_radians());
    let horizontal = north * az.cos() + east * az.sin();
    (center.to_unit() * el.sin() + horizontal * el.cos()).normalize()
}

/// Whether the page's tab is in the background (or the window minimized).
fn page_hidden() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| d.hidden())
}

async fn sleep_ms(ms: f64) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ =
                window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

fn set_text(el: &Element, text: &str) {
    if el.text_content().as_deref() != Some(text) {
        el.set_text_content(Some(text));
    }
}

/// Milliseconds from the page's high-resolution clock.
/// Map paper, for docked and laid-up hulls, as linear RGBA.
fn ink_paper(alpha: f32) -> [f32; 4] {
    palette::linear_rgba(palette::VESSEL_PAPER, alpha)
}

/// Where the view looks: the point at the screen center (a unit vector),
/// and how far in, as the camera's altitude (m). Sizes on screen go about
/// as 1/scale.
#[derive(Clone, Copy)]
struct Pose {
    focus: DVec3,
    scale: f64,
}

/// The view moving from one pose to another, eased in and out: along the
/// great circle between the focuses, and zooming evenly in log scale.
struct Flight {
    from: Pose,
    to: Pose,
    /// The animation frame time it began, from its first frame.
    started_ms: Option<f64>,
}

impl Flight {
    /// The pose `t` (0 to 1) of the way along.
    fn at(&self, t: f64) -> Pose {
        let e = if t < 0.5 {
            4.0 * t * t * t
        } else {
            1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
        };
        let (a, b) = (self.from.focus, self.to.focus);
        let angle = a.dot(b).clamp(-1.0, 1.0).acos();
        let focus = if angle < 1e-9 {
            b
        } else {
            (a * ((1.0 - e) * angle).sin() + b * (e * angle).sin()) / angle.sin()
        };
        let scale = (self.from.scale.ln() * (1.0 - e) + self.to.scale.ln() * e).exp();
        Pose { focus, scale }
    }
}

/// Whether the viewer has asked for less motion.
fn prefers_reduced_motion() -> bool {
    media_matches("(prefers-reduced-motion: reduce)")
}

/// Whether the page is laid out for a phone (index.html's narrow layout).
fn phone_layout() -> bool {
    media_matches("(max-width: 700px)")
}

fn media_matches(query: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.match_media(query).ok().flatten())
        .is_some_and(|q| q.matches())
}

/// A leg of a ferry route as drawn: a name for the HUD, and its points as
/// unit vectors.
struct RouteLine {
    name: String,
    points: Vec<DVec3>,
}

/// What's highlighted: routes by index, boats by index into the fleet,
/// terminals by WSF id.
#[derive(Default)]
struct Highlight {
    /// Fade what isn't highlighted (for a route).
    dim: bool,
    routes: Vec<usize>,
    /// The leg under the cursor, and the route it's part of (by index
    /// into `App::service_routes`).
    leg: Option<usize>,
    service: Option<usize>,
    vessels: BTreeSet<usize>,
    terminals: BTreeSet<u32>,
}

/// What the cursor is over, by index into the fleet or the terminals.
#[derive(Clone, Copy, PartialEq)]
enum Picked {
    Vessel(usize),
    Terminal(usize),
}

/// A name to set beside its boat or terminal's symbol (`symbol`, its box
/// on screen in CSS px), on the side it would rather be (see placement).
/// `key` names the symbol across frames; `class` styles it (index.html);
/// names with a lower `rank` are placed first.
struct Label {
    key: String,
    text: String,
    symbol: Rect,
    prefer: Side,
    rank: u8,
    class: &'static str,
    /// Faded back while something else is highlighted.
    dim: bool,
    /// Its symbol is highlighted: the name is too.
    lit: bool,
}

/// The names over the map (#labels), one element per boat or terminal,
/// with a leader line for each name away from its first choice of place;
/// kept between frames and moved as the map redraws.
struct Labels {
    root: Element,
    spans: HashMap<String, Element>,
    leaders: HashMap<String, Element>,
    /// Each name's size on screen (CSS px), measured once for its text
    /// and style.
    sizes: HashMap<String, (String, (f64, f64))>,
    /// Each name's slot last frame, which it keeps while it can.
    slots: HashMap<String, usize>,
    /// Draws the symbols' boxes, the names' slots and what they avoid
    /// (the debug switch), in #labels-debug.
    debug: Option<Element>,
}

impl Labels {
    /// Shows `labels` beside their symbols, clear of `obstacles` and each
    /// other where they can be, and hides the rest. `dots` are the notice
    /// dots' slots and the one each took, for the debug drawing.
    fn place(&mut self, labels: &[Label], obstacles: &[Rect], dots: &[(Vec<Rect>, usize)]) {
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
    fn set_debug(&mut self, on: bool) {
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

/// A notice to show at a dot: what it's about, and each notice in it.
struct Note {
    /// Names the boat or terminal across frames.
    key: String,
    /// The dot's center and diameter, in CSS px.
    at: (f64, f64),
    size: f64,
    /// Faded back while something else is highlighted.
    dim: bool,
    heading: String,
    items: Vec<NoteItem>,
}

struct NoteItem {
    title: String,
    /// Paragraphs, blank lines apart; may be empty.
    text: String,
    when: Option<String>,
}

/// The notice dots over the map (#notes): each a button with a panel that
/// shows on hover or keyboard focus, and stays open once clicked, until
/// clicked again, Escape, or a click on the map. Built with text nodes
/// only: WSF's text never becomes markup.
struct Notes {
    root: Element,
    /// Each note's element, and the content it was built with.
    items: HashMap<String, (Element, String)>,
    open: Option<String>,
}

impl Notes {
    /// Shows `notes` in a window `size` CSS px, and hides the rest.
    fn place(&mut self, notes: &[Note], size: (f64, f64)) {
        let document = self.root.owner_document().expect("document");
        for note in notes {
            let content = note_content(note);
            let (el, built) = self.items.entry(note.key.clone()).or_insert_with(|| {
                let el = document.create_element("div").expect("div");
                let _ = el.set_attribute("data-key", &note.key);
                let _ = self.root.append_child(&el);
                (el, String::new())
            });
            if *built != content {
                build_note(&document, el, note);
                *built = content;
            }
            let (x, y) = note.at;
            let mut class = String::from("note");
            if x > size.0 - NOTE_FLIP_PX {
                class += " flip";
            }
            if y > size.1 * NOTE_UP_FRACTION {
                class += " up";
            }
            if self.open.as_deref() == Some(note.key.as_str()) {
                class += " open";
            }
            if note.dim {
                class += " dim";
            }
            el.set_class_name(&class);
            let _ = el.set_attribute(
                "style",
                &format!(
                    "transform: translate({x:.1}px, {y:.1}px); --dot: {:.2}px",
                    note.size
                ),
            );
            let _ = el.remove_attribute("hidden");
        }
        for (key, (el, _)) in &self.items {
            if !notes.iter().any(|n| n.key == *key) {
                let _ = el.set_attribute("hidden", "");
            }
        }
        self.mark_open();
    }

    /// Opens the note `key`, or closes it if it's open.
    fn toggle(&mut self, key: &str) {
        self.open = if self.open.as_deref() == Some(key) {
            None
        } else {
            Some(key.to_string())
        };
        self.mark_open();
    }

    /// Closes the open note; says whether one was.
    fn close(&mut self) -> bool {
        let was_open = self.open.take().is_some();
        self.mark_open();
        was_open
    }

    fn mark_open(&self) {
        for (key, (el, _)) in &self.items {
            let open = self.open.as_deref() == Some(key.as_str());
            let _ = el.class_list().toggle_with_force("open", open);
            if let Some(dot) = el.first_element_child() {
                let _ = dot.set_attribute("aria-expanded", if open { "true" } else { "false" });
            }
        }
    }
}

/// What a note's element shows, to tell when it needs building again.
fn note_content(note: &Note) -> String {
    let mut content = note.heading.clone();
    for item in &note.items {
        for part in [&item.title, &item.text, item.when.as_deref().unwrap_or("")] {
            content.push('\u{1f}');
            content += part;
        }
    }
    content
}

/// Fills `el` with a note's dot and panel.
fn build_note(document: &web_sys::Document, el: &Element, note: &Note) {
    let child = |parent: &Element, tag: &str, class: &str, text: &str| {
        let e = document.create_element(tag).expect("element");
        if !class.is_empty() {
            e.set_class_name(class);
        }
        if !text.is_empty() {
            e.set_text_content(Some(text));
        }
        let _ = parent.append_child(&e);
        e
    };
    el.set_text_content(None);
    let dot = child(el, "button", "dot", "i");
    let _ = dot.set_attribute("type", "button");
    let count = match note.items.len() {
        1 => "a notice".to_string(),
        n => format!("{n} notices"),
    };
    let _ = dot.set_attribute("aria-label", &format!("{}: {count}", note.heading));
    let panel = child(el, "div", "panel", "");
    child(&panel, "div", "about", &note.heading);
    for item in &note.items {
        let section = child(&panel, "section", "", "");
        child(&section, "h3", "", &item.title);
        if let Some(when) = &item.when {
            child(&section, "div", "when", when);
        }
        if !item.text.is_empty() {
            child(&section, "p", "", &item.text);
        }
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A time (ms since the epoch) on the ferries' clock, e.g. "11:31 AM".
fn clock(epoch_ms: f64) -> String {
    let options = js_sys::Object::new();
    for (key, value) in [
        ("hour", "numeric"),
        ("minute", "2-digit"),
        ("timeZone", FERRY_TIME_ZONE),
    ] {
        let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    js_sys::Date::new(&JsValue::from_f64(epoch_ms))
        .to_locale_time_string_with_options("en-US", &options)
        .into()
}

/// A date and time on the ferries' clock, e.g. "Oct 29, 11:31 AM".
fn date_clock(epoch_ms: f64) -> String {
    let options = js_sys::Object::new();
    for (key, value) in [
        ("month", "short"),
        ("day", "numeric"),
        ("hour", "numeric"),
        ("minute", "2-digit"),
        ("timeZone", FERRY_TIME_ZONE),
    ] {
        let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    js_sys::Date::new(&JsValue::from_f64(epoch_ms))
        .to_locale_string("en-US", &options)
        .into()
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map_or(0.0, |p| p.now())
}

/// Puts `text` on the clipboard and briefly swaps `button`'s icon to a
/// check mark (see `.copied` in index.html).
fn copy_to_clipboard(text: &str, button: &Element) {
    let Some(window) = web_sys::window() else {
        return;
    };
    // The promise rejects only without permission; there is nothing more
    // useful to do then than skip the check mark.
    let _ = window.navigator().clipboard().write_text(text);
    let _ = button.class_list().add_1("copied");
    let button = button.clone();
    let reset = Closure::once_into_js(move || {
        let _ = button.class_list().remove_1("copied");
    });
    let _ = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(reset.unchecked_ref(), COPIED_MS);
}

async fn run() -> Result<(), String> {
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
            // Clicks within an open match's details (scrolling notices) stay.
            let on_row = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
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

/// Register `handler` for `event` on `target` for the life of the page.
fn listen<E: JsCast + 'static>(
    target: &web_sys::EventTarget,
    event: &str,
    app: &Rc<RefCell<App>>,
    handler: impl Fn(&mut App, E) + 'static,
) {
    let app = app.clone();
    let closure = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        handler(&mut app.borrow_mut(), e.unchecked_into());
    });
    target
        .add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())
        .expect("addEventListener");
    closure.forget();
}

/// Drive `frame` from requestAnimationFrame. The closure re-schedules itself.
fn animate(app: Rc<RefCell<App>>) {
    let callback: FrameCallback = Rc::new(RefCell::new(None));
    let next = callback.clone();
    *callback.borrow_mut() = Some(Closure::new(move |time_ms: f64| {
        app.borrow_mut().frame(time_ms);
        request_frame(next.borrow().as_ref().unwrap());
    }));
    request_frame(callback.borrow().as_ref().unwrap());
}

fn request_frame(f: &Closure<dyn FnMut(f64)>) {
    web_sys::window()
        .unwrap()
        .request_animation_frame(f.as_ref().unchecked_ref())
        .expect("requestAnimationFrame");
}

fn show_error(message: &str) {
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("error"))
    {
        el.set_text_content(Some(message));
        let _ = el.remove_attribute("hidden");
    }
}
