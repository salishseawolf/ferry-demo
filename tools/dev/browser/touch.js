// Touch checks for smoke.js, at phone size, with real multi-touch sent
// through the DevTools protocol (web/gestures.rs): a pinch zooms about
// the fingers' midpoint and moves with it, lifting a finger doesn't make
// the map jump, a wobbly tap still picks what it's on, a double tap on
// nothing zooms in, and measuring by tap measures between the taps. The
// view is read from the debug panel's Link row.
const { wait } = require("./app");

// The view: its center and altitude (m), and ground meters per CSS px.
async function view(page) {
  await wait(300);
  return page.evaluate(() => {
    const rows = {};
    const dl = document.querySelectorAll("#perf-body dt");
    for (const dt of dl) rows[dt.textContent] = dt.nextElementSibling.textContent;
    const [lat, lon, alt] = rows.Link.split("#at=")[1].split(",").map(Number);
    const mPerPx = Number(/([\d.]+) m\/px/.exec(rows.View)[1]);
    return { lat, lon, alt, mPerPx };
  });
}

// Meters from view `a`'s center to `b`'s, east and south (as screen x, y).
function moved(a, b) {
  const m = 111320;
  return [(b.lon - a.lon) * m * Math.cos((a.lat * Math.PI) / 180), (a.lat - b.lat) * m];
}

