// Cell meshes (the grid and the backdrop under it), on the globe and,
// forward-projected, on flat maps.

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) unit: vec3<f32>,
    @location(1) @interpolate(flat) cell: u32,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) cell: u32) -> VertexOut {
    var out: VertexOut;
    out.clip = world_to_clip(position * globals.params.z);
    out.unit = position;
    out.cell = cell;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let p = normalize(in.unit);
    // Lighting is in linear light: 2% ambient keeps the night side dark
    // but legible (about 15% brightness once encoded to sRGB).
    let light = 0.02 + 0.98 * lambert(p);
    return display(vec4<f32>(surface_albedo(in.cell) * light, 1.0));
}
