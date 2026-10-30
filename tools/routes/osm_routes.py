"""Washington State Ferries routes from OpenStreetMap, as cubic Béziers.

Fetches every way that belongs to a WSF ferry route relation in the
Salish Sea (through the Overpass API, cached under data/osm/), keeps the
ferry lines, drops ways that duplicate another (OSM often draws a route
once per direction, side by side), and fits each line with cubic Bézier
curves to within FIT_TOLERANCE_M. Writes tiles/routes/ferry-routes.json,
which the app loads alongside the terrain tiles, with the SERVICES table
below.

    python3 tools/routes/osm_routes.py        # REFRESH=1 to fetch again

Route geometry © OpenStreetMap contributors, under the ODbL.
"""

import datetime
import json
import math
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

# The public Overpass servers; busy ones answer 429 or 504, so try each,
# backing off between rounds.
MIRRORS = [
    "https://overpass-api.de/api/interpreter",
    "https://overpass.private.coffee/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
]
ROUNDS = 4
FIRST_WAIT_S = 10
USER_AGENT = "ferry-demo route extract (non-commercial; github.com/salishseawolf)"
# South, west, north, east: the Salish Sea's US waters.
BBOX = (47.0, -123.6, 48.9, -122.0)
QUERY = f"""[out:json][timeout:120];
relation["route"="ferry"]["operator"="Washington State Ferries"]{BBOX}->.routes;
way(r.routes)->.ways;
.ways out geom tags;"""

# The routes riders know, where one serves more than two terminals: OSM
# draws each leg on its own, and WSF's schedule service splits the
# triangle into three routes, but each of these has one public timetable
# and its boats work all its legs. Terminals are named as WSF's terminals
# service names them; the app joins any other leg to the route of its own
# two terminals.
SERVICES = [
    {
        "name": "Fauntleroy / Vashon / Southworth",
        "terminals": ["Fauntleroy", "Vashon Island", "Southworth"],
    },
    {
        "name": "Anacortes / San Juan Islands",
        "terminals": ["Anacortes", "Lopez Island", "Shaw Island", "Orcas Island", "Friday Harbor"],
    },
]

CACHE = "data/osm/wsf-routes.json"
OUT = "tiles/routes/ferry-routes.json"
ATTRIBUTION = "Ferry routes © OpenStreetMap contributors (ODbL)"

# Curves stay within this distance (m) of OSM's line, checked at points
# resampled every DENSIFY_M along it (the fit only checks at its input
# points, and OSM's are often kilometers apart).
FIT_TOLERANCE_M = 15.0
DENSIFY_M = 40.0
# A way whose points all lie within this distance (m) of a longer way
# with the same name duplicates it.
DUPLICATE_M = 250.0
# Local meters around the slice's latitude: an affine map of lon/lat, so
# Béziers fitted in it are the same curves in lon/lat.
LAT0 = 47.6
M_PER_DEG_LAT = 111_320.0
M_PER_DEG_LON = M_PER_DEG_LAT * math.cos(math.radians(LAT0))


def fetch():
    body = urllib.parse.urlencode({"data": QUERY}).encode()
    wait = FIRST_WAIT_S
    for round_ in range(ROUNDS):
        for url in MIRRORS:
            request = urllib.request.Request(url, body, {"User-Agent": USER_AGENT})
            try:
                with urllib.request.urlopen(request, timeout=180) as r:
                    text = r.read().decode()
                if text.lstrip().startswith("{"):
                    return text
                print(f"{url}: not JSON (busy?)", file=sys.stderr)
            except (urllib.error.URLError, TimeoutError) as e:
                print(f"{url}: {e}", file=sys.stderr)
        if round_ < ROUNDS - 1:
            print(f"all mirrors busy; retrying in {wait} s", file=sys.stderr)
            time.sleep(wait)
            wait *= 2
    sys.exit("Overpass unavailable; try again later")