async function checkTouch(page, check, size) {
  const cdp = await page.createCDPSession();
  const touch = (type, points) =>
    cdp.send("Input.dispatchTouchEvent", {
      type,
      // Each finger's id is its place in the list, unless it says.
      touchPoints: points.map(([x, y, id], k) => ({ x, y, id: id ?? k })),
    });
  // Fingers from `from` to `to` (lists of [x, y]) in `steps` moves.
  const slide = async (from, to, steps = 8) => {
    for (let i = 1; i <= steps; i++) {
      const t = i / steps;
      await touch(
        "touchMove",
        from.map(([x, y], k) => [x + (to[k][0] - x) * t, y + (to[k][1] - y) * t]),
      );
      await wait(16);
    }
  };
  const tap = async ([x, y], wobble = 0) => {
    await touch("touchStart", [[x, y]]);
    if (wobble) await touch("touchMove", [[x + wobble, y]]);
    await touch("touchEnd", []);
  };
  const pickable = async ([x, y]) => {
    await page.mouse.move(x, y);
    await wait(60);
    return page.evaluate(() => document.getElementById("world").classList.contains("pickable"));
  };
  await page.evaluate(() => document.getElementById("debug").click());
  await wait(500);
  // The debug panel hangs over the top on phones: touches stay below
  // it (one on it would zoom the page).
  const [w, h, below] = await page.evaluate(() => [
    innerWidth,
    innerHeight,
    document.getElementById("perf").getBoundingClientRect().bottom + 20,
  ]);
  // The band of map between the panel and the window's foot.
  const band = (share) => below + (h - 60 - below) * share;

  // Pinch out to twice the spread, about a point off the middle, while
  // moving 40 px right: half the altitude, the ground under the
  // midpoint carried along with it.
  const v0 = await view(page);
  const mid = [w / 2 - 60, band(0.5)];
  await touch("touchStart", [[mid[0] - 40, mid[1]]]);
  await touch("touchStart", [[mid[0] - 40, mid[1]], [mid[0] + 40, mid[1]]]);
  await slide(
    [[mid[0] - 40, mid[1]], [mid[0] + 40, mid[1]]],
    [[mid[0] - 40, mid[1]], [mid[0] + 120, mid[1]]],
  );
  const v1 = await view(page);
  const ratio = v1.alt / v0.alt;
  check(`${size}: a pinch out zooms in`, Math.abs(ratio - 0.5) < 0.05, `altitude × ${ratio.toFixed(3)}`);
  // The grabbed point: (mid - center) × m/px from the old center, now
  // at the new midpoint, 40 px right.
  const [cx, cy] = [w / 2, h / 2];
  const grabbed = [(mid[0] - cx) * v0.mPerPx, (mid[1] - cy) * v0.mPerPx];
  const now = [(mid[0] + 40 - cx) * v1.mPerPx, (mid[1] - cy) * v1.mPerPx];
  const [dx, dy] = moved(v0, v1);
  const off = Math.hypot(dx + now[0] - grabbed[0], dy + now[1] - grabbed[1]) / v1.mPerPx;
  check(`${size}: and keeps the ground between the fingers`, off < 4, `${off.toFixed(1)} px off`);
  // One finger up, the other dragging on: no jump, it drags.
  // touchEnd lists the fingers that lift.
  await touch("touchEnd", [[mid[0] - 40, mid[1], 0]]);
  const v2 = await view(page);
  const [jx, jy] = moved(v1, v2);
  check(`${size}: lifting a finger doesn't jump`, Math.hypot(jx, jy) / v2.mPerPx < 2);
  for (let x = mid[0] + 110; x >= mid[0] + 60; x -= 10) {
    await touch("touchMove", [[x, mid[1], 1]]);
    await wait(16);
  }
  await touch("touchEnd", []);
  const v3 = await view(page);
  const dragged = moved(v2, v3)[0] / v3.mPerPx;
  check(`${size}: and the other finger drags on`, Math.abs(dragged - 60) < 4, `${dragged.toFixed(1)} px`);

  // Back out, to see the whole sound again.
  const low = mid[1];
  await touch("touchStart", [[cx - 120, low], [cx + 120, low]]);
  await slide([[cx - 120, low], [cx + 120, low]], [[cx - 30, low], [cx + 30, low]]);
  await touch("touchEnd", []);
  await wait(500);

  // A double tap on open water (nothing there for the mouse's pointer).
  let nothing;
  for (let y = band(0); y < band(1) && !nothing; y += 8) {
    for (let x = 60; x < w - 30 && !nothing; x += 8) {
      if (!(await pickable([x, y]))) nothing = [x, y];
    }
  }
  await page.mouse.move(w / 2, 10);
  if (nothing) {
    const before = await view(page);
    await tap(nothing);
    await wait(120);
    await tap([nothing[0] + 5, nothing[1] + 3]);
    const after = await view(page);
    const zoom = after.alt / before.alt;
    check(`${size}: a double tap on nothing zooms in`, Math.abs(zoom - 0.5) < 0.05, `altitude × ${zoom.toFixed(3)}`);
  } else {
    check(`${size}: open water to double-tap`, false);
  }

  // Measuring: two taps 200 px apart measure that far on the ground.
  const scale = await view(page);
  await page.evaluate(() => document.getElementById("measure-tool").click());
  await tap([cx - 100, band(0.5)]);
  await wait(400);
  await tap([cx + 100, band(0.5)]);
  await wait(400);
  const shown = await page.$eval("#measure-distance", (d) => d.textContent);
  const got = parseFloat(shown.replace(/,/g, "")) * 1000;
  const want = 200 * scale.mPerPx;
  check(`${size}: measuring by tap`, Math.abs(got - want) / want < 0.03, `${shown}, ${Math.round(want)} m between the taps`);
  await page.evaluate(() => document.getElementById("measure-tool").click());

  // Drawn at two physical pixels to a CSS pixel, though the screen has
  // three; and, flat, no more basemap images than the screen needs.
  const drawn = await page.evaluate(() => {
    const c = document.getElementById("world");
    return c.width / c.clientWidth;
  });
  check(`${size}: drawn at 2× on a 3× screen`, Math.abs(drawn - 2) < 0.01, `${drawn.toFixed(2)}×`);
  const images = await page.evaluate(() => {
    for (const dt of document.querySelectorAll("#perf-body dt"))
      if (dt.textContent === "Basemap images") return dt.nextElementSibling.textContent;
    return null;
  });
  if (images !== null) {
    const mb = parseFloat(images.replace(/[^\d.]/g, "")) * (/ MB/.test(images) ? 1 : 1 / 1024);
    check(`${size}: basemap images within a phone's means`, mb <= 64, images);
  }
  await page.evaluate(() => document.getElementById("debug").click());
  await wait(300);

  // Last, as it opens the finder's sheet: a tap wobbling 7 px (past a
  // mouse's 4 px slop, within a finger's) on something the mouse's
  // pointer finds, with the panel closed and the whole map to look over.
  let thing;
  for (let y = h * 0.25; y < h * 0.85 && !thing; y += 12) {
    for (let x = 30; x < w - 30 && !thing; x += 12) {
      if (await pickable([x, y])) thing = [x, y];
    }
  }
  await page.mouse.move(w / 2, 10);
  check(`${size}: something on the map to tap`, !!thing);
  if (thing) {
    await tap(thing, 7);
    await wait(1500);
    const picked = await page.evaluate(() => !!document.querySelector("#finder-list .hit.selected"));
    check(`${size}: a wobbly tap picks what it's on`, picked);
  }
  await cdp.detach();
}

