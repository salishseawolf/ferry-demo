"""Builds the elevation tile pyramid the renderer streams.

Heights (meters, land and seabed) come from three sources, each overriding
the one before where it has data:

1. ETOPO 2022 15 arc-second surface (about 460 m): the global base.
2. Copernicus DEM (GLO-90 for the whole slice, GLO-30 over Puget Sound): land.
   It flattens the sea to 0, so its sea pixels keep ETOPO's depth.
3. NOAA CUDEM 1/9 arc-second topobathy (about 3 m): Washington's coast and
   inland waters, land and seabed together.

Tiles are Web Mercator XYZ, 256 px plus a 2 px border on every side (260 px
PNGs): the renderer's slope textures overlap their neighbors by one texel,
so sampling blends across tile edges as it does inside a tile, and slopes
at that overlap need one more pixel beyond it.

Each pixel's channels:

* R, G: height, (R * 256 + G) / 8 - 3500 meters (1/8 m steps, -3500 to
  +4692 m).
* B: roughly how wide the water here is: 16 * log2(width in m), 0 on land
  away from water (see `water_widths`). Shorelines carry their water's
  width too. The renderer fades narrow water (ponds, creeks) out as you
  zoom out.
* A: the water fraction, from ESA WorldCover's permanent water class
  (lakes, rivers wider than a pixel or two, the sea), falling back to
  "below sea level" where WorldCover has no data. Stored as
  1 + 254 * fraction, never 0, so no pixel is fully transparent.

Zoom 0-12 covers the slice (200 km around Kitsap County), zoom 13 only the
Puget Sound box. Keep the slice in step with KITSAP_SLICE in src/grid.rs
(the box is the pipeline's own). Each tier's sources cover all of its tiles, which reach past
the area they're chosen for (a zoom-10 tile is about 26 km wide).

Run through `mise run tiles:elevation`. Local copies of the sources (about
1 GB, fetched once) go to /cache, tiles to /out/elevation.
"""

import json
import math
import os
import sys
import time
import urllib.request
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
from osgeo import gdal

gdal.UseExceptions()
gdal.SetConfigOption("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR")
gdal.SetConfigOption("GDAL_HTTP_MULTIRANGE", "YES")
gdal.SetConfigOption("GDAL_HTTP_MERGE_CONSECUTIVE_RANGES", "YES")
gdal.SetConfigOption("VSI_CACHE", "TRUE")

EARTH_RADIUS_M = 6_371_008.8
SLICE_CENTER = (-122.65, 47.64)  # Kitsap County
SLICE_RADIUS_M = 200_000
PUGET_SOUND = (-123.5, 47.2, -122.2, 48.8)  # west, south, east, north
# Zoom tiers: the slice from coarse sources, the slice from 30 m land
# data, and the Puget Sound box from everything at its finest.
SLICE_ZOOMS = range(0, 10)
REGION_ZOOMS = range(10, 13)
DETAIL_ZOOMS = range(13, 14)

TILE = 256
BORDER = 2
# Heights are stored in 1/HEIGHT_STEPS m steps from -HEIGHT_OFFSET_M, in
# 16 bits: finer than any slope worth shading.
HEIGHT_STEPS = 8
HEIGHT_OFFSET_M = 3500
# Water widths: the coarsest block level (2^levels pixels), the code's
# steps per doubling of width, and pixels a width reaches onto land.
WIDTH_LEVELS = 10
WIDTH_CODE_SCALE = 16
SIZE_GROW_PX = 2
WORLD_M = 20_037_508.342789244  # Web Mercator half-width

ETOPO_URL = (
    "https://www.ngdc.noaa.gov/mgg/global/relief/ETOPO2022/data/15s/"
    "15s_surface_elev_gtif/ETOPO_2022_v1_15s_{tile}_surface.tif"
)
GLO_URL = "/vsicurl/https://copernicus-dem-{res}m.s3.amazonaws.com/{name}/{name}.tif"
GLO_LIST = "https://copernicus-dem-{res}m.s3.amazonaws.com/tileList.txt"
CUDEM_BUCKET = "https://noaa-nos-coastal-lidar-pds.s3.amazonaws.com"
CUDEM_PREFIX = "dem/NCEI_ninth_Topobathy_2014_8483/"
CUDEM_REGIONS = [
    "wash_pugetsound",
    "wash_juandefuca",
    "wash_bellingham",
    "wash_outercoast",
    "columbia_river",
]

