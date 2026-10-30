struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Meters along the path, and the dash period in CSS px (0: solid).
    @location(1) along: f32,
    @location(2) @interpolate(flat) dash: f32,
};

// Dashes are drawn for this share of each period, gaps for the rest.
const DASH_ON: f32 = 0.6;

// Each segment instance is a quad: (t along the segment, side of the line).
const CORNERS = array<vec2<f32>, 6>(
    vec2<f32>(0.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    vec2<f32>(0.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
);

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
    @location(2) color: vec4<f32>,
    // Times the standard line width (globals.params.w).
    @location(3) width: f32,
    // Meters along the path at a and b, and the dash period (CSS px).
    @location(4) along: vec2<f32>,
    @location(5) dash: f32,
) -> VertexOut {
    let corner = CORNERS[vi];
    let viewport = globals.params.xy;
    let ca = world_to_clip(a);
    let cb = world_to_clip(b);

    // Offset perpendicular to the segment in screen space, so lines keep a
    // constant pixel width (WebGPU itself only rasterizes 1 px lines).
    let screen = (cb.xy / cb.w - ca.xy / ca.w) * viewport;
    let len = length(screen);
    let dir = select(vec2<f32>(1.0, 0.0), screen / len, len > 1e-6);
    let normal = vec2<f32>(-dir.y, dir.x);

    var clip = mix(ca, cb, corner.x);
    clip = vec4<f32>(clip.xy + normal * corner.y * globals.params.w * width / viewport * clip.w, clip.zw);

    var out: VertexOut;
    out.clip = clip;
    out.color = color;
    out.along = mix(along.x, along.y, corner.x);
    out.dash = dash;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    if in.dash > 0.0 && fract(in.along / (in.dash * globals.terrain.y)) > DASH_ON {
        discard;
    }
    return display(in.color);
}
