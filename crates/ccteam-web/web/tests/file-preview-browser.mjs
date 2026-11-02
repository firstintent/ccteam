// Invoked by the tempdir-backed Rust integration fixture; no live daemon.
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";

const [base, path, markdownPath] = process.argv.slice(2);
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
  if (markdownPath) {
    await page.setContent(`<iframe title="markdown" sandbox="allow-scripts" src="${base}${markdownPath}"></iframe>`);
    const markdown = page.frameLocator("iframe");
    await markdown.getByRole("heading", { name: "Markdown document", exact: true }).waitFor();
    assert.equal(await markdown.getByRole("cell", { name: "world", exact: true }).count(), 1);
    assert.equal(await markdown.getByRole("checkbox").isChecked(), true);
    assert.equal(await markdown.locator("pre code.language-rust").count(), 1);
    await markdown.locator("img").evaluate((el) => el.decode());
    assert.equal(await markdown.locator("img").evaluate((el) => el.naturalWidth), 3);
    assert.equal(await markdown.locator("script").count(), 0);
    await markdown.getByRole("link", { name: "Section", exact: true }).click();
    await markdown.getByRole("link", { name: "Next", exact: true }).click();
    await markdown.getByRole("heading", { name: "Next document", exact: true }).waitFor();
    await direct.goto(`${base}${markdownPath}`);
    await direct.getByRole("heading", { name: "Markdown document", exact: true }).waitFor();
    assert.equal(await direct.evaluate(() => { try { return localStorage.length; } catch { return "isolated"; } }), "isolated");
    console.log("Chromium: Markdown tables/tasks/code/images/relative links and standalone rendering passed");
  }
} finally {
  await browser.close();
}
