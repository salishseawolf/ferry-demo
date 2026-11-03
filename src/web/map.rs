//! The map, drawn by the GPU where the browser has one it can use, and
//! otherwise by Canvas2D over the baked basemap (crate::flat). Both draw
//! the same scene through the same camera.

use crate::flat::FlatRenderer;
use crate::renderer::{FrameCounts, Renderer};

pub(super) enum Map {
    Gpu(Box<Renderer>),
    Flat(Box<FlatRenderer>),
}

impl Map {
    pub(super) fn sync_size(&mut self) -> (u32, u32) {
        match self {
            Self::Gpu(r) => r.sync_size(),
            Self::Flat(r) => r.sync_size(),
        }
    }

    pub(super) fn needs_frame(&self) -> bool {
        match self {
            Self::Gpu(r) => r.needs_frame(),
            Self::Flat(r) => r.needs_frame(),
        }
    }

    pub(super) fn set_overlay(&mut self, layer: u32, segments: &[crate::scene::Segment]) {
        match self {
            Self::Gpu(r) => r.set_overlay(layer, segments),
            Self::Flat(r) => r.set_overlay(layer, segments),
        }
    }

    pub(super) fn counts(&self) -> FrameCounts {
        match self {
            Self::Gpu(r) => r.counts,
            Self::Flat(r) => r.counts,
        }
    }

    /// Memory the map holds: the GPU's estimate, or the decoded basemap
    /// images.
    pub(super) fn bytes(&self) -> u64 {
        match self {
            Self::Gpu(r) => r.gpu_bytes(),
            Self::Flat(r) => r.image_bytes(),
        }
    }
}
