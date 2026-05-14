import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  // V0.3.2 F59 keeps only ccteam-owned SPA smoke in the default
  // Playwright gate. The other specs in this directory are retained
  // as AoE fork references until each surface is explicitly promoted.
  testMatch: ["**/v032-spa.spec.ts"],
  testIgnore: ["**/ensure-session-restart*"],
  timeout: 30000,
  retries: process.env.CI ? 1 : 0,
  use: {
    baseURL: "http://localhost:4173",
    headless: true,
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "npx vite preview --port 4173",
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
