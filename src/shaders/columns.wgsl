struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    // Unit cylinder: radius 1 around +Y, y from just below 0 up to 1.
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) base: vec3<f32>,
    @location(3) height: f32,
    @location(4) up: vec3<f32>,
    @location(5) width: f32,
    @location(6) color: vec4<f32>,
) -> VertexOut {
    // Any orthonormal basis around the surface normal works for a cylinder.
    let helper = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), abs(up.x) > 0.9);
    let tx = normalize(cross(helper, up));
    let tz = cross(up, tx);

    let world = base + tx * (position.x * width) + up * (position.y * height) + tz * (position.z * width);
    var out: VertexOut;
    // On flat maps the column flattens to a disc at its base.
    out.clip = world_to_clip(world);
    out.normal = tx * normal.x + up * normal.y + tz * normal.z;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let shade = 0.1 + 0.9 * lambert(in.normal);
    return display(vec4<f32>(in.color.rgb * shade, in.color.a));
}