# Source pixel size (m) as warped from, to pick averaging or interpolation
# per zoom. CUDEM's finest copy is overview 1, about 14 m.
SOURCE_PX_M = {
    "etopo": 463.0,
    "glo90": 180.0,
    "glo30": 30.0,
    "cudem_coast": 55.0,
    "cudem_detail": 14.0,
}
# Mosaic pixel sizes (degrees), matching the copies they're built from.
GLO90_DEG = 1 / 600
CUDEM_COAST_DEG = 0.0005
CUDEM_DETAIL_DEG = 0.25 / 2028

WORLDCOVER_URL = (
    "https://esa-worldcover.s3.eu-central-1.amazonaws.com/v200/2021/map/"
    "ESA_WorldCover_10m_2021_v200_{tile}_Map.tif"
)
# Water mosaics: 20 m over the slice, WorldCover's own 10 m over Puget
# Sound, where zoom 13 needs it.
WATER_REGION_DEG = 1 / 6000
WATER_DETAIL_DEG = 1 / 12000
# WorldCover's permanent water bodies class; 0 is no data.
WORLDCOVER_WATER = 80

ATTRIBUTION = [
    "ETOPO 2022 (NOAA NCEI), doi:10.25921/fd45-gt74",
    "Copernicus DEM GLO-30 and GLO-90 © DLR e.V. 2010-2014 and © Airbus "
    "Defence and Space GmbH 2014-2018, provided under COPERNICUS by the "
    "European Union and ESA",
    "NOAA NCEI CUDEM 1/9 arc-second topobathy, doi:10.25921/ds9v-ky35",
    "ESA WorldCover 10 m 2021 v200 © ESA WorldCover project, CC BY 4.0",
]


def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


# Geometry ---------------------------------------------------------------


def unit(lon, lat):
    lon, lat = math.radians(lon), math.radians(lat)
    return (math.cos(lat) * math.cos(lon), math.sin(lat), -math.cos(lat) * math.sin(lon))


def in_slice(lon, lat):
    a, b = unit(lon, lat), unit(*SLICE_CENTER)
    cos = sum(x * y for x, y in zip(a, b))
    return cos >= math.cos(SLICE_RADIUS_M / EARTH_RADIUS_M)


def slice_bounds():
    """Lon/lat box around the slice (it doesn't reach a pole)."""
    angle = math.degrees(SLICE_RADIUS_M / EARTH_RADIUS_M)
    lon, lat = SLICE_CENTER
    half_lon = math.degrees(
        math.asin(math.sin(math.radians(angle)) / math.cos(math.radians(lat)))
    )
    return (lon - half_lon, lat - angle, lon + half_lon, lat + angle)


def tile_x(lon, z):
    return int((lon + 180) / 360 * 2**z)


def tile_y(lat, z):
    lat = math.radians(lat)
    return int((1 - math.asinh(math.tan(lat)) / math.pi) / 2 * 2**z)


def tile_lon(x, z):
    return x / 2**z * 360 - 180


def tile_lat(y, z):
    return math.degrees(math.atan(math.sinh(math.pi * (1 - 2 * y / 2**z))))


def tile_range(bounds, z):
    west, south, east, north = bounds
    return tile_x(west, z), tile_y(north, z), tile_x(east, z), tile_y(south, z)


def tile_touches_slice(x, y, z):
    """Whether the tile reaches the slice: its point nearest the slice
    center (the center clamped into its lon/lat box; exact enough for
    tiles that don't reach a pole) lies in the slice."""
    w, e = tile_lon(x, z), tile_lon(x + 1, z)
    n, s = tile_lat(y, z), tile_lat(y + 1, z)
    lon, lat = SLICE_CENTER
    return in_slice(min(max(lon, w), e), min(max(lat, s), n))