def to_m(lon, lat):
    return (lon * M_PER_DEG_LON, lat * M_PER_DEG_LAT)


def to_lonlat(p):
    return (round(p[0] / M_PER_DEG_LON, 6), round(p[1] / M_PER_DEG_LAT, 6))


# 2D vector helpers on tuples.
def add(a, b):
    return (a[0] + b[0], a[1] + b[1])


def sub(a, b):
    return (a[0] - b[0], a[1] - b[1])


def mul(a, k):
    return (a[0] * k, a[1] * k)


def dot(a, b):
    return a[0] * b[0] + a[1] * b[1]


def norm(a):
    return math.hypot(a[0], a[1])


def unit(a):
    n = norm(a)
    return (a[0] / n, a[1] / n) if n else (0.0, 0.0)


def length_m(points):
    return sum(norm(sub(b, a)) for a, b in zip(points, points[1:]))


def distance_to_polyline(p, line):
    best = math.inf
    for a, b in zip(line, line[1:]):
        ab = sub(b, a)
        t = max(0.0, min(1.0, dot(sub(p, a), ab) / max(dot(ab, ab), 1e-9)))
        best = min(best, norm(sub(p, add(a, mul(ab, t)))))
    return best


# Cubic Bézier fitting after Schneider, "An Algorithm for Automatically
# Fitting Digitized Curves" (Graphics Gems, 1990).
def bezier(c, t):
    s = 1 - t
    return add(
        add(mul(c[0], s * s * s), mul(c[1], 3 * s * s * t)),
        add(mul(c[2], 3 * s * t * t), mul(c[3], t * t * t)),
    )


def bezier_d1(c, t):
    s = 1 - t
    return add(
        add(mul(sub(c[1], c[0]), 3 * s * s), mul(sub(c[2], c[1]), 6 * s * t)),
        mul(sub(c[3], c[2]), 3 * t * t),
    )


def bezier_d2(c, t):
    return add(
        mul(add(sub(c[2], mul(c[1], 2)), c[0]), 6 * (1 - t)),
        mul(add(sub(c[3], mul(c[2], 2)), c[1]), 6 * t),
    )


def chord_params(points):
    u = [0.0]
    for a, b in zip(points, points[1:]):
        u.append(u[-1] + norm(sub(b, a)))
    return [x / u[-1] for x in u] if u[-1] else u


def generate(points, u, t1, t2):
    first, last = points[0], points[-1]
    c = [[0.0, 0.0], [0.0, 0.0]]
    x = [0.0, 0.0]
    for p, t in zip(points, u):
        s = 1 - t
        a1 = mul(t1, 3 * s * s * t)
        a2 = mul(t2, 3 * s * t * t)
        c[0][0] += dot(a1, a1)
        c[0][1] += dot(a1, a2)
        c[1][1] += dot(a2, a2)
        rest = sub(p, bezier((first, first, last, last), t))
        x[0] += dot(a1, rest)
        x[1] += dot(a2, rest)
    c[1][0] = c[0][1]
    det = c[0][0] * c[1][1] - c[0][1] * c[1][0]
    alpha_l = (x[0] * c[1][1] - x[1] * c[0][1]) / det if det else 0.0
    alpha_r = (c[0][0] * x[1] - c[1][0] * x[0]) / det if det else 0.0
    span = norm(sub(last, first))
    if alpha_l < 1e-6 * span or alpha_r < 1e-6 * span:
        alpha_l = alpha_r = span / 3
    return (first, add(first, mul(t1, alpha_l)), add(last, mul(t2, alpha_r)), last)


def max_error(points, curve, u):
    worst, split = 0.0, len(points) // 2
    for i, (p, t) in enumerate(zip(points, u)):
        d = norm(sub(bezier(curve, t), p))
        if d > worst:
            worst, split = d, i
    return worst, split


