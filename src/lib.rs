// Only the pure math and state modules build natively (for tests); the
// renderer and browser glue target wasm.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod camera;
mod finder;
mod fleet;
mod geo;
mod grid;
mod hexgrid;
mod legend;
#[cfg(target_arch = "wasm32")]
mod live;
mod measure;
mod mesh;
mod palette;
mod perf;
mod placement;
#[cfg(target_arch = "wasm32")]
mod renderer;
mod routes;
mod scene;
mod schedule;
mod search;
mod terminals;
mod terrain;
#[cfg(target_arch = "wasm32")]
mod tiles;
mod view;
#[cfg(target_arch = "wasm32")]
mod web;