def ground_px_m(z):
    """Ground size of one pixel at zoom z over the slice center."""
    return 2 * WORLD_M / 2**z / TILE * math.cos(math.radians(SLICE_CENTER[1]))


# Sources ------------------------------------------------------------------


def fetch(url, path):
    if path.exists():
        return
    log(f"download {url}")
    tmp = path.with_suffix(".part")
    urllib.request.urlretrieve(url, tmp)
    tmp.rename(path)


def localize(job):
    """Copies a remote COG (`src`) to `dst` at one of its overviews (`ovr`,
    None for full resolution), as a local COG with its own overviews.
    Reading the right overview once keeps every later warp local and
    cheap: warping straight from remote files re-reads them per zoom."""
    src, dst, ovr = job
    if dst.exists():
        return dst
    tmp = dst.with_name(dst.name + ".part")
    args = [] if ovr is None else ["-ovr", str(ovr)]
    gdal.Translate(
        str(tmp),
        src,
        options=gdal.TranslateOptions(
            options=args,
            format="COG",
            creationOptions=["COMPRESS=DEFLATE", "PREDICTOR=FLOATING_POINT", "BIGTIFF=NO"],
        ),
    )
    tmp.rename(dst)
    return dst


def etopo_files(cache, bounds):
    west, south, east, north = bounds
    files = []
    for top in range(int(math.ceil(north / 15) * 15), int(south), -15):
        for left in range(int(math.floor(west / 15) * 15), int(east), 15):
            ns = f"N{top:02d}" if top >= 0 else f"S{-top:02d}"
            ew = f"W{-left:03d}" if left < 0 else f"E{left:03d}"
            name = f"{ns}{ew}"
            path = cache / "etopo" / f"{name}.tif"
            path.parent.mkdir(parents=True, exist_ok=True)
            fetch(ETOPO_URL.format(tile=name), path)
            files.append(str(path))
    return files


def glo_jobs(cache, res, ovr, bounds, keep):
    """Localize jobs for the Copernicus tiles (`res` 30 or 90) in `bounds`
    passing `keep(lon, lat)` at the tile's south-west corner."""
    west, south, east, north = bounds
    names = urllib.request.urlopen(GLO_LIST.format(res=res)).read().decode().split()
    jobs = []
    for name in names:
        parts = name.split("_")  # Copernicus_DSM_COG_30_N47_00_W124_00_DEM
        lat = int(parts[4][1:]) * (1 if parts[4][0] == "N" else -1)
        lon = int(parts[6][1:]) * (1 if parts[6][0] == "E" else -1)
        if west - 1 <= lon <= east and south - 1 <= lat <= north and keep(lon, lat):
            dst = cache / f"glo{res}" / f"{name}.tif"
            jobs.append((GLO_URL.format(res=res, name=name), dst, ovr))
    log(f"glo{res}: {len(jobs)} tiles")
    return jobs


def cudem_jobs(cache):
    """Localize jobs for CUDEM: tiles reaching the detail area at about
    14 m (overview 1), the rest at about 55 m (overview 3), enough for the
    slice's zoom 9. Each keeps its own nodata value (some use -9999, some
    -99999)."""
    west, south, east, north = PUGET_SOUND
    jobs = []
    for region in CUDEM_REGIONS:
        query = f"{CUDEM_BUCKET}/?list-type=2&prefix={CUDEM_PREFIX}{region}/"
        xml = urllib.request.urlopen(query).read().decode()
        for part in xml.split("<Key>")[1:]:
            key = part.split("</Key>")[0]
            if not key.endswith(".tif"):
                continue
            # ncei19_n47x25_w122x50_2024v1.tif: north-west corner, 0.25° tiles.
            name = key.rsplit("/", 1)[1]
            n, w = name.split("_")[1:3]
            top = float(n[1:].replace("x", "."))
            left = -float(w[1:].replace("x", "."))
            detail = left < east and left + 0.25 > west and top > south and top - 0.25 < north
            dst = cache / "cudem" / name
            jobs.append((f"/vsicurl/{CUDEM_BUCKET}/{key}", dst, 1 if detail else 3))
    log(f"cudem: {len(jobs)} tiles, {sum(j[2] == 1 for j in jobs)} in detail")
    return jobs