// The details sheet (web/sheet.rs), with matches showing: dragged by its
// handle it follows the finger and settles at the nearest height, a flick
// carries it on, and a tap still steps it on.
async function checkSheet(page, check, size) {
  const cdp = await page.createCDPSession();
  const touch = (type, points) =>
    cdp.send("Input.dispatchTouchEvent", { type, touchPoints: points.map(([x, y]) => ({ x, y, id: 0 })) });
  const sheet = () =>
    page.evaluate(() => {
      const f = document.getElementById("finder");
      const height = document.getElementById("finder-drop").getBoundingClientRect().height;
      return { at: ["peek", "half", "full"].find((s) => f.classList.contains(`sheet-${s}`)), height };
    });
  const handle = async () => {
    const r = await page.$eval("#finder-handle", (h) => h.getBoundingClientRect().toJSON());
    return [r.x + r.width / 2, r.y + r.height / 2];
  };
  // From the handle by `dy` (up negative) in `steps` moves `ms` apart.
  const drag = async (dy, steps, ms) => {
    const [x, y] = await handle();
    await touch("touchStart", [[x, y]]);
    for (let i = 1; i <= steps; i++) {
      await touch("touchMove", [[x, y + (dy * i) / steps]]);
      await wait(ms);
    }
    const midway = await sheet();
    await touch("touchEnd", []);
    await wait(600);
    return midway;
  };
  const matched = await page.evaluate(() => document.getElementById("finder").classList.contains("matched"));
  check(`${size}: matches in the sheet`, matched);
  if (!matched) return cdp.detach();
  await drag(1000, 20, 16);
  const start = await sheet();
  check(`${size}: dragged down, the sheet peeks`, start.at === "peek");
  // Slowly, so it settles by where it is: 200 px up from the peek is
  // nearer half than the peek, and 300 more nearer full.
  const midway = await drag(-200, 10, 40);
  const grown = midway.height - start.height;
  check(`${size}: the sheet follows the finger`, Math.abs(grown - 200) < 2, `${grown.toFixed(0)} px for 200`);
  check(`${size}: and settles at the nearest height`, (await sheet()).at === "half");
  await drag(-300, 10, 40);
  check(`${size}: and on up to full`, (await sheet()).at === "full");
  // A short quick flick up from the peek: nearer the peek by distance,
  // but its speed carries it on to half.
  await drag(1000, 20, 16);
  await drag(-80, 4, 20);
  check(`${size}: a flick carries it on`, (await sheet()).at === "half");
  const [x, y] = await handle();
  await touch("touchStart", [[x, y]]);
  await touch("touchEnd", []);
  await wait(600);
  check(`${size}: a tap on the handle steps it on`, (await sheet()).at === "full");
  await cdp.detach();
}

module.exports = { checkTouch, checkSheet };
