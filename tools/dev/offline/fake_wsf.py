"""A stand-in for WSF's API, for running the app with no network or key.

    python3 fake_wsf.py [PORT]        (default 8095)

Point the server at it with WSF_BASE=http://127.0.0.1:PORT/ferries/api/
(run.sh does). Live and reference paths answer with real answers saved
from the dev database on 2026-10-30 (answers/<path, / as _>.json), so the
boats stay where they were. Schedule paths answer for whatever day is
asked with two routes: Fauntleroy / Vashon / Southworth, from WSF's
published fall 2026 times (Southworth's own sailings are made up), and
Edmonds / Kingston, made up, every 40 minutes.

FAKE_WSF_DROP lists what WSF has stopped publishing, to try a route or leg
with no sailings: "6" for WSF's route 6, "13:20" for the route's sailings
from terminal 20, "13:9>20" for its leg from 9 to 20.

FAKE_WSF_FLUSH_MOVES=1 moves the schedule flush date at every look, as WSF's
does, to try how the server reloads.
"""

import json
import os
import sys
from datetime import date, datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from zoneinfo import ZoneInfo

ANSWERS = Path(__file__).parent / "answers"
DROP = os.environ.get("FAKE_WSF_DROP", "").split()
FLUSH_MOVES = os.environ.get("FAKE_WSF_FLUSH_MOVES") == "1"
PACIFIC = ZoneInfo("America/Los_Angeles")

VESSELS = {"Kitsap": 6, "Cathlamet": 1, "Salish": 66, "Walla Walla": 36, "Spokane": 32}

# (clock, vessel[, annotation indexes]); "+" is after midnight.
FAUNTLEROY_VASHON = [
    ("5:05 AM", "Kitsap"), ("5:45 AM", "Cathlamet"), ("6:45 AM", "Kitsap"), ("7:40 AM", "Kitsap"),
    ("8:15 AM", "Cathlamet"), ("9:10 AM", "Cathlamet"), ("9:30 AM", "Kitsap"), ("10:25 AM", "Kitsap"),
    ("11:30 AM", "Kitsap"), ("12:10 PM", "Cathlamet"), ("1:15 PM", "Kitsap"), ("1:35 PM", "Cathlamet"),
    ("2:10 PM", "Kitsap"), ("3:00 PM", "Cathlamet"), ("3:50 PM", "Kitsap", [0]), ("4:35 PM", "Cathlamet"),
    ("5:15 PM", "Kitsap"), ("6:15 PM", "Cathlamet"), ("6:35 PM", "Kitsap"), ("7:35 PM", "Cathlamet"),
    ("7:55 PM", "Kitsap"), ("9:15 PM", "Kitsap"), ("10:35 PM", "Kitsap"), ("11:50 PM", "Kitsap"),
    ("1:05 AM+", "Kitsap"),
]
VASHON_FAUNTLEROY = [
    ("4:05 AM", "Cathlamet"), ("4:30 AM", "Kitsap"), ("5:20 AM", "Cathlamet"), ("6:15 AM", "Kitsap", [0]),
    ("6:55 AM", "Cathlamet"), ("7:15 AM", "Kitsap"), ("7:50 AM", "Cathlamet"), ("8:40 AM", "Cathlamet"),
    ("9:00 AM", "Kitsap"), ("9:55 AM", "Kitsap"), ("10:20 AM", "Cathlamet"), ("11:45 AM", "Cathlamet"),
    ("12:40 PM", "Kitsap"), ("1:40 PM", "Kitsap"), ("3:20 PM", "Kitsap"), ("4:45 PM", "Kitsap"),
    ("5:45 PM", "Cathlamet"), ("6:40 PM", "Cathlamet", [1]), ("8:15 PM", "Kitsap", [1]),
    ("10:05 PM", "Kitsap"), ("11:00 PM", "Kitsap", [1]), ("12:15 AM+", "Kitsap", [1]),
]
SOUTHWORTH = [("6:20 AM", "Kitsap"), ("10:40 AM", "Kitsap"), ("4:05 PM", "Kitsap"), ("7:20 PM", "Kitsap")]