def build_vrt(out, files, vrt_nodata):
    """A VRT of same-resolution, grid-aligned files (see `mosaic` for why
    anything else goes through a mosaic instead)."""
    if not out.exists():
        gdal.BuildVRT(str(out), files, VRTNodata=vrt_nodata).FlushCache()
    return str(out)


def mosaic(name, cache, files, bounds, px_deg):
    """One seamless raster (a COG with overviews) of `files` over `bounds`.

    Zoom levels are warped from these rather than from the source tiles:
    warping many files works file by file, and interpolation at each
    file's edge, with no neighbor to see, leaves a visible seam. A single
    file also lets each zoom read just the overview it needs.

    The mosaic itself is averaged straight from the files, never through a
    VRT: a VRT resampling files of mixed resolution averages each edge
    pixel as if its uncovered part were 0, leaving a trench along every
    tile edge. Each file keeps its own nodata value (CUDEM's vary)."""
    out = cache / f"{name}.tif"
    if out.exists():
        return str(out)
    log(f"mosaic {name}: {len(files)} files")
    west, south, east, north = bounds
    tmp = out.with_name(out.name + ".part")
    gdal.Warp(
        str(tmp),
        files,
        format="COG",
        dstSRS="EPSG:4326",
        outputBounds=(west, south, east, north),
        xRes=px_deg,
        yRes=px_deg,
        resampleAlg="average",
        dstNodata=float("nan"),
        outputType=gdal.GDT_Float32,
        multithread=True,
        warpMemoryLimit=4096,
        warpOptions=["NUM_THREADS=ALL_CPUS"],
        creationOptions=[
            "COMPRESS=DEFLATE",
            "PREDICTOR=FLOATING_POINT",
            "BIGTIFF=IF_SAFER",
            "NUM_THREADS=ALL_CPUS",
        ],
    )
    tmp.rename(out)
    return str(out)


def worldcover_files(cache, bounds):
    """WorldCover's 3° tiles over `bounds`, named by south-west corner."""
    west, south, east, north = bounds
    files = []
    for lat in range(math.floor(south / 3) * 3, math.ceil(north / 3) * 3, 3):
        for lon in range(math.floor(west / 3) * 3, math.ceil(east / 3) * 3, 3):
            ns = f"N{lat:02d}" if lat >= 0 else f"S{-lat:02d}"
            ew = f"E{lon:03d}" if lon >= 0 else f"W{-lon:03d}"
            path = cache / "worldcover" / f"{ns}{ew}.tif"
            path.parent.mkdir(parents=True, exist_ok=True)
            fetch(WORLDCOVER_URL.format(tile=ns + ew), path)
            files.append(str(path))
    return files


