// Bakes the basemap (`mise run tiles:basemap`): opens the running app with
// `?bake` in headless Chromium, frames each tile its plan lists
// (src/basemap.rs, src/web/bake.rs) and saves what the GPU draws as WebP
// in tiles/basemap/, with the index.json the app reads. Tiles already
// there are kept (FORCE=1 bakes them again), so a stopped bake picks up
// where it left off; LEVELS=0,1,2 bakes just those levels (and indexes
// just them). Needs the app running with its elevation tiles
// (`mise run dev` or `dev:offline`).
const fs = require("fs");
const path = require("path");
const { open, wait } = require("./app");

const OUT = path.join(__dirname, "../../../tiles/basemap");
const ELEVATION = path.join(__dirname, "../../../tiles/elevation/index.json");
// As basemap::TILE_PX.
const TILE_PX = 512;
const QUALITY = 80;
const READY_TIMEOUT_MS = 60000;

let browser;
(async () => {
  const opened = await open({
    fragment: "?bake",
    settle: 3000,
    viewport: { width: TILE_PX, height: TILE_PX, deviceScaleFactor: 1 },
  });
  browser = opened.browser;
  const { page, errors } = opened;
  const only = process.env.LEVELS ? process.env.LEVELS.split(",").map(Number) : null;
  const plan = JSON.parse(await page.evaluate(() => window.ferryBake.plan())).filter(
    ([z]) => !only || only.includes(z),
  );
  const force = process.env.FORCE === "1";
  console.log(`${plan.length} tiles to bake into ${OUT}`);
  const levels = {};
  let baked = 0;
  let kept = 0;
  const started = Date.now();
  for (const [i, [z, x, y]] of plan.entries()) {
    (levels[z] ||= []).push([x, y]);
    const file = path.join(OUT, `${z}/${x}/${y}.webp`);
    if (!force && fs.existsSync(file)) {
      kept++;
      continue;
    }
    // Once more after a reload (the dev server's, after a rebuild).
    try {
      await bake(page, z, x, y, file);
    } catch (e) {
      console.log(`tile ${z}/${x}/${y}: ${e.message}; reloading and trying again`);
      await page.reload({ waitUntil: "load" });
      await page.waitForFunction(() => window.ferryBake, { timeout: READY_TIMEOUT_MS });
      await bake(page, z, x, y, file);
    }
    baked++;
    if (baked % 50 === 0) {
      const each = (Date.now() - started) / baked;
      const left = Math.round(((plan.length - i - 1) * each) / 60000);
      console.log(`${i + 1}/${plan.length} (about ${left} min left)`);
    }
  }
  const attribution = JSON.parse(fs.readFileSync(ELEVATION, "utf8")).attribution;
  const index = {
    levels,
    tile_size: TILE_PX,
    projection: "orthographic about the slice's center, tiles on the map (src/basemap.rs)",
    attribution,
  };
  fs.mkdirSync(OUT, { recursive: true });
  fs.writeFileSync(path.join(OUT, "index.json"), JSON.stringify(index));
  console.log(`baked ${baked}, kept ${kept}; wrote index.json`);
  if (errors.length) console.log(`page errors: ${errors.slice(0, 3).join(" | ")}`);
})()
  .catch((e) => {
    console.error(e.message);
    process.exitCode = 1;
  })
  .finally(() => browser?.close());

// Frames tile z/x/y, waits until it's drawn and shown, and saves it.
async function bake(page, z, x, y, file) {
  await page.evaluate((z, x, y) => window.ferryBake.show(z, x, y), z, x, y);
  const deadline = Date.now() + READY_TIMEOUT_MS;
  while (!(await page.evaluate(() => window.ferryBake.ready()))) {
    if (Date.now() > deadline) throw new Error("never finished drawing");
    await wait(50);
  }
  // A frame more, so what's drawn has reached the screen.
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  fs.mkdirSync(path.dirname(file), { recursive: true });
  await page.screenshot({
    path: file,
    type: "webp",
    quality: QUALITY,
    clip: { x: 0, y: 0, width: TILE_PX, height: TILE_PX },
  });
}
