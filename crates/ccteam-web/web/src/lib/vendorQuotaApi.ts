// VENDOR-QUOTA-1 — client for `GET /api/v1/vendors/quota` (admin-only; the
// Ops & Hosts vendor rows render mini quota bars off it).
//
// Backend SoT: `crates/ccteam-web/src/routes/vendor_quota.rs` (thin HTTP +
// cache layer) over `ccteam-core/src/vendor_quota.rs` (normalized model +
// parsers). States: `available` (render up to two window bars + an optional
// plan badge) / `not_subscription` / `unavailable` (both render NOTHING —
// no error styling; a 403 for tenants is swallowed by the caller the same
// way). Vendors with no probe surface (opencode/pi/dsh) are simply absent
// from the list.

import { httpError } from "./httpError";

export type QuotaWindowKind = "five_hour" | "weekly" | "monthly";

/** One normalized quota window (`QuotaWindow`). */
export interface QuotaWindow {
  kind: QuotaWindowKind;
  /** 0–100. */
  used_percent: number;
  /** ISO timestamp; absent when the vendor does not report one. */
  resets_at?: string | null;
}

export type QuotaState = "available" | "not_subscription" | "unavailable";

/** One vendor's quota row (`VendorQuota`). */
export interface VendorQuota {
  vendor: string;
  state: QuotaState;
  plan?: string | null;
  windows?: QuotaWindow[];
}

/** `GET /api/v1/vendors/quota` — every probe-bearing vendor's quota row
 *  (registry-ordered). Throws on 401 (`UNAUTHENTICATED`) / non-2xx. */
export async function getVendorQuotas(): Promise<VendorQuota[]> {
  let res: Response;
  try {
    res = await fetch("/api/v1/vendors/quota", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  const body = (await res.json()) as { quotas?: VendorQuota[] };
  return body.quotas ?? [];
}