def classify_water(job):
    """Water and has-data bands (0 or 255) from one WorldCover tile, read
    at `level` (0 for its 10 m, 1 for its 20 m overview) over the part of
    it inside lon/lat `bounds`, written to `dst`. Done in numpy strips:
    much faster than a lookup-table VRT, which GDAL runs on one core."""
    src, dst, level, bounds = job
    if dst.exists():
        return str(dst)
    ds = gdal.Open(src)
    band = ds.GetRasterBand(1)
    if level:
        band = band.GetOverview(level - 1)
    gt = ds.GetGeoTransform()
    scale = ds.RasterXSize / band.XSize
    px = gt[1] * scale
    west, south, east, north = bounds
    x0 = max(0, int((west - gt[0]) / px))
    x1 = min(band.XSize, int(math.ceil((east - gt[0]) / px)))
    y0 = max(0, int((gt[3] - north) / px))
    y1 = min(band.YSize, int(math.ceil((gt[3] - south) / px)))
    if x1 <= x0 or y1 <= y0:
        return None
    tmp = dst.with_name(dst.name + ".part")
    out = gdal.GetDriverByName("GTiff").Create(
        str(tmp), x1 - x0, y1 - y0, 2, gdal.GDT_Byte,
        options=["TILED=YES", "COMPRESS=DEFLATE", "PREDICTOR=2"],
    )
    out.SetGeoTransform((gt[0] + x0 * px, px, 0, gt[3] - y0 * px, 0, -px))
    out.SetProjection(ds.GetProjection())
    for row in range(y0, y1, 2048):
        rows = min(2048, y1 - row)
        classes = band.ReadAsArray(x0, row, x1 - x0, rows)
        out.GetRasterBand(1).WriteArray(np.where(classes == WORLDCOVER_WATER, 255, 0).astype(np.uint8), 0, row - y0)
        out.GetRasterBand(2).WriteArray(np.where(classes != 0, 255, 0).astype(np.uint8), 0, row - y0)
    out = None
    tmp.rename(dst)
    return str(dst)


def water_mosaic(name, cache, files, bounds, px_deg, level, pool):
    """A two-band COG over `bounds` (water, has data; 0-255 fractions)
    averaged to `px_deg` from WorldCover at `level` (see
    `classify_water`), with averaged overviews so each zoom reads its
    water fraction directly."""
    out = cache / f"{name}.tif"
    if out.exists():
        return str(out)
    log(f"water mosaic {name}")
    parts = cache / "worldcover" / name
    parts.mkdir(parents=True, exist_ok=True)
    jobs = [(f, parts / Path(f).name, level, bounds) for f in files]
    classified = [p for p in pool.map(classify_water, jobs) if p]
    west, south, east, north = bounds
    tmp = out.with_name(out.name + ".part")
    gdal.Warp(
        str(tmp),
        classified,
        format="COG",
        outputBounds=(west, south, east, north),
        xRes=px_deg,
        yRes=px_deg,
        resampleAlg="average",
        multithread=True,
        warpMemoryLimit=4096,
        warpOptions=["NUM_THREADS=ALL_CPUS"],
        creationOptions=[
            "COMPRESS=DEFLATE",
            "PREDICTOR=2",
            "BIGTIFF=IF_SAFER",
            "OVERVIEW_RESAMPLING=AVERAGE",
            "NUM_THREADS=ALL_CPUS",
        ],
    )
    tmp.rename(out)
    return str(out)


def grow(bounds, margin):
    west, south, east, north = bounds
    return (west - margin, south - margin, east + margin, north + margin)


# Warping and merging --------------------------------------------------------


def warp(src, name, z, tiles):
    """`src` (a raster) warped onto zoom z's pixel grid over `tiles` (x0, y0, x1, y1),
    with a border pixel all round. NaN where the source has no data."""
    x0, y0, x1, y1 = tiles
    size = 2 * WORLD_M / 2**z
    px = size / TILE
    bounds = (
        -WORLD_M + x0 * size - px * BORDER,
        WORLD_M - (y1 + 1) * size - px * BORDER,
        -WORLD_M + (x1 + 1) * size + px * BORDER,
        WORLD_M - y0 * size + px * BORDER,
    )
    shrinking = ground_px_m(z) >= SOURCE_PX_M[name]
    ds = gdal.Warp(
        "",
        src,
        format="MEM",
        dstSRS="EPSG:3857",
        outputBounds=bounds,
        width=(x1 - x0 + 1) * TILE + 2 * BORDER,
        height=(y1 - y0 + 1) * TILE + 2 * BORDER,
        resampleAlg="average" if shrinking else "cubic",
        dstNodata=float("nan"),
        outputType=gdal.GDT_Float32,
        multithread=True,
        warpMemoryLimit=4096,
        warpOptions=["NUM_THREADS=ALL_CPUS"],
    )
    data = ds.GetRasterBand(1).ReadAsArray()
    ds = None
    return data


# Copernicus sea is at least this deep (m); see `merge`.
SEA_FLOOR_M = 1.0

