# ferry-demo

[![CI](https://github.com/salishseawolf/ferry-demo/actions/workflows/ci.yml/badge.svg)](https://github.com/salishseawolf/ferry-demo/actions/workflows/ci.yml)
[![Audit](https://github.com/salishseawolf/ferry-demo/actions/workflows/audit.yml/badge.svg)](https://github.com/salishseawolf/ferry-demo/actions/workflows/audit.yml)

A live map of Washington State Ferries on a 3D globe of Puget Sound, written in Rust and rendered with WebGPU in the browser.

- **Live boats:** every ferry's position, heading and wake every 10 seconds, drawn to scale on terrain and bathymetry.
- **Schedules:** each terminal's and route's sailings for the day, with the next one held in view.
- **When to arrive:** a recommended arrival time by car for each sailing, estimated from how comparable sailings filled before, adjusted live when today is busier than usual, and falling back to WSF's own guidance where there's too little history.
- **Car deck use:** how full each sailing left, and by hour of the day on each route.
- **Notices, crossing times and a finder** for boats, terminals and routes.

## How it's built

| Part | What it is |
|---|---|
| `src/` | The app: Rust compiled to WebAssembly, rendering with wgpu (WebGPU). Terrain comes from pre-built tiles; the panels are plain HTML over the canvas. |
| `crates/server` | The server (axum, sqlx, Postgres): polls WSF's API, keeps every answer, derives trips, car space and loads from them, works out arrival advice, and serves the app's API under `/api/v1/`. |
| `crates/model` | What the two share: the API's documents, geodesy, the route network. |
| `tools/` | The pipelines behind the terrain tiles and the ferry routes (from OpenStreetMap). |

The server records WSF's raw answers in Postgres and derives everything else from them, so history can be rebuilt as the derivations improve (`ferry-server rebuild`). Arrival advice comes from a daily job (`ferry-server compute-advice`) that stores each terminal's history, from which the server publishes fresh advice every minute.

## Running it

The tools are pinned with [mise](https://mise.jdx.dev); the services run with Podman.

```
mise run dev
```

That starts the app on http://localhost:8080, with the server and Postgres beside it. Live data needs a WSDOT Traveler API access code in `WSDOT_API_KEY`. The terrain tiles are built separately (`mise run tiles:elevation`, a large download) or come from the published tiles image.

Other tasks:

| Task | |
|---|---|
| `mise run setup` | Once per clone: installs a git hook that checks formatting before each commit |
| `mise run check` | Formatting and clippy, for the browser build and natively |
| `mise run test` | The tests; the database ones need `DATABASE_URL` |
| `mise run build` | The release build of the app |
| `mise run container:build` | The release images |
| `mise run dev:advice` | The daily advice job, by hand |

## Images

Every push to `main` that passes CI publishes the release images, with build provenance:

- `ghcr.io/salishseawolf/ferry-demo`: the app (nginx), terrain tiles included
- `ghcr.io/salishseawolf/ferry-demo-server`: the server
- `ghcr.io/salishseawolf/ferry-demo-tiles`: the terrain tiles, published from the machine with the source data (`mise run tiles:publish`)

## Data

- Ferry positions, schedules, car space and notices: Washington State Department of Transportation, Washington State Ferries
- Ferry routes: © OpenStreetMap contributors (ODbL)
- Elevation: ETOPO 2022 (NOAA NCEI); Copernicus DEM GLO-30 and GLO-90 © DLR e.V. 2010-2014 and © Airbus Defence and Space GmbH 2014-2018, provided under COPERNICUS by the European Union and ESA; NOAA NCEI CUDEM 1/9 arc-second topobathy
- Land cover: ESA WorldCover 10 m 2021 v200 © ESA WorldCover project, CC BY 4.0
- Typefaces: Source Serif 4, Overpass and Overpass Mono, under the SIL Open Font License (`assets/fonts/`)

This project isn't affiliated with WSDOT or Washington State Ferries.

## License

Copyright (c) 2026 Thomas G Wolf. All rights reserved: see [LICENSE](LICENSE). The typefaces and the data sources above keep their own licenses.