def reparameterize(points, curve, u):
    out = []
    for p, t in zip(points, u):
        d = sub(bezier(curve, t), p)
        d1, d2 = bezier_d1(curve, t), bezier_d2(curve, t)
        denom = dot(d1, d1) + dot(d, d2)
        out.append(t - dot(d, d1) / denom if denom else t)
    return out


def fit(points, t1, t2, tolerance):
    if len(points) == 2:
        third = norm(sub(points[1], points[0])) / 3
        return [(points[0], add(points[0], mul(t1, third)), add(points[1], mul(t2, third)), points[1])]
    u = chord_params(points)
    curve = generate(points, u, t1, t2)
    error, split = max_error(points, curve, u)
    if error < tolerance:
        return [curve]
    if error < 4 * tolerance:
        for _ in range(20):
            u = reparameterize(points, curve, u)
            curve = generate(points, u, t1, t2)
            error, split = max_error(points, curve, u)
            if error < tolerance:
                return [curve]
    split = min(max(split, 1), len(points) - 2)
    center = unit(sub(points[split - 1], points[split + 1]))
    return fit(points[: split + 1], t1, center, tolerance) + fit(
        points[split:], mul(center, -1), t2, tolerance
    )


def densify(points):
    out = [points[0]]
    for a, b in zip(points, points[1:]):
        steps = max(1, math.ceil(norm(sub(b, a)) / DENSIFY_M))
        out.extend(add(a, mul(sub(b, a), i / steps)) for i in range(1, steps + 1))
    return out


def fit_line(points):
    points = densify(points)
    t1 = unit(sub(points[1], points[0]))
    t2 = unit(sub(points[-2], points[-1]))
    return fit(points, t1, t2, FIT_TOLERANCE_M)


def main():
    if os.environ.get("REFRESH") or not os.path.exists(CACHE):
        os.makedirs(os.path.dirname(CACHE), exist_ok=True)
        text = fetch()
        with open(CACHE, "w") as f:
            f.write(text)
    with open(CACHE) as f:
        data = json.load(f)

    ways = [
        e for e in data["elements"]
        if e["type"] == "way" and e.get("tags", {}).get("route") == "ferry"
    ]
    lines = []
    for w in ways:
        points = [to_m(g["lon"], g["lat"]) for g in w["geometry"]]
        # Consecutive repeats would give zero-length tangents.
        points = [p for i, p in enumerate(points) if i == 0 or p != points[i - 1]]
        if len(points) >= 2:
            lines.append({"way": w["id"], "name": w["tags"].get("name", ""), "points": points})
    # Longest first, so a duplicate is always the shorter of a pair.
    lines.sort(key=lambda l: -length_m(l["points"]))
    kept = []
    for line in lines:
        duplicate = any(
            k["name"] == line["name"]
            and all(distance_to_polyline(p, k["points"]) < DUPLICATE_M for p in line["points"])
            for k in kept
        )
        if duplicate:
            print(f"  dropping way {line['way']} ({line['name']}): duplicates another")
        else:
            kept.append(line)

    routes = []
    for line in sorted(kept, key=lambda l: l["name"]):
        curves = fit_line(line["points"])
        routes.append({
            "way": line["way"],
            "name": line["name"],
            "curves": [[to_lonlat(p) for p in c] for c in curves],
        })
        print(f"  {line['name']}: {len(line['points'])} points -> {len(curves)} curves")

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    fetched = datetime.datetime.fromtimestamp(os.path.getmtime(CACHE), datetime.UTC)
    with open(OUT, "w") as f:
        json.dump({
            "attribution": ATTRIBUTION,
            "source": "OpenStreetMap ways in Washington State Ferries route relations",
            "fetched": fetched.strftime("%Y-%m-%d"),
            "tolerance_m": FIT_TOLERANCE_M,
            "services": SERVICES,
            "routes": routes,
        }, f, separators=(",", ":"))
    print(f"{len(routes)} routes -> {OUT}")


if __name__ == "__main__":
    main()
