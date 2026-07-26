// Best-effort "latest version" for host agent rows.
//
// npm registry allows browser CORS (`Access-Control-Allow-Origin: *`), so the
// Status · 运维总览 page can show current → latest without a new daemon
// endpoint. Cache is in-memory + TTL so re-probes don't hammer the registry.
// Unknown / non-npm vendors (e.g. kimi binary installs) simply stay blank.

/** npm package name for a known agent vendor, or null when we have no source. */
export function npmPackageForVendor(vendor: string): string | null {
  switch (vendor) {
    case "claude":
      return "@anthropic-ai/claude-code";
    case "codex":
      return "@openai/codex";
    case "grok":
      return "@xai-official/grok";
    case "opencode":
      return "opencode-ai";
    default:
      return null;
  }
}

/** Extract the first dotted numeric version from a probe string
 *  (`"claude 2.1.220"` → `"2.1.220"`, `"codex-cli 0.144.1"` → `"0.144.1"`). */
export function extractVersion(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const m = raw.trim().match(/(\d+\.\d+(?:\.\d+)?(?:[-+][0-9A-Za-z.]+)?)/);
  return m?.[1] ?? null;
}

function normalizeParts(v: string): number[] {
  return v
    .trim()
    .replace(/^v/i, "")
    .split(".")
    .map((part) => {
      const digits = part.match(/^\d+/);
      return digits ? Number(digits[0]) : 0;
    });
}

/** True when `latest` is strictly newer than `current` (both free-form). */
export function isOutdated(
  current: string | null | undefined,
  latest: string | null | undefined,
): boolean {
  const c = extractVersion(current ?? null);
  const l = extractVersion(latest ?? null);
  if (!c || !l) return false;
  const ca = normalizeParts(c);
  const la = normalizeParts(l);
  const n = Math.max(ca.length, la.length);
  for (let i = 0; i < n; i++) {
    const x = la[i] ?? 0;
    const y = ca[i] ?? 0;
    if (x !== y) return x > y;
  }
  return false;
}

const TTL_MS = 6 * 60 * 60 * 1000; // 6h — same spirit as version_check's lazy gate
type CacheEntry = { at: number; version: string | null };
const cache = new Map<string, CacheEntry>();
const inflight = new Map<string, Promise<string | null>>();

/** Fetch one package's latest version from the npm registry. Cached + deduped. */
export async function fetchNpmLatest(pkg: string): Promise<string | null> {
  const hit = cache.get(pkg);
  if (hit && Date.now() - hit.at < TTL_MS) return hit.version;

  const pending = inflight.get(pkg);
  if (pending) return pending;

  // Scoped packages need `@scope%2Fname` in the registry path.
  const path = pkg.startsWith("@") ? pkg.replace("/", "%2F") : pkg;
  const url = `https://registry.npmjs.org/${path}/latest`;

  const job = (async () => {
    try {
      const res = await fetch(url, {
        headers: { Accept: "application/json" },
        // no credentials — public registry
      });
      if (!res.ok) {
        cache.set(pkg, { at: Date.now(), version: null });
        return null;
      }
      const body = (await res.json()) as { version?: string };
      const version = typeof body.version === "string" ? body.version : null;
      cache.set(pkg, { at: Date.now(), version });
      return version;
    } catch {
      cache.set(pkg, { at: Date.now(), version: null });
      return null;
    } finally {
      inflight.delete(pkg);
    }
  })();

  inflight.set(pkg, job);
  return job;
}

/** Resolve latest versions for a set of vendor tokens. Missing/unknown → omitted. */
export async function fetchVendorLatests(
  vendors: string[],
): Promise<Record<string, string>> {
  const unique = Array.from(new Set(vendors));
  const pairs = await Promise.all(
    unique.map(async (vendor) => {
      const pkg = npmPackageForVendor(vendor);
      if (!pkg) return [vendor, null] as const;
      const latest = await fetchNpmLatest(pkg);
      return [vendor, latest] as const;
    }),
  );
  const out: Record<string, string> = {};
  for (const [vendor, latest] of pairs) {
    if (latest) out[vendor] = latest;
  }
  return out;
}

/** Test seam: drop the in-memory cache between unit tests. */
export function __resetVendorLatestCacheForTests(): void {
  cache.clear();
  inflight.clear();
}
