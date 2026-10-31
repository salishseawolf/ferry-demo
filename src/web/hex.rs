//! The H3 hex grid layer.

use crate::grid::{KITSAP_SLICE, h3_cell};
use crate::hexgrid::{cells_around, edge_m, outlines, resolution_for};
use crate::scene::{GRID_COLOR, Scene};
use crate::view::LAYER_CELLS;
use glam::DVec3;
use h3o::{CellIndex, Resolution};

use super::App;

/// The hex grid covers this many times the view, so it can zoom out this
/// far before rebuilding (see `HexGrid`).
const HEX_COVER: f64 = 1.25;

impl App {
    /// Rebuilds the hex grid's lines if the view calls for it (see
    /// `HexGrid`), and keeps the resolution current for the HUD.
    pub(super) fn update_hex_grid(&mut self) {
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
}

/// The hex grid on screen. It covers a little more than the view, with
/// cells only where they can be seen, and is rebuilt only when the view
/// outgrows that: another resolution or window size, the
/// view's center moving into another cell a level up, or zooming out past
/// the margin. Zooming in rebuilds it only if the last build had to settle
/// for a coarser resolution than the zoom asked for.
pub(super) struct HexGrid {
    /// What it was built for: the resolution asked for, the cell a level
    /// coarser under the view's center, and the canvas size.
    pub(super) built_for: Option<(Resolution, CellIndex, (i32, i32))>,
    /// Ground meters per CSS pixel when built.
    pub(super) built_m_per_px: f64,
    /// The resolution drawn (coarser than asked if the view would need too
    /// many cells), and how many cells.
    pub(super) resolution: Resolution,
    pub(super) cells: usize,
}
