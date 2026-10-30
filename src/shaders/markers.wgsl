// Point symbols, sized by the caller to scale with the map: ferries as
// double-ended hulls lying along their heading (with an optional outline
// outside, solid or dashed), and terminals as upright anchors with a gauge
// of the next sailing's drive-up space beneath. A highlighted symbol has
// a soft halo, reaching `GLOW` of its longer side. Drawn last, over
// everything: the caller only sends markers in view.

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    // Position in the symbol's frame, in physical pixels: x along the
    // heading on screen, y across it.
    @location(0) local: vec2<f32>,
    // Hull length and beam (px).
    @location(1) @interpolate(flat) size: vec2<f32>,
    // Outline width (px), whether it's dashed (0 or 1), the shape (KIND_*),
    // and for anchors the share of space left (negative: no gauge).
    @location(2) @interpolate(flat) style: vec4<f32>,
    @location(3) fill: vec4<f32>,
    @location(4) outline: vec4<f32>,
    @location(5) glow: vec4<f32>,
};

const CORNERS = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
);
// Antialiasing ramp, and the dash period of dashed outlines, in pixels.
const AA_PX: f32 = 0.75;
const DASH_PX: f32 = 4.0;
// The ends taper over about a fifth of the length each: the hull is a
// band of the beam, cut by an ellipse this much wider than the beam.
const TAPER: f32 = 1.6;
const GLOW: f32 = 0.25;
// A point this far (m) along the heading gives its direction on screen.
const HEADING_PROBE_M: f32 = 200.0;
// Must match `MarkerKind` in scene.rs.
const KIND_HULL: f32 = 0.0;
const KIND_ANCHOR: f32 = 1.0;
// Anchors scale with the map, and so do their stroke and the gauge's
// thickness and gap below: fractions of the anchor's height.
fn anchor_stroke(s: f32) -> f32 {
    return 0.11 * s;
}
fn gauge_px(s: f32) -> f32 {
    return 0.24 * s;
}
fn gauge_gap_px(s: f32) -> f32 {
    return 0.18 * s;
}

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @location(0) position: vec3<f32>,
    @location(1) heading: vec3<f32>,
    @location(2) size: vec2<f32>,
    @location(3) style: vec4<f32>,
    @location(4) fill: vec4<f32>,
    @location(5) outline: vec4<f32>,
    @location(6) glow: vec4<f32>,
) -> VertexOut {
    let viewport = globals.params.xy;
    let center = world_to_clip(position);
    let ahead = world_to_clip(position + heading * HEADING_PROBE_M);
    let screen = (ahead.xy / ahead.w - center.xy / center.w) * viewport;
    let len = length(screen);
    let anchor = style.z == KIND_ANCHOR;
    // Hulls lie along their heading; anchors stand upright (x right, y up).
    var along = select(vec2<f32>(0.0, 1.0), screen / len, len > 1e-6);
    var across = vec2<f32>(along.y, -along.x);
    if anchor {
        along = vec2<f32>(1.0, 0.0);
        across = vec2<f32>(0.0, 1.0);
    }

    // The quad covers the shape, its outline, an anchor's gauge and any
    // halo.
    let gauge = select(0.0, gauge_gap_px(size.y) + gauge_px(size.y), anchor);
    let halo = select(0.0, glow_px(size), glow.a > 0.0);
    let local = CORNERS[vi] * (size / 2.0 + style.x + AA_PX + gauge + halo);
    let offset_px = along * local.x + across * local.y;
    var out: VertexOut;
    // In front of everything (depth 0), at the marker's place on screen.
    out.clip = vec4<f32>(center.xy + offset_px * 2.0 / viewport * center.w, 0.0, center.w);
    out.local = local;
    out.size = size;
    out.style = style;
    out.fill = fill;
    out.outline = outline;
    out.glow = glow;
    return out;
}

fn glow_px(size: vec2<f32>) -> f32 {
    return GLOW * max(size.x, size.y);
}

// Approximate signed distance (px) to the ellipse with semi-axes r.
fn sd_ellipse(p: vec2<f32>, r: vec2<f32>) -> f32 {
    let k0 = length(p / r);
    let k1 = length(p / (r * r));
    return k0 * (k0 - 1.0) / max(k1, 1e-6);
}

// Signed distance (px) to a double-ended hull of `size` (length, beam):
// straight sides, both ends tapering to a rounded point.
fn sd_hull(p: vec2<f32>, size: vec2<f32>) -> f32 {
    let half = size / 2.0;
    let sides = abs(p.y) - half.y;
    return max(sides, sd_ellipse(p, vec2<f32>(half.x, half.y * TAPER)));
}

// Distance (px) from p to the segment a-b.
fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    return length(pa - ba * clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0));
}

// Signed distance (px) to the box centered at the origin with half
// extents `half`.
fn sd_box(p: vec2<f32>, half: vec2<f32>) -> f32 {
    let q = abs(p) - half;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
}

