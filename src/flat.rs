//! The map without a GPU: Canvas2D drawing the baked basemap (basemap.rs)
//! under the same lines and symbols the GPU draws (`Scene`), through the
//! same camera, so it reads as the same map. It draws in physical pixels,
//! as the GPU does, so the scene's sizes carry over as they are.

use std::cell::Cell;
use std::collections::HashMap;
use std::f64::consts::FRAC_PI_2;
use std::rc::Rc;

use glam::{DMat4, DVec2, DVec3, Vec3};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlImageElement};

use crate::basemap::{Grid, TILE_PX};
use crate::camera::MapCamera;
use crate::palette::css_rgba;
use crate::renderer::{FrameCounts, Globals, OVERLAYS, displayed_size};
use crate::scene::{Marker, MarkerKind, Scene, Segment};
use crate::terrain::{Manifest, TileKey};

/// What shows where there's no basemap: the GPU's clear color.
const BACKGROUND: &str = "#0d1016";
/// Basemap images kept loaded: twice what covers the screen, and the
/// coarse levels' (at most 21), within these bounds. Decoded, each is
/// 1 MiB, which a phone's browser counts against the tab.
const MIN_IMAGES: usize = 32;
const MAX_IMAGES: usize = 160;
/// Basemap images fetched at once.
const MAX_LOADING: usize = 8;
/// Coarse levels always fetched for the view, so something shows at once.
const ALWAYS_LEVEL: u8 = 2;
/// As in lines.wgsl: the share of each dash period drawn.
const DASH_ON: f64 = 0.6;
/// As in markers.wgsl.
const TAPER: f64 = 1.6;
const GLOW: f64 = 0.25;
const HEADING_PROBE_M: f64 = 200.0;
const DASH_PX: f64 = 4.0;
/// Points along each side of a hull's outline.
const HULL_STEPS: usize = 16;

struct Image {
    element: HtmlImageElement,
    /// Set by its load (or error) handler.
    state: Rc<Cell<Load>>,
    /// The frame it was last drawn or wanted in.
    used: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Load {
    Loading,
    Ready,
    Failed,
}

pub struct FlatRenderer {
    canvas: HtmlCanvasElement,
    context: CanvasRenderingContext2d,
    size: (u32, u32),
    grid: Grid,
    /// The basemap tiles there are (none without a bake).
    manifest: Manifest,
    images: HashMap<TileKey, Image>,
    /// An image arrived since the last frame: draw again.
    arrived: Rc<Cell<bool>>,
    /// Line overlays in the order of `OVERLAYS`.
    overlays: Vec<Vec<Segment>>,
    frame: u64,
    /// How many images to keep, for the screen's size (see `MAX_IMAGES`).
    keep: usize,
    pub counts: FrameCounts,
}

impl FlatRenderer {
    pub fn new(canvas: HtmlCanvasElement, grid: Grid, manifest: Manifest) -> Result<Self, String> {
        let context = canvas
            .get_context("2d")
            .ok()
            .flatten()
            .and_then(|c| c.dyn_into::<CanvasRenderingContext2d>().ok())
            .ok_or("no 2D canvas")?;
        Ok(Self {
            canvas,
            context,
            size: (0, 0),
            grid,
            manifest,
            images: HashMap::new(),
            arrived: Rc::default(),
            overlays: vec![Vec::new(); OVERLAYS.len()],
            frame: 0,
            keep: MAX_IMAGES,
            counts: FrameCounts::default(),
        })
    }

    /// Resizes the canvas to its displayed size if it changed, and returns
    /// the size in physical pixels.
    pub fn sync_size(&mut self) -> (u32, u32) {
        let wanted = displayed_size(&self.canvas);
        if wanted != self.size {
            self.canvas.set_width(wanted.0);
            self.canvas.set_height(wanted.1);
            self.size = wanted;
        }
        self.size
    }

    /// Whether there's something new to draw: a resize, or images in.
    pub fn needs_frame(&self) -> bool {
        self.arrived.get() || displayed_size(&self.canvas) != self.size
    }

    /// Bytes of the images held, decoded.
    pub fn image_bytes(&self) -> u64 {
        let ready = self
            .images
            .values()
            .filter(|i| i.state.get() == Load::Ready);
        ready.count() as u64 * u64::from(TILE_PX * TILE_PX) * 4
    }

    /// Replace the lines of the overlay drawn for `layer` (one of
    /// `OVERLAYS`).
    pub fn set_overlay(&mut self, layer: u32, segments: &[Segment]) {
        if let Some(i) = OVERLAYS.iter().position(|&l| l == layer) {
            self.overlays[i] = segments.to_vec();
        }
    }

