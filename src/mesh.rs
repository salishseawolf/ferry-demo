use crate::geo::Cap;
use bytemuck::{Pod, Zeroable};
use glam::DVec3;

/// Cell id of backdrop vertices (NO_CELL in surface.wgsl).
pub const NO_CELL: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

/// Globe vertex: a point on the unit sphere and the grid cell it belongs to.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct CellVertex {
    pub position: [f32; 3],
    pub cell: u32,
}

/// Rings and spokes of [`cap_mesh`]: rings every quarter degree or so
/// keep each facet's sag (about 15 m) well under the cap's sink.
const CAP_RING_DEG: f64 = 0.25;
const CAP_SEGMENTS: u32 = 512;

/// `cap` as a disc of [`NO_CELL`] triangles (concentric rings around the
/// center), sunk to `scale` of the radius so the grid's cells always draw
/// over it.
pub fn cap_mesh(cap: &Cap, scale: f64) -> (Vec<CellVertex>, Vec<u32>) {
    let angle = cap.angle();
    let rings = (angle.to_degrees() / CAP_RING_DEG).ceil() as u32;
    let c = cap.center.to_unit();
    let (east, north) = cap.center.east_north();
    let vertex = |p: DVec3| CellVertex {
        position: (p * scale).as_vec3().to_array(),
        cell: NO_CELL,
    };
    // The center, then each ring outward, counterclockwise from east.
    let mut vertices = vec![vertex(c)];
    for r in 1..=rings {
        let a = angle * r as f64 / rings as f64;
        for k in 0..CAP_SEGMENTS {
            let t = std::f64::consts::TAU * k as f64 / CAP_SEGMENTS as f64;
            vertices.push(vertex(
                c * a.cos() + (east * t.cos() + north * t.sin()) * a.sin(),
            ));
        }
    }
    let at = |r: u32, k: u32| 1 + (r - 1) * CAP_SEGMENTS + k % CAP_SEGMENTS;
    let mut indices = Vec::new();
    for k in 0..CAP_SEGMENTS {
        indices.extend([0, at(1, k), at(1, k + 1)]);
        for r in 1..rings {
            let (inner, inner_next) = (at(r, k), at(r, k + 1));
            let (outer, outer_next) = (at(r + 1, k), at(r + 1, k + 1));
            indices.extend([inner, outer, outer_next, inner, outer_next, inner_next]);
        }
    }
    (vertices, indices)
}

/// A unit square in `segments` x `segments` quads, as (u, v) points from
/// (0, 0) to (1, 1): the patch every terrain region is drawn with.
pub fn patch(segments: u32) -> (Vec<[f32; 2]>, Vec<u32>) {
    let n = segments + 1;
    let vertices = (0..n * n)
        .map(|i| {
            [
                (i % n) as f32 / segments as f32,
                (i / n) as f32 / segments as f32,
            ]
        })
        .collect();
    let indices = (0..segments * segments)
        .flat_map(|q| {
            let (row, col) = (q / segments, q % segments);
            let a = row * n + col;
            [a, a + n, a + 1, a + 1, a + n, a + n + 1]
        })
        .collect();
    (vertices, indices)
}

/// Unit cylinder around +Y with radius 1, from slightly below 0 (so it sinks
/// into the surface instead of floating over globe facets) up to 1, with a
/// top cap. The bottom is never visible, so it stays open.
pub fn cylinder(sides: u32) -> Mesh {
    const BOTTOM: f32 = -0.05;
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for i in 0..sides {
        let angle = |k: u32| k as f32 / sides as f32 * std::f32::consts::TAU;
        let (s0, c0) = angle(i).sin_cos();
        let (s1, c1) = angle(i + 1).sin_cos();
        let base = vertices.len() as u32;
        for (x, z) in [(c0, s0), (c1, s1)] {
            vertices.push(Vertex {
                position: [x, BOTTOM, z],
                normal: [x, 0.0, z],
            });
            vertices.push(Vertex {
                position: [x, 1.0, z],
                normal: [x, 0.0, z],
            });
        }
        indices.extend([base, base + 1, base + 3, base, base + 3, base + 2]);
        let cap = vertices.len() as u32;
        vertices.push(Vertex {
            position: [0.0, 1.0, 0.0],
            normal: [0.0, 1.0, 0.0],
        });
        vertices.push(Vertex {
            position: [c0, 1.0, s0],
            normal: [0.0, 1.0, 0.0],
        });
        vertices.push(Vertex {
            position: [c1, 1.0, s1],
            normal: [0.0, 1.0, 0.0],
        });
        indices.extend([cap, cap + 2, cap + 1]);
    }
    Mesh { vertices, indices }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::EARTH_RADIUS_M;
    use crate::grid::KITSAP_SLICE;
    use glam::Vec3;

    fn assert_faces_outward((vertices, indices): &(Vec<CellVertex>, Vec<u32>)) {
        let pos = |i: u32| Vec3::from_array(vertices[i as usize].position);
        for tri in indices.chunks(3) {
            let (a, b, c) = (pos(tri[0]), pos(tri[1]), pos(tri[2]));
            assert!((b - a).cross(c - a).dot(a) > 0.0, "front faces are CCW");
            let cells: Vec<u32> = tri.iter().map(|&i| vertices[i as usize].cell).collect();
            assert!(cells[0] == cells[1] && cells[1] == cells[2]);
        }
    }

    #[test]
    fn patch_covers_the_unit_square() {
        let (vertices, indices) = patch(4);
        assert_eq!(vertices.len(), 25);
        assert_eq!(indices.len(), 4 * 4 * 6);
        assert_eq!(vertices[24], [1.0, 1.0]);
        // Total area of the triangles is the square's.
        let area: f32 = indices
            .chunks(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|i| vertices[t[i] as usize]);
                ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs() / 2.0
            })
            .sum();
        assert!((area - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cap_faces_outward_and_reaches_its_radius() {
        let mesh = cap_mesh(&KITSAP_SLICE, 0.9999);
        assert_faces_outward(&mesh);
        assert!(mesh.0.iter().all(|v| v.cell == NO_CELL));
        let c = KITSAP_SLICE.center.to_unit();
        let reach = mesh
            .0
            .iter()
            .map(|v| Vec3::from_array(v.position).as_dvec3())
            .map(|p| p.angle_between(c) * EARTH_RADIUS_M)
            .fold(0.0, f64::max);
        assert!((reach - KITSAP_SLICE.radius_m).abs() < 1.0, "{reach}");
        // Every vertex is sunk below the grid's cells.
        assert!(
            mesh.0
                .iter()
                .all(|v| Vec3::from_array(v.position).length() < 1.0)
        );
    }
}
