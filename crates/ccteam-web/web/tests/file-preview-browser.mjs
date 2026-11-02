// Invoked by the tempdir-backed Rust integration fixture; no live daemon.
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";

const [base, path] = process.argv.slice(2);
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage();
  await page.goto(`${base}/app/`);
  await page.evaluate(() => { localStorage.setItem("preview-parent-secret", "must-stay-private"); });
  await page.setContent(`<iframe title="preview" sandbox="allow-scripts" src="${base}${path}"></iframe>`);
  const frame = page.frameLocator("iframe");
  await frame.locator("body[data-loaded=yes]").waitFor();
  assert.equal(await frame.locator("img").evaluate((el) => el.complete && el.naturalWidth === 3), true);
  assert.equal(await frame.locator("h1").evaluate((el) => getComputedStyle(el).color), "rgb(255, 0, 0)");
  assert.equal(await frame.locator("body").evaluate(() => {
    try { return parent.document.body ? "leaked" : "bad"; } catch { return "isolated"; }
  }), "isolated");
  assert.equal(await frame.locator("body").evaluate(() => {
    try { return localStorage.getItem("preview-parent-secret"); } catch { return "isolated"; }
  }), "isolated");
  const direct = await browser.newPage();
  await direct.goto(`${base}${path}`);
  await direct.locator("body[data-loaded=yes]").waitFor();
  assert.equal(await direct.evaluate(() => {
    try { document.cookie = "preview=unsafe"; return "leaked"; } catch { return "isolated"; }
  }), "isolated");
  console.log("Chromium: relative module + stylesheet, iframe and standalone isolation passed");
} finally {
  await browser.close();
}
