// Render one post's slides.html into the JPGs Relay posts.
//
//   node slides/render.mjs <post folder>
//
// Every element with class "slide" becomes NN.jpg at 1080x1920, and
// sheet.jpg puts them side by side at about phone-feed size for the
// reject-list check. Assets in slides.html resolve relative to the post
// folder, so shared logos are referenced relative to it (the example uses ../assets/<file>).
import { createRequire } from "node:module";
import { readdirSync, unlinkSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

// require() so a globally installed Playwright (NODE_PATH) works too.
const { chromium } = createRequire(import.meta.url)("playwright");

const dir = resolve(process.argv[2] || "");
if (!process.argv[2]) {
  console.error("usage: node slides/render.mjs <post folder>");
  process.exit(2);
}
for (const f of readdirSync(dir)) if (/^\d\d\.jpg$|^sheet\.jpg$/.test(f)) unlinkSync(join(dir, f));

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1080, height: 1920 } });
await page.goto(pathToFileURL(join(dir, "slides.html")).href);
await page.evaluate(() => document.fonts.ready);
const slides = await page.$$(".slide");
if (slides.length < 3 || slides.length > 10) throw new Error(`${slides.length} slides; use 3 to 10`);
const shots = [];
for (const [i, el] of slides.entries()) {
  const box = await el.boundingBox();
  if (Math.round(box.width) !== 1080 || Math.round(box.height) !== 1920) {
    throw new Error(`slide ${i + 1} is ${box.width}x${box.height}; must be 1080x1920`);
  }
  const buf = await el.screenshot({ type: "jpeg", quality: 88 });
  writeFileSync(join(dir, `${String(i + 1).padStart(2, "0")}.jpg`), buf);
  shots.push(buf.toString("base64"));
}
// Contact sheet: slides at 360px wide, the size they read at in the feed.
const sheet = await browser.newPage({ viewport: { width: 380 * shots.length, height: 660 } });
await sheet.setContent(
  `<body style="margin:0;background:#888;display:flex;gap:20px;padding:10px">${shots
    .map((s) => `<img src="data:image/jpeg;base64,${s}" style="width:360px;height:640px">`)
    .join("")}</body>`,
);
writeFileSync(join(dir, "sheet.jpg"), await sheet.screenshot({ type: "jpeg", quality: 80, fullPage: true }));
await browser.close();
console.log(`${dir}: ${shots.length} slides`);
