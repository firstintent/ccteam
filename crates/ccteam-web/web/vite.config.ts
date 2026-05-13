/// <reference types="vitest/config" />
// V0.3.2 F53 — vite config for the ccteam-web SPA bundle.
//
// `base: "/app/"` keeps bundled asset URLs aligned with the new
// rust-embed mount point (`/assets/spa/...` paths under `/app/...`).
// The dev proxy sends the API + SSE + screenshot + WS surface to the
// loopback port that `ccteam web` binds by default (see
// `crates/ccteam-web/src/lib.rs::ServeOpts::default`).

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const CCTEAM_WEB_PORT = "http://127.0.0.1:7331";

export default defineConfig({
  base: "/app/",
  plugins: [react(), tailwindcss()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    proxy: {
      "/api": { target: CCTEAM_WEB_PORT, changeOrigin: true },
      "/sse": { target: CCTEAM_WEB_PORT, changeOrigin: true, ws: false },
      "/screenshot": { target: CCTEAM_WEB_PORT, changeOrigin: true },
      "/ws": { target: CCTEAM_WEB_PORT, changeOrigin: true, ws: true },
    },
  },
  // Vitest unit tests live alongside source as `*.test.ts(x)`. Playwright
  // suites under `tests/` use the same `.spec.ts` extension Playwright
  // expects but aren't valid vitest tests, so we explicitly exclude them.
  test: {
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    exclude: ["tests/**", "node_modules/**", "dist/**"],
  },
});
