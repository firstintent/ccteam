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
import { readFileSync, statSync } from 'node:fs'
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
  /** Persist the minted bearer so the next boot finds it instead of minting again. */
  persist?: (bearer: string) => Promise<void>
  fetchImpl?: FetchLike
  label?: string
  logger?: { warn(message: string): void }
}

export interface EnrollmentOutcome {
  ok: boolean
  bearer?: string
  error?: string
}

/**
 * Ask the daemon for this DSH process's tool-face credential.
 *
 * `POST /api/v1/enroll` MINTS (crates/ccteam-web/src/routes/enroll.rs), so
 * "idempotent ensure" is achieved by calling it at most once and storing the
 * result in the plugin's own settings — the same place a human would have
 * pasted it, visible and revocable from both ends. A caller that already has a
 * credential must not reach this function at all.
 */
export async function requestEnrollment(options: EnrollmentOptions): Promise<EnrollmentOutcome> {
  const doFetch = options.fetchImpl ?? ((input: string, init?: RequestInit) => fetch(input, init))
  const base = options.daemonUrl().trim().replace(/\/+$/, '')
  const authorization = options.authorization()
  let response: Response
  try {
    response = await doFetch(`${base}/api/v1/enroll`, {
      method: 'POST',
      headers: {
        accept: 'application/json',
        'content-type': 'application/json',
        ...(authorization === undefined ? {} : { authorization }),
      },
      body: JSON.stringify({ label: options.label ?? 'dsh plugin' }),
    })
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) }
  }
  if (!response.ok) {
    return { ok: false, error: `HTTP ${response.status}` }
  }
  let bearer: string | undefined
  try {
    const body = (await response.json()) as { bearer?: unknown }
    bearer = typeof body.bearer === 'string' && body.bearer.trim() !== '' ? body.bearer.trim() : undefined
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) }
  }
  if (bearer === undefined) return { ok: false, error: 'the daemon returned no bearer' }
  if (options.persist !== undefined) {
    try {
      await options.persist(bearer)
    } catch (error) {
      // The credential is usable this run even if it could not be stored; say so.
      options.logger?.warn(
        `ccteam-ui: minted an enrollment credential but could not store it: ${error instanceof Error ? error.message : String(error)}`,
      )
    }
  }
  return { ok: true, bearer }
}

export interface EnrollmentBootstrap {
  /** The credential this process minted, if it has one yet. Never blocks. */
  value(): string | undefined
  /** Mint once, at most. Concurrent callers share the one attempt. */
  ensure(): Promise<string | undefined>
  /** How many mint calls actually reached the daemon — asserted by the tests. */
  readonly mints: number
}

/**
 * One credential per DSH process, minted at most once.
 *
 * `POST /api/v1/enroll` is a MINT, so calling it on every boot would leave a
 * new record behind every time DSH restarts. Minting once and storing the
 * result through `persist` is what makes it an ensure in practice: the next
 * boot reads it back out of the settings card, sees a credential, and never
 * calls this at all.
 */
export function createEnrollmentBootstrap(options: EnrollmentOptions): EnrollmentBootstrap {
  let minted: string | undefined
  let inFlight: Promise<string | undefined> | undefined
  let mints = 0
  return {
    value: () => minted,
    async ensure(): Promise<string | undefined> {
      if (minted !== undefined) return minted
      if (inFlight !== undefined) return await inFlight
      mints += 1
      inFlight = requestEnrollment(options)
        .then(outcome => {
          if (outcome.ok) {
            minted = outcome.bearer
          } else {
            options.logger?.warn(`ccteam-ui: could not enroll with the ccteam daemon: ${outcome.error}`)
          }
          return minted
        })
        .finally(() => {
          inFlight = undefined
        })
      return await inFlight
    },
    get mints(): number {
      return mints
    },
  }
}