    pub fn render(&mut self, camera: &MapCamera, globals: &Globals, scene: &Scene) {
        self.frame += 1;
        self.arrived.set(false);
        let (w, h) = (f64::from(self.size.0), f64::from(self.size.1));
        self.context.set_fill_style_str(BACKGROUND);
        self.context.fill_rect(0.0, 0.0, w, h);
        let mut counts = FrameCounts {
            draw_calls: self.draw_basemap(camera, w, h),
            ..Default::default()
        };
        let ctx = &self.context;

        let view = View {
            view_proj: camera.view_proj(),
            size: DVec2::new(w, h),
            line_px: f64::from(globals.params[3]),
            // CSS pixels' meters, and physical pixels to a CSS pixel.
            dash_m_per_px: f64::from(globals.terrain[1]),
            dpr: h / camera_height_css(&self.canvas),
        };
        let visible = OVERLAYS
            .iter()
            .zip(&self.overlays)
            .filter(|(layer, _)| globals.flags[0] & **layer != 0)
            .map(|(_, lines)| lines.as_slice())
            .chain([scene.segments.as_slice()]);
        for lines in visible {
            counts.line_segments += lines.len() as u64;
            draw_lines(ctx, &view, lines);
        }
        for column in &scene.columns {
            let p = view.px(Vec3::from_array(column.base).as_dvec3());
            let r = f64::from(column.width) * view.size.y / (2.0 * camera.half_height_m());
            ctx.set_fill_style_str(&css_rgba(column.color));
            ctx.begin_path();
            let _ = ctx.arc(p.x, p.y, r.max(1.5), 0.0, std::f64::consts::TAU);
            ctx.fill();
        }
        for marker in &scene.markers {
            draw_marker(ctx, &view, marker);
        }
        self.counts = counts;
        self.evict();
    }

    /// Draws the basemap under the view: each tile at the level as sharp
    /// as the screen, or a coarser one's part while it loads. Returns how
    /// many images were drawn.
    fn draw_basemap(&mut self, camera: &MapCamera, w: f64, h: f64) -> u32 {
        let m_per_px = camera.m_per_px(h);
        let center = camera.center();
        let half = DVec2::new(w, h) * m_per_px / 2.0;
        let (min, max) = (center - half, center + half);
        let z = self.grid.level_for(m_per_px);
        for coarse in 0..=ALWAYS_LEVEL.min(z) {
            for key in self.grid.covering(coarse, min, max).collect::<Vec<_>>() {
                self.want(key);
            }
        }
        let mut drawn = 0;
        let shown: Vec<TileKey> = self.grid.covering(z, min, max).collect();
        let coarse: usize = (0..=ALWAYS_LEVEL.min(z)).map(|z| 1 << (2 * z)).sum();
        self.keep = (2 * shown.len() + coarse).clamp(MIN_IMAGES, MAX_IMAGES);
        for key in shown {
            self.want(key);
            // The tile itself, or the nearest ancestor that's in.
            let mut source = Some(key);
            while let Some(s) = source {
                if self.ready(s) {
                    break;
                }
                source = s.parent();
            }
            let Some(source) = source else { continue };
            let (offset, scale) = key.within(source);
            let tile_px = f64::from(TILE_PX);
            let (sw, s_xy) = (scale * tile_px, offset * tile_px);
            let (lo, hi) = self.grid.bounds(key);
            // Whole pixels, so neighbors meet without seams.
            let x0 = ((lo.x - min.x) / m_per_px).round();
            let x1 = ((hi.x - min.x) / m_per_px).round();
            let y0 = ((max.y - hi.y) / m_per_px).round();
            let y1 = ((max.y - lo.y) / m_per_px).round();
            let image = &self.images[&source].element;
            let _ = self
                .context
                .draw_image_with_html_image_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                    image,
                    s_xy.x,
                    s_xy.y,
                    sw,
                    sw,
                    x0,
                    y0,
                    x1 - x0,
                    y1 - y0,
                );
            drawn += 1;
        }
        drawn
    }

    fn ready(&mut self, key: TileKey) -> bool {
        match self.images.get_mut(&key) {
            Some(image) if image.state.get() == Load::Ready => {
                image.used = self.frame;
                true
            }
            _ => false,
        }
    }