fn hull(in: VertexOut) -> vec4<f32> {
    let d = sd_hull(in.local, in.size);
    let inside = 1.0 - smoothstep(-AA_PX, AA_PX, d);
    let covered = 1.0 - smoothstep(in.style.x - AA_PX, in.style.x + AA_PX, d);
    // Dashes run around the outline, near enough by |x| + |y|.
    let dash = step(0.5, fract((abs(in.local.x) + abs(in.local.y)) / DASH_PX));
    let outline_alpha = in.outline.a * select(1.0, dash, in.style.y > 0.5);
    let rgb = mix(in.outline.rgb, in.fill.rgb, inside);
    return vec4<f32>(rgb, mix(outline_alpha, in.fill.a, inside) * covered);
}

// Distance (px) to an arc of the circle of radius r about the origin,
// `spread` radians either side of straight down.
fn sd_lower_arc(p: vec2<f32>, r: f32, spread: f32) -> f32 {
    let angle = atan2(p.x, -p.y);
    if abs(angle) <= spread {
        return abs(length(p) - r);
    }
    let end = vec2<f32>(sign(angle) * sin(spread), -cos(spread)) * r;
    return length(p - end);
}

// An anchor `size.y` px tall: ring, shank, stock and curved arms with
// flukes. Beneath it, a gauge filled from the left with the share of
// space left, frame and all in the fill color (warm when nearly full, so
// that shows even with the gauge almost empty).
// Signed distance (px) to the anchor's strokes, `s` px tall.
fn sd_anchor(p: vec2<f32>, s: f32) -> f32 {
    let ring = abs(length(p - vec2<f32>(0.0, 0.38 * s)) - 0.1 * s);
    let shank = sd_segment(p, vec2<f32>(0.0, 0.28 * s), vec2<f32>(0.0, -0.44 * s));
    let stock = sd_segment(p, vec2<f32>(-0.2 * s, 0.17 * s), vec2<f32>(0.2 * s, 0.17 * s));
    let spread = 1.15;
    let r = 0.36 * s;
    let center = vec2<f32>(0.0, -0.08 * s);
    let arms = sd_lower_arc(p - center, r, spread);
    // Flukes: short barbs turned up and out from each arm's tip.
    let tip = vec2<f32>(sin(spread), -cos(spread)) * r + center;
    let q = vec2<f32>(abs(p.x), p.y);
    let fluke = sd_segment(q, tip, tip + vec2<f32>(0.1 * s, 0.07 * s));
    return min(min(min(ring, shank), min(stock, arms)), fluke) - anchor_stroke(s) / 2.0;
}

fn anchor(in: VertexOut) -> vec4<f32> {
    let p = in.local;
    let s = in.size.y;
    let d = sd_anchor(p, s);
    var color = vec4<f32>(in.outline.rgb, in.outline.a * (1.0 - smoothstep(-AA_PX, AA_PX, d)));
    let left = in.style.w;
    if left >= 0.0 {
        let box_center = vec2<f32>(0.0, -0.5 * s - gauge_gap_px(s) - gauge_px(s) / 2.0);
        let box_half = vec2<f32>(0.45 * s, gauge_px(s) / 2.0);
        let g = sd_box(p - box_center, box_half);
        // Filled from the left for the space left; the rest empty, framed.
        let along = (p.x + box_half.x) / (2.0 * box_half.x);
        let filled = step(along, left) * (1.0 - smoothstep(-AA_PX, AA_PX, g));
        let frame = 1.0 - smoothstep(0.6 - AA_PX, 0.6 + AA_PX, abs(g));
        let gauge = vec4<f32>(in.fill.rgb, in.fill.a * max(frame, filled));
        if gauge.a > color.a {
            color = gauge;
        }
    }
    return color;
}

// The halo: strongest at the symbol's edge, fading out over `glow_px`.
fn halo(in: VertexOut) -> f32 {
    if in.glow.a <= 0.0 {
        return 0.0;
    }
    let is_anchor = in.style.z == KIND_ANCHOR;
    let d = select(sd_hull(in.local, in.size) - in.style.x, sd_anchor(in.local, in.size.y), is_anchor);
    let fade = 1.0 - smoothstep(0.0, glow_px(in.size), max(d, 0.0));
    return in.glow.a * fade * fade;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let shape = select(hull(in), anchor(in), in.style.z == KIND_ANCHOR);
    // The shape over its halo.
    let h = halo(in);
    let a = shape.a + h * (1.0 - shape.a);
    let rgb = select(in.glow.rgb, (shape.rgb * shape.a + in.glow.rgb * h * (1.0 - shape.a)) / a, a > 0.0);
    let color = vec4<f32>(rgb, a);
    if color.a <= 0.0 {
        discard;
    }
    return display(color);
}
