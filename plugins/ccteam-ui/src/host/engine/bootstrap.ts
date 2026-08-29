/**
 * Zero-configuration credentials for a LOCAL daemon, and the hard limits on
 * how far that convenience is allowed to reach.
 *
 * A plugin that just installed and started the engine sits under the same OS
 * user as that engine, so making the human paste a token they already own back
 * into a settings card is friction, not security. The bootstrap therefore
 * reads ONE file — `$CCTEAM_HOME/secrets/web-token`, the console token the
 * daemon writes for its own operator, in a 0700 directory under this uid.
 *
 * Four rules keep that from becoming a credential-scavenging habit:
 *
 *  1. ONE file. Never the rest of `secrets/`, never a vendor's config, never
 *     another home. The tool face's enrollment credential is ASKED OF THE
 *     DAEMON over REST — it is not lying around to be read, and reading it out
 *     of a file would be exactly the habit this rule exists to prevent.
 *  2. A user-entered token always wins. A pinned or card-entered value means
 *     the human named a daemon (LAN, remote, another identity); a file under
 *     this home describes a different daemon and must not silently override it.
 *  3. Read once per process, and only when nothing else supplied a token.
 *  4. Nothing here enters `process.env` and nothing reaches the browser (D19).
 *     The value is returned to the BFF's closure and used to build one
 *     `Authorization` header.
 */
import { createHash } from 'node:crypto'
import { readFileSync, statSync } from 'node:fs'
import { dirname, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { webTokenPath } from './locate.js'

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>

export interface TokenBootstrapOptions {
  /** Canonical `$CCTEAM_HOME`, read live (a settings edit can move it). */
  home: () => string
  /** Test seam only; production reads the real filesystem. */
  readTokenFile?: (path: string) => string | undefined
  logger?: { warn(message: string): void }
}

export interface TokenBootstrap {
  /** The bootstrapped token, or `undefined` when there is nothing to read. */
  read(): string | undefined
  /** How many times the file was actually opened — asserted by the tests. */
  readonly reads: number
  /** The path it would read, for honest reporting on the card. */
  path(): string
}

/**
 * Same-uid check: a token file owned by somebody else is not this user's
 * console token, and presenting it would be using another identity's
 * credential. Reported as absent rather than used.
 */
function readSameUidFile(path: string, logger?: { warn(message: string): void }): string | undefined {
  let stat: ReturnType<typeof statSync>
  try {
    stat = statSync(path)
  } catch {
    return undefined
  }
  const uid = typeof process.getuid === 'function' ? process.getuid() : undefined
  if (uid !== undefined && stat.uid !== uid) {
    logger?.warn(`ccteam-ui: ignoring ${path} — it belongs to uid ${stat.uid}, not ${uid}`)
    return undefined
  }
  try {
    const value = readFileSync(path, 'utf8').trim()
    return value === '' ? undefined : value
  } catch {
    return undefined
  }
}

export function createTokenBootstrap(options: TokenBootstrapOptions): TokenBootstrap {
  const readFile = options.readTokenFile ?? ((path: string) => readSameUidFile(path, options.logger))
  let attempted = false
  let cached: string | undefined
  let reads = 0
  return {
    read(): string | undefined {
      if (!attempted) {
        attempted = true
        reads += 1
        cached = readFile(webTokenPath(options.home()))
      }
      return cached
    },
    get reads(): number {
      return reads
    },
    path(): string {
      return webTokenPath(options.home())
    },
  }
}

export interface EnrollmentOptions {
  daemonUrl: () => string
  /** Authorization value the REST call carries; `undefined` = unauthenticated. */
  authorization: () => string | undefined
  /**
   * The credential this installation ALREADY holds (profile row or settings
   * card), if any. It is what separates "the daemon's record is mine" from
   * "the daemon has a record whose secret I do not have" — see
   * {@link createEnrollmentBootstrap}.
   */
  held?: () => string | undefined
  /** Persist the bearer, so the next boot finds it without asking at all. */
  persist?: (bearer: string) => Promise<void>
  fetchImpl?: FetchLike
  /** Slot name. Defaults to {@link defaultEnrollmentLabel}. */
  label?: string
  logger?: { warn(message: string): void }
}

export interface EnrollmentOutcome {
  ok: boolean
  /**
   * Set ONLY when the daemon created the credential — the one moment its
   * secret exists on the wire. An ensure that resolved to an existing record
   * answers without one, by construction.
   */
  bearer?: string
  /**
   * `ccteam-enroll:<id>:` — present on any success. Identifies the record
   * without carrying a secret byte, so a holder can check whether what it has
   * is what the daemon answers with.
   */
  bearerPrefix?: string
  error?: string
}

/**
 * The daemon route this asks: `POST /api/v1/enroll` with `ensure: true` and a
 * label, which is idempotent per (identity, label) — one credential per
 * installation, however many times DSH restarts
 * (`crates/ccteam-web/src/routes/enroll.rs` → `ccteam_core::enroll::ensure_in`,
 * the same function the machine credential goes through).
 *
 * This constant plus {@link requestEnrollment} are the only two places that
 * know the shape of that call.
 */
export const ENROLL_PATH = '/api/v1/enroll'

/**
 * The slot THIS installation's credential lives in.
 *
 * It has to name the installation, not just the plugin: two DSH profiles
 * running under one ccteam identity would otherwise share a slot, and the
 * second one — holding no secret for a record that already exists — would
 * rotate the first one's credential out from under it on every boot.
 *
 * A profile is a directory (`<dsh home>/profiles/<name>`) and this file is
 * loaded from inside it, so the name is right there in the module path; that
 * survives pnpm's `.pnpm/...` indirection, which sits below the profile dir.
 * When there is no `profiles/<name>` on the path (a hoisted install, a test),
 * the fallback is a digest of the directory this module was loaded from —
 * still unique per installation, still stable across boots, just not
 * human-readable.
 */
export function defaultEnrollmentLabel(modulePath = fileURLToPath(import.meta.url)): string {
  const parts = modulePath.split(sep)
  const at = parts.lastIndexOf('profiles')
  const named = at >= 0 ? parts[at + 1] : undefined
  if (named !== undefined && named !== '') return `dsh-plugin:${named}`
  const digest = createHash('sha256').update(dirname(modulePath)).digest('hex').slice(0, 12)
  return `dsh-plugin:path-${digest}`
}

/**
 * Ask the daemon for this installation's tool-face credential.
 *
 * `rotate` is for the caller that cannot use the record the daemon already
 * has (it never stored the secret, or the store was lost): the replacement is
 * minted and the old record revoked, which is the only way back — a stored
 * secret is never readable again.
 */
export async function requestEnrollment(
  options: EnrollmentOptions,
  rotate = false,
): Promise<EnrollmentOutcome> {
  const doFetch = options.fetchImpl ?? ((input: string, init?: RequestInit) => fetch(input, init))
  const base = options.daemonUrl().trim().replace(/\/+$/, '')
  const authorization = options.authorization()
  const label = options.label ?? defaultEnrollmentLabel()
  let response: Response
  try {
    response = await doFetch(`${base}${ENROLL_PATH}`, {
      method: 'POST',
      headers: {
        accept: 'application/json',
        'content-type': 'application/json',
        ...(authorization === undefined ? {} : { authorization }),
      },
      body: JSON.stringify(rotate ? { label, ensure: true, rotate: true } : { label, ensure: true }),
    })
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) }
  }
  if (!response.ok) {
    return { ok: false, error: `HTTP ${response.status}` }
  }
  let bearer: string | undefined
  let bearerPrefix: string | undefined
  try {
    const body = (await response.json()) as {
      bearer?: unknown
      credential?: { bearer_prefix?: unknown }
    }
    bearer = typeof body.bearer === 'string' && body.bearer.trim() !== '' ? body.bearer.trim() : undefined
    const prefix = body.credential?.bearer_prefix
    bearerPrefix = typeof prefix === 'string' && prefix.trim() !== '' ? prefix.trim() : undefined
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) }
  }
  if (bearer === undefined && bearerPrefix === undefined) {
    return { ok: false, error: 'the daemon named no credential' }
  }
  if (bearer !== undefined && options.persist !== undefined) {
    try {
      await options.persist(bearer)
    } catch (error) {
      // The credential is usable this run even if it could not be stored; say
      // so. The next boot re-ensures and lands on the same slot.
      options.logger?.warn(
        `ccteam-ui: received an enrollment credential but could not store it: ${error instanceof Error ? error.message : String(error)}`,
      )
    }
  }
  return { ok: true, bearer, bearerPrefix }
}

