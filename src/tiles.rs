//! Streams terrain tiles from the server into the GPU cache: reads the
//! pipeline's index.json, fetches the tiles each view needs (coarse ones
//! first, so there is always something to fall back on), and hands each
//! frame its terrain regions.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::Response;

use crate::grid::KITSAP_SLICE;
use crate::renderer::{Renderer, STAGING_LAYERS, TERRAIN_SLOTS, TerrainInstance, TileUpload};
use crate::terrain::{Cache, Manifest, TileKey, View, decode_tile, select};

/// Tile requests in flight at once.
const MAX_IN_FLIGHT: usize = 8;

/// What arrives from fetches, shared with their futures.
#[derive(Default)]
struct Arrivals {
    /// Decoded tiles (see `terrain::decode_tile`).
    ready: Vec<(TileKey, Vec<u8>)>,
    failed: HashSet<TileKey>,
}

pub struct TerrainLoader {
    manifest: Manifest,
    cache: Cache,
    frame: u64,
    in_flight: HashSet<TileKey>,
    arrivals: Rc<RefCell<Arrivals>>,
    /// Regions drawn in the last frame, and how many of those from a
    /// coarser tile standing in for their own.
    pub drawn: usize,
    pub stand_ins: usize,
}

/// The tile index: which tiles exist, and the sources to credit.
pub struct Index {
    pub manifest: Manifest,
    pub attribution: Vec<String>,
}

impl TerrainLoader {
    pub fn new(manifest: Manifest) -> Self {
        Self {
            manifest,
            cache: Cache::new(TERRAIN_SLOTS),
            frame: 0,
            in_flight: HashSet::new(),
            arrivals: Rc::default(),
            drawn: 0,
            stand_ins: 0,
        }
    }

    /// Whether tiles are on their way or waiting to upload: the map needs
    /// redrawing until they're in.
    pub fn busy(&self) -> bool {
        !self.in_flight.is_empty() || !self.arrivals.borrow().ready.is_empty()
    }

    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    pub fn loading(&self) -> usize {
        self.in_flight.len()
    }

    pub fn available(&self) -> usize {
        self.manifest.len()
    }

    /// One frame's terrain: uploads tiles that have arrived, asks for the
    /// ones `view` needs next, and returns the regions to draw.
    pub fn update(&mut self, renderer: &mut Renderer, view: &impl View) -> Vec<TerrainInstance> {
        self.frame += 1;
        let frame = self.frame;
        let regions = select(&self.manifest, view);
        // Mark what this frame draws from, so uploads don't evict it.
        for &r in &regions {
            self.cache.source(r, frame);
        }
        self.upload(renderer);

        let instances: Vec<TerrainInstance> = regions
            .iter()
            .filter_map(|&r| {
                let s = self.cache.source(r, frame)?;
                Some(TerrainInstance {
                    key: [u32::from(r.z), r.x, r.y],
                    slot: s.slot,
                    source: [s.offset.x as f32, s.offset.y as f32, s.scale as f32, 0.0],
                })
            })
            .collect();
        self.drawn = instances.len();
        self.stand_ins = instances.iter().filter(|i| i.source[2] < 1.0).count();
        self.request(&regions);
        instances
    }

    fn upload(&mut self, renderer: &mut Renderer) {
        let mut arrivals = self.arrivals.borrow_mut();
        let mut uploads = Vec::new();
        while uploads.len() < STAGING_LAYERS {
            let Some((key, rgba)) = arrivals.ready.pop() else {
                break;
            };
            self.in_flight.remove(&key);
            // Every slot drawn from this frame: drop it; it's asked for
            // again once there's room.
            if let Some(slot) = self.cache.insert(key, self.frame) {
                uploads.push(TileUpload { rgba, key, slot });
            }
        }
        for key in &arrivals.failed {
            self.in_flight.remove(key);
        }
        drop(arrivals);
        renderer.upload_tiles(&uploads);
    }

    /// Fetches what `regions` need and the cache lacks: each region's tile
    /// and its ancestors, coarsest first.
    fn request(&mut self, regions: &[TileKey]) {
        let mut wanted = BTreeSet::new();
        for &r in regions {
            let mut key = Some(r);
            while let Some(k) = key {
                if !wanted.insert(k) {
                    break;
                }
                key = k.parent();
            }
        }
        let next: Vec<TileKey> = {
            let failed = &self.arrivals.borrow().failed;
            wanted
                .into_iter()
                .filter(|&k| {
                    self.manifest.contains(k)
                        && !self.cache.contains(k)
                        && !self.in_flight.contains(&k)
                        && !failed.contains(&k)
                })
                .take(MAX_IN_FLIGHT.saturating_sub(self.in_flight.len()))
                .collect()
        };
        for key in next {
            self.in_flight.insert(key);
            let arrivals = self.arrivals.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match fetch_tile(&key.url("elevation", "png")).await {
                    Ok(rgba) => arrivals.borrow_mut().ready.push((key, rgba)),
                    Err(e) => {
                        log::warn!("tile {key:?}: {e:?}");
                        arrivals.borrow_mut().failed.insert(key);
                    }
                }
            });
        }
    }
}

/// Fetches and parses the index.json of `layer` (a folder of tiles/).
pub async fn load_index(layer: &str) -> Result<Index, String> {
    let url = format!("tiles/{layer}/index.json");
    let json = fetch(&url)
        .await
        .and_then(|r| r.json())
        .map(JsFuture::from)
        .map_err(|e| format!("{url}: {e:?}"))?
        .await
        .map_err(|e| format!("{url}: {e:?}"))?;
    let get = |v: &JsValue, key: &str| js_sys::Reflect::get(v, &key.into()).unwrap_or_default();
    let levels = get(&json, "levels");
    let mut tiles = Vec::new();
    let levels_object: &js_sys::Object = levels.unchecked_ref();
    for z in js_sys::Object::keys(levels_object).iter() {
        let zoom: u8 = z
            .as_string()
            .and_then(|s| s.parse().ok())
            .ok_or("bad zoom")?;
        let list: js_sys::Array = js_sys::Reflect::get(&levels, &z)
            .map_err(|_| "bad level")?
            .unchecked_into();
        for xy in list.iter() {
            let xy: js_sys::Array = xy.unchecked_into();
            let n = |i| xy.get(i).as_f64().unwrap_or(0.0) as u32;
            tiles.push(TileKey {
                z: zoom,
                x: n(0),
                y: n(1),
            });
        }
    }
    let attribution = js_sys::Array::from(&get(&json, "attribution"))
        .iter()
        .filter_map(|s| s.as_string())
        .collect();
    Ok(Index {
        manifest: Manifest::new(tiles, KITSAP_SLICE),
        attribution,
    })
}

pub async fn fetch(url: &str) -> Result<Response, JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let response: Response = JsFuture::from(window.fetch_with_str(url))
        .await?
        .dyn_into()?;
    if !response.ok() {
        return Err(format!("HTTP {}", response.status()).into());
    }
    Ok(response)
}

/// A tile fetched and decoded to RGBA bytes. Decoding here keeps the
/// browser's image pipeline (color management, premultiplying, and
/// copyExternalImageToTexture, which crashed Firefox's WebGPU) out of it.
async fn fetch_tile(url: &str) -> Result<Vec<u8>, JsValue> {
    let buffer = JsFuture::from(fetch(url).await?.array_buffer()?).await?;
    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
    Ok(decode_tile(&bytes)?)
}