def warp_water(src, z, tiles):
    """The water mosaic on zoom z's grid (as `warp`): water and has-data
    fractions, 0 to 1."""
    x0, y0, x1, y1 = tiles
    size = 2 * WORLD_M / 2**z
    px = size / TILE
    ds = gdal.Warp(
        "",
        src,
        format="MEM",
        dstSRS="EPSG:3857",
        outputBounds=(
            -WORLD_M + x0 * size - px * BORDER,
            WORLD_M - (y1 + 1) * size - px * BORDER,
            -WORLD_M + (x1 + 1) * size + px * BORDER,
            WORLD_M - y0 * size + px * BORDER,
        ),
        width=(x1 - x0 + 1) * TILE + 2 * BORDER,
        height=(y1 - y0 + 1) * TILE + 2 * BORDER,
        resampleAlg="average",
        outputType=gdal.GDT_Float32,
        multithread=True,
        warpMemoryLimit=4096,
        warpOptions=["NUM_THREADS=ALL_CPUS"],
    )
    water, valid = (ds.GetRasterBand(i).ReadAsArray() / 255 for i in (1, 2))
    ds = None
    return water, valid


# CUDEM blends in over this many pixels from the edge of its coverage.
# Inland, it (bare earth) meets Copernicus (a surface model with trees and
# buildings) meters apart; an abrupt switch shades as a ridge.
FEATHER_PX = 8


def coverage_weight(valid, steps):
    """0 outside `valid`, rising to 1 at `steps` pixels inside its edge: the
    fraction of `steps` 4-neighbor erosions each pixel survives."""
    weight = np.zeros(valid.shape, np.float32)
    inside = valid.copy()
    for _ in range(steps):
        weight += inside
        eroded = inside.copy()
        eroded[1:, :] &= inside[:-1, :]
        eroded[:-1, :] &= inside[1:, :]
        eroded[:, 1:] &= inside[:, :-1]
        eroded[:, :-1] &= inside[:, 1:]
        inside = eroded
    return weight / steps


def merge(etopo, land, cudem):
    """Heights from the three sources by priority; see the module doc."""
    h = np.nan_to_num(etopo, nan=0.0)
    has_land = ~np.isnan(land)
    # Copernicus marks the sea as exactly 0: those pixels keep ETOPO's
    # depth, but at least SEA_FLOOR_M deep, so ETOPO's coarse coastline
    # can't leave sea at or above sea level (where the map would draw it as
    # coast). Land is anything else Copernicus has.
    sea = has_land & (land == 0)
    h = np.where(sea, np.minimum(h, -SEA_FLOOR_M), np.where(has_land, land, h))
    valid = ~np.isnan(cudem)
    w = coverage_weight(valid, FEATHER_PX)
    return np.where(valid, w * np.nan_to_num(cudem) + (1 - w) * h, h)