export interface EnrollmentBootstrap {
  /** The credential this process resolved, if it has one yet. Never blocks. */
  value(): string | undefined
  /** Resolve this installation's credential. Concurrent callers share one attempt. */
  ensure(): Promise<string | undefined>
  /** How many REST calls actually reached the daemon — asserted by the tests. */
  readonly requests: number
}

/**
 * One credential per installation — the daemon guarantees it, not this file.
 *
 * The endpoint is idempotent per (identity, label), so asking twice cannot
 * leave two records behind; what is memoized here is only the answer, because
 * a process has no reason to re-ask once it holds a usable credential.
 *
 * Two answers are possible and they mean different things:
 *
 *   - CREATED (`bearer`): the daemon minted it just now. Store it and use it.
 *   - EXISTS (`bearerPrefix` only): the slot is taken and the secret is not
 *     recoverable. If what this installation already holds has that prefix,
 *     it IS the credential and there is nothing to do; otherwise the store was
 *     lost, and `rotate` replaces the record rather than piling a second one
 *     next to it.
 */
export function createEnrollmentBootstrap(options: EnrollmentOptions): EnrollmentBootstrap {
  let resolved: string | undefined
  let inFlight: Promise<string | undefined> | undefined
  let requests = 0

  const ask = async (rotate: boolean): Promise<EnrollmentOutcome> => {
    requests += 1
    return await requestEnrollment(options, rotate)
  }

  const run = async (): Promise<string | undefined> => {
    const first = await ask(false)
    if (!first.ok) {
      options.logger?.warn(`ccteam-ui: could not enroll with the ccteam daemon: ${first.error}`)
      return undefined
    }
    if (first.bearer !== undefined) return first.bearer
    const held = options.held?.()
    if (held !== undefined && first.bearerPrefix !== undefined && held.startsWith(first.bearerPrefix)) {
      // Already holding exactly this record — asking again would be the only
      // way to turn an ensure into a second credential.
      return held
    }
    const rotated = await ask(true)
    if (!rotated.ok) {
      options.logger?.warn(
        `ccteam-ui: the ccteam daemon holds an enrollment credential for this profile that this plugin cannot use, and replacing it failed: ${rotated.error}`,
      )
      return undefined
    }
    return rotated.bearer
  }

  return {
    value: () => resolved,
    async ensure(): Promise<string | undefined> {
      if (resolved !== undefined) return resolved
      if (inFlight !== undefined) return await inFlight
      inFlight = run()
        .then(bearer => {
          resolved = bearer
          return resolved
        })
        .finally(() => {
          inFlight = undefined
        })
      return await inFlight
    },
    get requests(): number {
      return requests
    },
  }
}
