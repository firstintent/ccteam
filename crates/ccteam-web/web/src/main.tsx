// V0.3.2 F53 + F59 — SPA entry point.
//
// `BrowserRouter basename="/app"` mounts everything under `/app/...`.
// As of F59 the legacy axum HTML routes (`/`, `/project/{slug}`,
// `/session/{slug}/{sid}`) 301-redirect into the SPA path, so the
// SPA is the only live UI surface.
//
// The AoE original registered a service worker (`/sw.js`) for push
// notifications + an `?session=` legacy URL rewrite. ccteam-web ships
// neither (push deferred to V0.4 per V0.3.2 PRD §5), so both have
// been dropped along with `lib/legacySessionRedirect.ts`.

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
// Imported first so the URL `?token=` capture runs before any fetch.
import "./lib/token";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
import { ToastBusBridge, ToastProvider } from "./components/Toasts";
import { installFetchErrorToasts } from "./lib/fetchInterceptor";
import "./index.css";

installFetchErrorToasts();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ToastProvider>
      <ToastBusBridge />
      <BrowserRouter basename="/app">
        <App />
      </BrowserRouter>
    </ToastProvider>
  </StrictMode>,
);
