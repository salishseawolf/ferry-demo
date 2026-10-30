//! What the viewer has chosen to see: the base map style and the overlay
//! layers. Shared by the renderer, the color key and the shaders
//! (common.wgsl repeats these values).

/// Bits of the overlay layer mask.
pub const LAYER_CELLS: u32 = 1;
pub const LAYER_FERRIES: u32 = 2;

/// How the base map is drawn: one function each in styles.wgsl.
pub const STYLE_CARTOGRAPHIC: u32 = 0;
pub const STYLE_HYPSOMETRIC: u32 = 1;
