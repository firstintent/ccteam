import { vendorChipClass } from "../lib/vendors";

/** Shared compact harness badge. Color normalization stays centralized in
 * `lib/vendors.ts`; callers only provide the session's wire vendor. */
export function VendorChip({ vendor }: { vendor: string }) {
  return (
    <span className={`${vendorChipClass(vendor)} vendor-chip`} data-vendor={vendor}>
      {vendor}
    </span>
  );
}
