// V0.3.2 F53 — SPA entry point.
//
// `BrowserRouter basename="/app"` mounts everything under `/app/...`
// so the legacy AoE root (`/`) plus ccteam's existing axum HTML routes
// (`/`, `/project/{slug}`, `/session/{sid}`) can stay live during the
// V0.3.2 dual-track period. F58/F59 swap the SPA in as the primary
// surface once feature parity lands.
//
// The AoE original registered a service worker (`/sw.js`) for push
// notifications. ccteam-web ships no such asset and the push surface
// is out of scope for V0.3.2 — registration is dropped.

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
// Imported first so the URL `?token=` capture runs before any fetch.
import "./lib/token";
import "./lib/legacySessionRedirect";
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