def water_widths(water, z, y0):
    """Per pixel, the width code (see the module doc) of the water there:
    roughly how wide the water body is.

    The water fraction is averaged into blocks of 2, 4, 8 ... pixels; a
    spot's width is the largest block size at which it's still at least
    half water. Each level's water is grown by one block before mapping
    back, so a big lake's shore keeps the lake's width rather than the
    narrow one its edge pixels alone would give. `y0` is the raster's first
    tile row, for each row's ground scale."""
    rows, cols = water.shape
    level = np.zeros((rows, cols), np.uint8)
    pooled = water.astype(np.float32)
    for lv in range(1, WIDTH_LEVELS + 1):
        h, w = pooled.shape
        padded = np.zeros((h + h % 2, w + w % 2), np.float32)
        padded[:h, :w] = pooled
        pooled = padded.reshape(padded.shape[0] // 2, 2, padded.shape[1] // 2, 2).mean(axis=(1, 3))
        wet = pooled >= 0.5
        if not wet.any():
            break
        grown = wet.copy()
        grown[1:] |= wet[:-1]
        grown[:-1] |= wet[1:]
        grown[:, 1:] |= wet[:, :-1]
        grown[:, :-1] |= wet[:, 1:]
        f = 2**lv
        up = np.repeat(np.repeat(grown, f, axis=0), f, axis=1)[:rows, :cols]
        level[up] = lv
    # Block size in meters, row by row (Mercator pixels shrink poleward).
    n = TILE * 2**z
    y = y0 * TILE - BORDER + np.arange(rows) + 0.5
    m_per_px = 2 * WORLD_M / n * np.cos(np.arctan(np.sinh(np.pi * (1 - 2 * y / n))))
    width_m = (2.0 ** level) * m_per_px[:, None]
    code = np.clip(np.round(WIDTH_CODE_SCALE * np.log2(width_m)), 1, 255).astype(np.uint8)
    # Land keeps 0, except where it borders water (shorelines know the
    # width of their water).
    near = water >= 0.5
    for _ in range(SIZE_GROW_PX):
        grown = near.copy()
        grown[1:] |= near[:-1]
        grown[:-1] |= near[1:]
        grown[:, 1:] |= near[:, :-1]
        grown[:, :-1] |= near[:, 1:]
        near = grown
    return np.where(near, code, 0).astype(np.uint8)


def encode(heights, water, sizes):
    """RGBA of heights (m), water body sizes and water fractions (see the
    module doc)."""
    v = np.clip(np.round((heights + HEIGHT_OFFSET_M) * HEIGHT_STEPS), 0, 65535).astype(np.uint16)
    a = 1 + np.round(np.clip(water, 0, 1) * 254)
    return np.stack([v >> 8, v & 255, sizes, a]).astype(np.uint8)


def write_png(job):
    path, rgba = job
    path.parent.mkdir(parents=True, exist_ok=True)
    mem = gdal.GetDriverByName("MEM").Create("", rgba.shape[2], rgba.shape[1], 4, gdal.GDT_Byte)
    for i in range(4):
        mem.GetRasterBand(i + 1).WriteArray(rgba[i])
    gdal.GetDriverByName("PNG").CreateCopy(str(path), mem, options=["ZLEVEL=9"])
    return path.stat().st_size


def build_zoom(z, area, keep, sources, water_src, out, pool):
    tiles = tile_range(area, z)
    x0, y0, x1, y1 = tiles
    wanted = [
        (x, y) for x in range(x0, x1 + 1) for y in range(y0, y1 + 1) if keep(x, y, z)
    ]
    log(f"z{z}: warping {x1 - x0 + 1}x{y1 - y0 + 1} tiles, keeping {len(wanted)}")
    # ETOPO, then land, then CUDEM layers, finer ones over coarser.
    etopo, land, *cudem_layers = (warp(src, name, z, tiles) for name, src in sources)
    cudem = cudem_layers[0]
    for finer in cudem_layers[1:]:
        cudem = np.where(np.isnan(finer), cudem, finer)
    heights = merge(etopo, land, cudem)
    del etopo, land, cudem, cudem_layers
    # Water where WorldCover says so, else (no data there) below sea level.
    wc_water, wc_valid = warp_water(water_src, z, tiles)
    water = wc_water + (1 - wc_valid) * (heights < 0)
    del wc_water, wc_valid
    started = time.time()
    sizes = water_widths(water, z, y0)
    log(f"z{z}: water widths in {time.time() - started:.0f} s")
    span = TILE + 2 * BORDER
    window = lambda a, x, y: a[(y - y0) * TILE : (y - y0) * TILE + span, (x - x0) * TILE : (x - x0) * TILE + span]
    jobs = (
        (
            out / str(z) / str(x) / f"{y}.png",
            encode(window(heights, x, y), window(water, x, y), window(sizes, x, y)),
        )
        for x, y in wanted
    )
    size = sum(pool.map(write_png, jobs, chunksize=16))
    log(f"z{z}: {len(wanted)} tiles, {size / 1e6:.1f} MB")
    return wanted, size


def main():
    cache, out = Path("/cache"), Path("/out/elevation")
    cache.mkdir(parents=True, exist_ok=True)
    out.mkdir(parents=True, exist_ok=True)
    only = os.environ.get("ZOOMS")
    zooms = (
        [int(z) for z in only.split(",")]
        if only
        else [*SLICE_ZOOMS, *REGION_ZOOMS, *DETAIL_ZOOMS]
    )

    bounds = slice_bounds()
    log(f"slice bounds {tuple(round(b, 2) for b in bounds)}")
    etopo = etopo_files(cache, bounds)
    touches = lambda lon, lat: any(in_slice(lon + i, lat + j) for i in (0, 1) for j in (0, 1))
    # GLO-90's first overview is about 180 m, enough for zoom 9.
    glo90 = glo_jobs(cache, 90, 0, bounds, touches)
    glo30 = glo_jobs(cache, 30, None, bounds, touches)
    cudem = cudem_jobs(cache)
    for d in ("glo90", "glo30", "cudem"):
        (cache / d).mkdir(exist_ok=True)
    with ProcessPoolExecutor(max_workers=8) as pool:
        jobs = glo90 + glo30 + cudem
        for i, _ in enumerate(pool.map(localize, jobs), 1):
            if i % 50 == 0 or i == len(jobs):
                log(f"localized {i}/{len(jobs)}")
    files = lambda jobs: [str(dst) for _, dst, _ in jobs]
    # Areas no file covers read as nodata, not 0 (which would read as sea).
    etopo = build_vrt(cache / "etopo.vrt", etopo, vrt_nodata=-99999)
    glo30 = build_vrt(cache / "glo30.vrt", files(glo30), vrt_nodata=-99999)
    glo90 = mosaic("glo90", cache, files(glo90), grow(bounds, 1), GLO90_DEG)
    cudem_files = files(cudem)
    cudem_coast = mosaic(
        "cudem_coast", cache, cudem_files, (-125.5, 45.5, -121.5, 49.5), CUDEM_COAST_DEG
    )
    cudem_detail = mosaic("cudem_detail", cache, cudem_files, grow(PUGET_SOUND, 0.1), CUDEM_DETAIL_DEG)
    worldcover = worldcover_files(cache, bounds)
    with ProcessPoolExecutor(max_workers=4) as pool:
        water_region = water_mosaic(
            "water_region", cache, worldcover, grow(bounds, 0.2), WATER_REGION_DEG, 1, pool
        )
        water_detail = water_mosaic(
            "water_detail", cache, worldcover, grow(PUGET_SOUND, 0.1), WATER_DETAIL_DEG, 0, pool
        )

    index_path = out / "index.json"
    index = json.loads(index_path.read_text()) if index_path.exists() else {"levels": {}}
    total = 0
    with ProcessPoolExecutor() as pool:
        for z in zooms:
            if z in SLICE_ZOOMS:
                area, keep = bounds, tile_touches_slice
                sources = [("etopo", etopo), ("glo90", glo90), ("cudem_coast", cudem_coast)]
            else:
                area, keep = (
                    (bounds, tile_touches_slice)
                    if z in REGION_ZOOMS
                    else (PUGET_SOUND, lambda x, y, z: True)
                )
                sources = [
                    ("etopo", etopo),
                    ("glo30", glo30),
                    ("cudem_coast", cudem_coast),
                    ("cudem_detail", cudem_detail),
                ]
            water = water_detail if z in DETAIL_ZOOMS else water_region
            tiles, size = build_zoom(z, area, keep, sources, water, out, pool)
            index["levels"][str(z)] = sorted(tiles)
            total += size
    index.update(
        {
            "tile_size": TILE,
            "border": BORDER,
            "encoding": "height16 + water body size + water fraction (see elevation.py)",
            "attribution": ATTRIBUTION,
        }
    )
    index["levels"] = dict(sorted(index["levels"].items(), key=lambda kv: int(kv[0])))
    index_path.write_text(json.dumps(index, separators=(",", ":")))
    log(f"done: {total / 1e6:.1f} MB written")


if __name__ == "__main__":
    sys.exit(main())