    /// Starts fetching `key`'s image if there is one and it isn't in.
    fn want(&mut self, key: TileKey) {
        if let Some(image) = self.images.get_mut(&key) {
            image.used = self.frame;
            return;
        }
        let loading = self
            .images
            .values()
            .filter(|i| i.state.get() == Load::Loading);
        if !self.manifest.contains(key) || loading.count() >= MAX_LOADING {
            return;
        }
        let Ok(element) = HtmlImageElement::new() else {
            return;
        };
        let state = Rc::new(Cell::new(Load::Loading));
        for (event, outcome) in [("load", Load::Ready), ("error", Load::Failed)] {
            let (state, arrived) = (state.clone(), self.arrived.clone());
            let handler = Closure::once_into_js(move || {
                state.set(outcome);
                arrived.set(true);
            });
            let _ = element.add_event_listener_with_callback(event, handler.unchecked_ref());
        }
        element.set_src(&key.url("basemap", "webp"));
        let used = self.frame;
        self.images.insert(
            key,
            Image {
                element,
                state,
                used,
            },
        );
    }

    /// Lets go of the images used longest ago, beyond `keep`.
    fn evict(&mut self) {
        if self.images.len() <= self.keep {
            return;
        }
        let mut by_age: Vec<(u64, TileKey)> = (self.images.iter())
            .filter(|(_, i)| i.state.get() != Load::Loading && i.used < self.frame)
            .map(|(k, i)| (i.used, *k))
            .collect();
        by_age.sort();
        let excess = self.images.len() - self.keep;
        for (_, key) in by_age.into_iter().take(excess) {
            self.images.remove(&key);
        }
    }
}

/// The canvas's height in CSS pixels.
fn camera_height_css(canvas: &HtmlCanvasElement) -> f64 {
    f64::from(canvas.client_height().max(1))
}

/// Where things go on the canvas, and the sizes the shaders draw with.
struct View {
    view_proj: DMat4,
    /// The canvas in physical pixels.
    size: DVec2,
    /// The standard line width (physical px).
    line_px: f64,
    dash_m_per_px: f64,
    dpr: f64,
}

impl View {
    /// Where the world position `p` (m) is on the canvas (physical px).
    fn px(&self, p: DVec3) -> DVec2 {
        let clip = self.view_proj * p.extend(1.0);
        DVec2::new(
            (clip.x + 1.0) / 2.0 * self.size.x,
            (1.0 - clip.y) / 2.0 * self.size.y,
        )
    }
}

/// Lines as lines.wgsl draws them: a constant width on screen, dashed by
/// distance along their path. Runs of segments end to end in one style go
/// as one path.
fn draw_lines(ctx: &CanvasRenderingContext2d, view: &View, lines: &[Segment]) {
    ctx.set_line_cap("butt");
    ctx.set_line_join("round");
    let mut i = 0;
    while i < lines.len() {
        let first = &lines[i];
        let mut end = i + 1;
        while end < lines.len() {
            let (prev, next) = (&lines[end - 1], &lines[end]);
            let same =
                next.color == first.color && next.width == first.width && next.dash == first.dash;
            if !same || next.a != prev.b {
                break;
            }
            end += 1;
        }
        ctx.set_stroke_style_str(&css_rgba(first.color));
        ctx.set_line_width(f64::from(first.width) * view.line_px);
        let dash = f64::from(first.dash) * view.dpr;
        let pattern = if dash > 0.0 {
            vec![DASH_ON * dash, (1.0 - DASH_ON) * dash]
        } else {
            Vec::new()
        };
        let pattern = js_sys::Array::from_iter(pattern.into_iter().map(JsValue::from));
        let _ = ctx.set_line_dash(&pattern);
        if dash > 0.0 {
            let along_px = f64::from(first.along[0]) / view.dash_m_per_px * view.dpr;
            ctx.set_line_dash_offset(along_px);
        }
        ctx.begin_path();
        let start = view.px(Vec3::from_array(first.a).as_dvec3());
        ctx.move_to(start.x, start.y);
        for s in &lines[i..end] {
            let p = view.px(Vec3::from_array(s.b).as_dvec3());
            ctx.line_to(p.x, p.y);
        }
        ctx.stroke();
        i = end;
    }
    let _ = ctx.set_line_dash(&js_sys::Array::new());
}