def wsf_date(dt):
    offset = dt.utcoffset() // timedelta(minutes=1)
    sign = "-" if offset < 0 else "+"
    return f"/Date({int(dt.timestamp() * 1000)}{sign}{abs(offset) // 60:02}{abs(offset) % 60:02})/"


def at(day, clock):
    t = datetime.strptime(clock.rstrip("+"), "%I:%M %p")
    d = day + timedelta(days=1) if clock.endswith("+") else day
    return datetime(d.year, d.month, d.day, t.hour, t.minute, tzinfo=PACIFIC)


def every(first_hour, minutes, count, boats):
    start = datetime(2000, 1, 1, first_hour)
    out = []
    for i in range(count):
        t = start + timedelta(minutes=minutes * i)
        clock = t.strftime("%I:%M %p").lstrip("0") + ("+" if t.day != start.day else "")
        out.append((clock, boats[i % 2]))
    return out


def combo(day, route, dep, arr, notes, rows, minutes):
    via = {i for i, n in enumerate(notes) if n.startswith("Via")}
    times = []
    for clock, vessel, *rest in rows:
        indexes = rest[0] if rest else []
        leaves = at(day, clock)
        times.append({
            "DepartingTime": wsf_date(leaves),
            "ArrivingTime": wsf_date(leaves + timedelta(minutes=45 if via & set(indexes) else minutes)),
            "LoadingRule": 1 if indexes == [0] and notes[0].startswith("Loads") else 3,
            "VesselID": VESSELS[vessel], "VesselName": vessel,
            "VesselHandicapAccessible": True, "VesselPositionNum": 1,
            "Routes": [route], "AnnotationIndexes": indexes,
        })
    return {"DepartingTerminalID": dep, "ArrivingTerminalID": arr, "SailingNotes": "",
            "Annotations": notes, "AnnotationsIVR": [], "Times": times}


def schedule(day, route):
    head = {"ScheduleID": 201, "ScheduleName": "Fall 2026", "ScheduleSeason": 3,
            "SchedulePDFUrl": "", "AllRoutes": [route]}
    if route == 6:
        combos = [
            combo(day, 6, 8, 12, [], every(5, 40, 28, ["Walla Walla", "Spokane"]), 30),
            combo(day, 6, 12, 8, [], every(4, 40, 29, ["Spokane", "Walla Walla"]), 30),
        ]
    else:
        via = "Via Southworth, crossing time 45 minutes"
        combos = [
            combo(day, 13, 9, 22, [via], FAUNTLEROY_VASHON, 20),
            # Kitsap's 3:50 calls at Southworth first.
            combo(day, 13, 9, 20, [], [("3:50 PM", "Kitsap")], 30),
            combo(day, 13, 22, 9, ["Loads foot passengers and pre-registered carpools and vanpools only",
                                   via], VASHON_FAUNTLEROY, 20),
            combo(day, 13, 20, 22, [], SOUTHWORTH, 15),
            combo(day, 13, 20, 9, [], SOUTHWORTH, 35),
        ]
    combos = [c for c in combos if not dropped(route, c)]
    return head | {"TerminalCombos": combos}


def dropped(route, combo):
    leg = f"{route}:{combo['DepartingTerminalID']}"
    return leg in DROP or f"{leg}>{combo['ArrivingTerminalID']}" in DROP


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        parts = self.path.split("?")[0].removeprefix("/ferries/api/").strip("/").split("/")
        body = None
        if parts == ["schedule", "rest", "cacheflushdate"]:
            flushed = datetime.now(PACIFIC)
            if not FLUSH_MOVES:
                flushed = flushed.replace(hour=0, minute=0, second=0, microsecond=0)
            body = json.dumps(wsf_date(flushed))
        elif parts[:3] == ["schedule", "rest", "routes"] and len(parts) == 4:
            body = json.dumps([{"RouteID": r} for r in (13, 6) if str(r) not in DROP])
        elif parts[:3] == ["schedule", "rest", "schedule"] and len(parts) == 5:
            body = json.dumps(schedule(date.fromisoformat(parts[3]), int(parts[4])))
        else:
            f = ANSWERS / ("_".join(parts) + ".json")
            if f.is_file():
                body = f.read_text()
        if body is None:
            self.send_response(404)
            self.end_headers()
            return
        data = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8095
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
