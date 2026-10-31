// Opens the app in headless Chromium with WebGPU, for shot.js and
// smoke.js. CHROMIUM names the browser (default: chromium on the PATH or
// in /usr/bin); APP_URL the app (default http://127.0.0.1:8080/, where
// `mise run dev:offline` serves it).
const fs = require("fs");
const path = require("path");
const puppeteer = require("puppeteer-core");

const URL = process.env.APP_URL || "http://127.0.0.1:8080/";
const DESK = { width: 1400, height: 900 };
const PHONE = { width: 390, height: 844, isMobile: true, hasTouch: true };

function chromium() {
  if (process.env.CHROMIUM) return process.env.CHROMIUM;
  const dirs = (process.env.PATH || "").split(path.delimiter).concat("/usr/bin");
  for (const dir of dirs) {
    for (const name of ["chromium", "chromium-browser", "google-chrome"]) {
      const file = path.join(dir, name);
      if (fs.existsSync(file)) return file;
    }
  }
  throw new Error("no Chromium found: set CHROMIUM");
}

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// The app at `fragment` (e.g. "#at=47.6,-122.4,20000"), once it has had
// time to fetch and draw. Errors from the page are collected in `errors`,
// but for missing files (tiles not built, say).
async function open({ phone = false, fragment = "", settle = 7000 } = {}) {
  const browser = await puppeteer.launch({
    executablePath: chromium(),
    headless: "new",
    args: [
      "--no-sandbox",
      "--enable-unsafe-webgpu",
      "--enable-features=Vulkan",
      "--use-angle=vulkan",
      "--ignore-gpu-blocklist",
    ],
  });
  const page = await browser.newPage();
  const errors = [];
  page.on("console", (m) => {
    if (m.type() === "error" && !/404|Failed to load resource/.test(m.text())) errors.push(m.text());
  });
  page.on("pageerror", (e) => errors.push(e.message));
  await page.setViewport(phone ? PHONE : DESK);
  await page.goto(URL + fragment, { waitUntil: "load" });
  await wait(settle);
  return { browser, page, errors };
}

// Types `text` into the finder and waits for its details.
async function find(page, text) {
  await page.click("#finder-input", { clickCount: 3 });
  await page.keyboard.type(text, { delay: 30 });
  await wait(3000);
}

module.exports = { open, find, wait };