/// A symbol as markers.wgsl draws it: a hull along its heading, or an
/// upright anchor with its gauge.
fn draw_marker(ctx: &CanvasRenderingContext2d, view: &View, m: &Marker) {
    let position = Vec3::from_array(m.position).as_dvec3();
    let center = view.px(position);
    let ahead = view.px(position + Vec3::from_array(m.heading).as_dvec3() * HEADING_PROBE_M);
    let along = ahead - center;
    let angle = if along.length() > 1e-6 {
        along.y.atan2(along.x)
    } else {
        -FRAC_PI_2
    };
    let size = DVec2::new(f64::from(m.size[0]), f64::from(m.size[1]));
    let [outline_px, dashed, kind, left] = m.style.map(f64::from);
    ctx.save();
    let _ = ctx.translate(center.x, center.y);
    if m.glow[3] > 0.0 {
        ctx.set_shadow_color(&css_rgba(m.glow));
        ctx.set_shadow_blur(GLOW * size.x.max(size.y));
    }
    if kind == MarkerKind::Anchor as i32 as f64 {
        // Shapes drawn with y up, as in the shader.
        let _ = ctx.scale(1.0, -1.0);
        draw_anchor(ctx, size.y, m, left);
    } else {
        let _ = ctx.rotate(angle);
        draw_hull(ctx, size, outline_px, dashed > 0.5, m);
    }
    ctx.restore();
}

/// A double-ended hull `size` (length, beam) px, lying along x: straight
/// sides, both ends tapering to a rounded point (sd_hull).
fn hull_path(ctx: &CanvasRenderingContext2d, size: DVec2) {
    let half = size / 2.0;
    let side = |x: f64| {
        let t = (x / half.x).clamp(-1.0, 1.0);
        (half.y * TAPER * (1.0 - t * t).sqrt()).min(half.y)
    };
    ctx.begin_path();
    for i in 0..=HULL_STEPS {
        let x = -half.x + size.x * i as f64 / HULL_STEPS as f64;
        ctx.line_to(x, -side(x));
    }
    for i in (0..=HULL_STEPS).rev() {
        let x = -half.x + size.x * i as f64 / HULL_STEPS as f64;
        ctx.line_to(x, side(x));
    }
    ctx.close_path();
}

fn draw_hull(
    ctx: &CanvasRenderingContext2d,
    size: DVec2,
    outline_px: f64,
    dashed: bool,
    m: &Marker,
) {
    hull_path(ctx, size);
    if outline_px > 0.0 && m.outline[3] > 0.0 {
        // Stroked twice as wide as the outline, half of it under the fill.
        ctx.set_stroke_style_str(&css_rgba(m.outline));
        ctx.set_line_width(2.0 * outline_px);
        if dashed {
            let half = JsValue::from(DASH_PX / 2.0);
            let _ = ctx.set_line_dash(&js_sys::Array::of2(&half, &half));
        }
        ctx.stroke();
        let _ = ctx.set_line_dash(&js_sys::Array::new());
        // The halo once, with the outline.
        ctx.set_shadow_color("transparent");
    }
    ctx.set_fill_style_str(&css_rgba(m.fill));
    ctx.fill();
}

/// An anchor `s` px tall (sd_anchor), and beneath it the gauge filled from
/// the left with the share of space `left` (none if negative).
fn draw_anchor(ctx: &CanvasRenderingContext2d, s: f64, m: &Marker, left: f64) {
    ctx.set_stroke_style_str(&css_rgba(m.outline));
    ctx.set_line_width(0.11 * s);
    ctx.set_line_cap("round");
    ctx.begin_path();
    let _ = ctx.arc(0.0, 0.38 * s, 0.1 * s, 0.0, std::f64::consts::TAU);
    ctx.move_to(0.0, 0.28 * s);
    ctx.line_to(0.0, -0.44 * s);
    ctx.move_to(-0.2 * s, 0.17 * s);
    ctx.line_to(0.2 * s, 0.17 * s);
    let (spread, r, cy) = (1.15, 0.36 * s, -0.08 * s);
    let down = -FRAC_PI_2;
    ctx.move_to((down - spread).cos() * r, cy + (down - spread).sin() * r);
    let _ = ctx.arc(0.0, cy, r, down - spread, down + spread);
    let tip = DVec2::new(spread.sin() * r, cy - spread.cos() * r);
    for sign in [-1.0, 1.0] {
        ctx.move_to(sign * tip.x, tip.y);
        ctx.line_to(sign * (tip.x + 0.1 * s), tip.y + 0.07 * s);
    }
    ctx.stroke();
    ctx.set_shadow_color("transparent");
    if left >= 0.0 {
        let (g, gap) = (0.24 * s, 0.18 * s);
        let (x0, y0) = (-0.45 * s, -0.5 * s - gap - g);
        ctx.set_fill_style_str(&css_rgba(m.fill));
        ctx.fill_rect(x0, y0, 0.9 * s * left.min(1.0), g);
        ctx.set_stroke_style_str(&css_rgba(m.fill));
        ctx.set_line_width(1.2);
        ctx.stroke_rect(x0, y0, 0.9 * s, g);
    }
}
