import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  // V0.3.2 F59 keeps only ccteam-owned SPA smoke in the default
  // Playwright gate. The other specs in this directory are retained
  // as AoE fork references until each surface is explicitly promoted.
  // v0.9.0 W4 promotes the team-view smoke alongside it.
  testMatch: ["**/v032-spa.spec.ts", "**/v090-agents.spec.ts"],
  testIgnore: ["**/ensure-session-restart*"],
  timeout: 30000,
  retries: process.env.CI ? 1 : 0,
  use: {
    baseURL: "http://localhost:4173",
    headless: true,
    screenshot: "only-on-failure",
  },
  webServer: {
    // Direct binary path (not `npx vite preview`) — portable to hosts with no
    // `npx` on PATH (only `node`/`npm`; observed in this wave's sandbox) since
    // `vite` is already a devDependency, so `node_modules/.bin/vite` always
    // resolves post-install without depending on the npx shim at all.
    command: "node_modules/.bin/vite preview --port 4173",
    port: 4173,
    reuseExistingServer: !process.env.CI,
  },
  reporter: process.env.CI ? [["html", { open: "never" }], ["github"]] : "list",
  projects: [
    {
      name: "chromium",
      use: { browserName: "chromium" },
    },
  ],
});
