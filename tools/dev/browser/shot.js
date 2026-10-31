// A screenshot of the app: node shot.js OUT.png [--phone] [--at LAT,LON,ALT_M]
// [--find TEXT] [--wait MS]. Prints the page's errors, if any.
const { open, find, wait } = require("./app");

(async () => {
  const args = process.argv.slice(2);
  const opt = (name) => {
    const i = args.indexOf(name);
    return i >= 0 ? args.splice(i, 2)[1] : undefined;
  };
  const phone = args.includes("--phone") && args.splice(args.indexOf("--phone"), 1);
  const at = opt("--at");
  const text = opt("--find");
  const extra = +(opt("--wait") || 0);
  const [out] = args;
  if (!out) {
    console.error("usage: node shot.js OUT.png [--phone] [--at LAT,LON,ALT_M] [--find TEXT] [--wait MS]");
    process.exit(2);
  }
  const { browser, page, errors } = await open({ phone: !!phone, fragment: at ? `#at=${at}` : "" });
  if (text) await find(page, text);
  await wait(extra);
  await page.screenshot({ path: out });
  await browser.close();
  for (const e of errors) console.log("page error:", e);
  console.log(out);
})().catch((e) => {
  console.error(e.message);
  process.exit(1);
});
