// Checks the app end to end against the offline stack (`mise run
// dev:offline`, whose fake WSF has Edmonds / Kingston's schedule and the
// boats' saved positions), at desk and phone sizes: no page errors, the
// finder finds a boat, and Kingston's timetable has sailings, arrival
// advice and a next sailing. Exits non-zero if anything's missing.
const { open, find, wait } = require("./app");

(async () => {
  let failed = 0;
  const check = (what, ok, detail = "") => {
    console.log(`${ok ? "ok  " : "FAIL"} ${what}${detail ? `: ${detail}` : ""}`);
    if (!ok) failed++;
  };
  for (const phone of [false, true]) {
    const size = phone ? "phone" : "desk";
    const { browser, page, errors } = await open({ phone });
    await find(page, "walla walla");
    const boats = await page.$$eval("#finder-list .hit .name", (n) => n.map((e) => e.textContent));
    check(`${size}: finds a boat`, boats.some((b) => /Walla Walla/.test(b)), boats.slice(0, 3).join(", "));
    await find(page, "kingston");
    const table = await page.evaluate(() => {
      const rows = [...document.querySelectorAll("#finder-list .hit.selected .timetable li:not(.head):not(.day)")];
      return {
        rows: rows.length,
        advice: rows.filter((r) => r.querySelector(".advice")?.textContent).length,
        next: rows.filter((r) => r.classList.contains("next")).length,
      };
    });
    check(`${size}: Kingston's timetable has sailings`, table.rows > 0, `${table.rows} rows`);
    check(`${size}: they have arrival advice`, table.advice > 0, `${table.advice} with advice`);
    check(`${size}: one is next`, table.next === 1 || table.rows === 0, `${table.next} next`);
    // Older sailings, where there are any: there and back.
    const older = await page.$("#finder-list .hit.selected .older-link");
    if (older) {
      await page.evaluate(() => document.querySelector("#finder-list .hit.selected .older-link").click());
      await wait(2500);
      const opened = await page.$$eval("#finder-page-rows li:not(.head):not(.day)", (r) => r.length);
      const shown = await page.$eval("#finder-page", (p) => !p.hidden);
      check(`${size}: older sailings open`, shown && opened > 0, `${opened} rows`);
      await page.click("#finder-page-back");
      await wait(300);
      check(`${size}: back to the details`, await page.$eval("#finder-page", (p) => p.hidden));
    }
    check(`${size}: no page errors`, errors.length === 0, errors.slice(0, 3).join(" | "));
    await browser.close();
  }
  process.exit(failed ? 1 : 0);
})().catch((e) => {
  console.error(e.message);
  process.exit(1);
});
