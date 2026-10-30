//! What the ferry-demo app and server share: the documents the server's
//! API serves (`api`), geodesy on the sphere (`geo`), and the ferry route
//! network (`routes`, `terminal`). Pure Rust, building for both the
//! browser (wasm) and the server.

pub mod api;
pub mod geo;
pub mod routes;
pub mod terminal;
pub mod text;
