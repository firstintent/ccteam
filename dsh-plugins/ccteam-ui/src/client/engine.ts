/**
 * The engine as the workbench reasons about it: pure selectors over the
 * host's `EngineStatus` (state dot, label key, first-run gating, version
 * relation, action enablement, polling cadence), the action runners, and the
 * one poller that keeps the store's engine slice fresh while a seat is
 * watching it. Nothing here renders; the components read the store.
 *
 * Every verdict is DERIVED from the host's facts, never asked of it: what the
 * payload lacks (a color, a first-run decision, whether the engine is older
 * than the plugin) is computed here, so the host half stays facts-only.
 */
import type {
  EngineActionResult,
  EngineState,
  EngineStatus,
  EngineUnsupervisedReason,
  ProjectInfo,
} from '../shared/contract.js'
import type { ApiClient } from './api.js'
import type { CcteamLocaleKey } from './locales.js'
import type { Action, ConsoleStore, EngineAction, EngineSlice } from './store.js'

type Dispatch = (action: Action) => void

// ------------------------------------------------------------------ verdicts

/** StateDot's four colors plus a neutral grey ("installed, not running"). */
export type EngineDotState = 'done' | 'ongoing' | 'warning' | 'error' | 'neutral'

/**
 * Dot color per engine state: healthy green for running/attached, the
 * working ring while starting/installing, grey for stopped, amber for a
 * missing engine, red for unsupported and both mismatches.
 * @param status - the host's status (null before the first poll).
 * @returns the dot state.
 */
export function engineDot(status: EngineStatus | null): EngineDotState {
  if (status === null) return 'neutral'
  switch (status.state) {
    case 'running':
    case 'attached':
      return 'done'
    case 'starting':
    case 'installing':
      return 'ongoing'
    case 'stopped':
      return 'neutral'
    case 'missing':
      return 'warning'
    case 'unsupported':
    case 'mismatch':
      return 'error'
  }
}

const STATE_KEY: Record<Exclude<EngineState, 'mismatch'>, CcteamLocaleKey> = {
  unsupported: 'engine.state.unsupported',
  missing: 'engine.state.missing',
  installing: 'engine.state.installing',
  stopped: 'engine.state.stopped',
  starting: 'engine.state.starting',
  running: 'engine.state.running',
  attached: 'engine.state.attached',
}

/**
 * Dictionary key of the state label (mismatch splits by what mismatched).
 * @param status - the host's status (null before the first poll).
 * @returns the locale key.
 */
export function engineStateKey(status: EngineStatus | null): CcteamLocaleKey {
  if (status === null) return 'engine.state.unknown'
  if (status.state === 'mismatch') {
    return status.mismatch === 'home' ? 'engine.state.mismatchHome' : 'engine.state.mismatchVersion'
  }
  return STATE_KEY[status.state]
}

const INERT_KEY: Record<EngineUnsupervisedReason, CcteamLocaleKey> = {
  managed: 'engine.inert.managed',
  pinned: 'engine.inert.pinned',
  remote: 'engine.inert.remote',
  unsupported: 'engine.inert.unsupported',
}

/**
 * The one calm sentence for an inert supervisor (managed / pinned / remote /
 * unsupported), or null while the plugin supervises the engine itself.
 * @param status - the host's status.
 * @returns the locale key, or null.
 */
export function engineInertKey(status: EngineStatus | null): CcteamLocaleKey | null {
  if (status === null || status.supervised) return null
  return INERT_KEY[status.unsupervisedReason ?? 'pinned']
}

// ------------------------------------------------------------------ versions

interface ParsedVersion {
  core: [number, number, number]
  pre: string[]
}

/**
 * Parse `major.minor.patch[-pre][+build]` (a leading `v` tolerated).
 * @param raw - version text.
 * @returns the parts, or undefined when it is not a version.
 */
export function parseVersion(raw: string): ParsedVersion | undefined {
  const match = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(raw.trim())
  if (match === null) return undefined
  return {
    core: [Number(match[1]), Number(match[2]), Number(match[3])],
    pre: match[4] === undefined ? [] : match[4].split('.'),
  }
}

/**
 * Semver order: numeric triplet, then pre-release (a pre-release sorts below
 * the release it precedes; identifiers compare numerically when both are
 * numbers, else lexically).
 * @param a - left version.
 * @param b - right version.
 * @returns -1 / 0 / 1, or undefined when either side is not a version.
 */
export function compareVersions(a: string, b: string): number | undefined {
  const left = parseVersion(a)
  const right = parseVersion(b)
  if (left === undefined || right === undefined) return undefined
  for (let i = 0; i < 3; i += 1) {
    const delta = left.core[i]! - right.core[i]!
    if (delta !== 0) return delta < 0 ? -1 : 1
  }
  if (left.pre.length === 0 || right.pre.length === 0) {
    if (left.pre.length === right.pre.length) return 0
    return left.pre.length === 0 ? 1 : -1
  }
  const length = Math.max(left.pre.length, right.pre.length)
  for (let i = 0; i < length; i += 1) {
    const l = left.pre[i]
    const r = right.pre[i]
    if (l === undefined) return -1
    if (r === undefined) return 1
    const ln = /^\d+$/.test(l) ? Number(l) : undefined
    const rn = /^\d+$/.test(r) ? Number(r) : undefined
    if (ln !== undefined && rn !== undefined) {
      if (ln !== rn) return ln < rn ? -1 : 1
      continue
    }
    if (ln !== undefined) return -1
    if (rn !== undefined) return 1
    if (l !== r) return l < r ? -1 : 1
  }
  return 0
}

/** How the engine's version relates to the one this plugin was published against. */
export type VersionRelation =
  | { kind: 'match' }
  | { kind: 'unknown' }
  | { kind: 'engine-older'; engine: string; pinned: string }
  | { kind: 'plugin-older'; plugin: string; engine: string }

/**
 * Compare the effective engine version (the running daemon's when one
 * answers, else the installed binary's) with the plugin's pinned version.
 * Pre-1.0 the plugin and engine move in lockstep (PRD D5), so "plugin vX" in
 * the banner IS the pinned engine version.
 * @param engineVersion - installed binary version.
 * @param pinnedVersion - package.json `ccteam.engine`.
 * @param daemonVersion - the running daemon's version, when reachable.
 * @returns the relation.
 */
export function versionRelation(
  engineVersion: string | undefined,
  pinnedVersion: string,
  daemonVersion?: string,
): VersionRelation {
  const engine = daemonVersion ?? engineVersion
  if (engine === undefined || engine === '' || pinnedVersion === '') return { kind: 'unknown' }
  const order = compareVersions(engine, pinnedVersion)
  if (order === undefined) return { kind: 'unknown' }
  if (order < 0) return { kind: 'engine-older', engine, pinned: pinnedVersion }
  if (order > 0) return { kind: 'plugin-older', plugin: pinnedVersion, engine }
  return { kind: 'match' }
}

/**
 * The version relation of a status (unknown before the first poll).
 * @param status - the host's status.
 * @returns the relation.
 */
export function relationOf(status: EngineStatus | null): VersionRelation {
  if (status === null) return { kind: 'unknown' }
  return versionRelation(status.binaryVersion, status.pinnedVersion, status.runningVersion)
}

// ----------------------------------------------------------------- gating

/** What the workbench shows first. */
export type FirstRunState = 'engine-not-ready' | 'no-project' | 'ready'

/**
 * First-run gate. An engine that answers (running / attached, or a version
 * mismatch — still a live daemon) is ready; every other state gates the
 * workbench on the engine panel. With a ready engine and a loaded, empty
 * project catalog the "add a workspace" panel gates instead. An unknown
 * engine (before the first poll) is treated as ready so the workbench's own
 * connection screens apply.
 * @param engine - the host's status.
 * @param projects - the project catalog (null while unloaded).
 * @returns the gate.
 */
export function firstRunState(engine: EngineStatus | null, projects: ProjectInfo[] | null): FirstRunState {
  if (engine === null) return 'ready'
  const live = engine.state === 'running' || engine.state === 'attached'
    || (engine.state === 'mismatch' && engine.mismatch === 'version')
  if (!live) return 'engine-not-ready'
  if (projects !== null && projects.length === 0) return 'no-project'
  return 'ready'
}

// ---------------------------------------------------------------- actions

export interface EngineEnablement {
  start: boolean
  stop: boolean
  restart: boolean
  update: boolean
}

const NOTHING: EngineEnablement = { start: false, stop: false, restart: false, update: false }

/**
 * Which explicit actions the card offers right now: start when stopped or
 * missing-but-installable, stop/restart when the daemon is live, update only
 * on a version mismatch where the engine is the older side. Nothing while an
 * action is in flight or the supervisor is inert.
 * @param status - the host's status.
 * @param pending - the action in flight.
 * @returns the enablement.
 */
export function engineEnablement(status: EngineStatus | null, pending: EngineAction | null): EngineEnablement {
  if (status === null || !status.supervised || pending !== null) return NOTHING
  const live = status.state === 'running' || status.state === 'attached'
  return {
    start: status.state === 'stopped' || status.state === 'missing',
    stop: live,
    restart: live,
    update: status.state === 'mismatch' && status.mismatch === 'version' && relationOf(status).kind === 'engine-older',
  }
}

/**
 * Stop and restart take a running daemon down (ccteam web + the IM gateway
 * with it), so they ask first; start and update do not.
 * @param action - the action.
 * @returns whether the Modal precedes it.
 */
export function needsConfirmation(action: EngineAction): boolean {
  return action === 'stop' || action === 'restart'
}

/**
 * The card's action entry point: confirmable actions open the Modal (no
 * call yet), the others run immediately.
 * @param dispatch - store write path.
 * @param api - BFF client.
 * @param action - the action.
 * @returns settled when the immediate action finished (resolves at once for a confirm).
 */
export async function requestEngineAction(dispatch: Dispatch, api: ApiClient, action: EngineAction): Promise<void> {
  if (needsConfirmation(action)) {
    dispatch({ type: 'engine_confirm', action })
    return
  }
  await runEngineAction(dispatch, api, action)
}

/**
 * The Modal's OK: run whatever is awaiting confirmation (nothing otherwise).
 * @param dispatch - store write path.
 * @param api - BFF client.
 * @param engine - the engine slice.
 * @returns settled when the action finished.
 */
export async function confirmEngineAction(dispatch: Dispatch, api: ApiClient, engine: Pick<EngineSlice, 'confirm'>): Promise<void> {
  if (engine.confirm === null) return
  await runEngineAction(dispatch, api, engine.confirm)
}

/**
 * Refresh the engine status into the store.
 * @param dispatch - store write path.
 * @param api - BFF client.
 * @returns the status, or null when the host could not be reached.
 */
export async function refreshEngine(dispatch: Dispatch, api: ApiClient): Promise<EngineStatus | null> {
  try {
    const status = await api.call('engine.status', {})
    dispatch({ type: 'engine_loaded', status })
    return status
  } catch (error) {
    dispatch({ type: 'engine_failed', message: describe(error) })
    return null
  }
}

/**
 * Run one action: optimistic pending state, then the host's result (its
 * status and, on refusal, its error text) or the transport failure.
 * @param dispatch - store write path.
 * @param api - BFF client.
 * @param action - the action.
 * @returns the host's result, or null when the host could not be reached.
 */
export async function runEngineAction(dispatch: Dispatch, api: ApiClient, action: EngineAction): Promise<EngineActionResult | null> {
  dispatch({ type: 'engine_action_started', action })
  try {
    const result = await api.call(`engine.${action}`, {})
    dispatch({ type: 'engine_action_settled', action, result })
    return result
  } catch (error) {
    dispatch({ type: 'engine_action_failed', action, message: describe(error) })
    return null
  }
}

// ---------------------------------------------------------------- polling

/** Cadence while the engine is changing state (starting / installing / an action in flight). */
export const ENGINE_POLL_TRANSITION_MS = 1_000
/** Cadence while a seat shows a settled engine. */
export const ENGINE_POLL_IDLE_MS = 5_000

/**
 * Poll cadence: 1s through a transition, 5s otherwise.
 * @param engine - the engine slice (status + pending).
 * @returns the delay in ms.
 */
export function enginePollMs(engine: Pick<EngineSlice, 'status' | 'pending'>): number {
  if (engine.pending !== null) return ENGINE_POLL_TRANSITION_MS
  const state = engine.status?.state
  return state === 'starting' || state === 'installing' ? ENGINE_POLL_TRANSITION_MS : ENGINE_POLL_IDLE_MS
}

/** Injectable timers (tests drive them by hand). */
export interface PollScheduler {
  setTimeout(callback: () => void, ms: number): unknown
  clearTimeout(handle: unknown): void
}

export interface PollerOptions {
  scheduler?: PollScheduler
  /** The daemon became reachable / unreachable between two polls. */
  onReachableChange?: (reachable: boolean) => void
}

/**
 * The one engine poller: refreshes at once when the first seat starts
 * watching, then at {@link enginePollMs} cadence (a transition shortens a
 * pending wait), and stops the moment nothing watches. Never two refreshes
 * in flight.
 * @param store - the workbench store.
 * @param api - BFF client.
 * @param options - timers and the reachability callback.
 * @returns the disposer.
 */
export function startEnginePoller(store: ConsoleStore, api: ApiClient, options: PollerOptions = {}): () => void {
  const scheduler: PollScheduler = options.scheduler ?? {
    setTimeout: (callback, ms) => setTimeout(callback, ms),
    clearTimeout: handle => clearTimeout(handle as ReturnType<typeof setTimeout>),
  }
  let timer: unknown = null
  let scheduledMs = 0
  let inFlight = false
  let stopped = false
  let lastReachable: boolean | undefined

  const watched = (): boolean => store.getSnapshot().engine.watchers > 0
  const clear = (): void => {
    if (timer === null) return
    scheduler.clearTimeout(timer)
    timer = null
  }
  const schedule = (): void => {
    if (stopped || inFlight || !watched()) return
    const ms = enginePollMs(store.getSnapshot().engine)
    if (timer !== null) {
      if (ms >= scheduledMs) return
      clear()
    }
    scheduledMs = ms
    timer = scheduler.setTimeout(() => {
      timer = null
      void tick()
    }, ms)
  }
  const tick = async (): Promise<void> => {
    if (stopped || inFlight || !watched()) return
    inFlight = true
    const status = await refreshEngine(store.dispatch, api)
    inFlight = false
    if (stopped) return
    if (status !== null) {
      if (lastReachable !== undefined && lastReachable !== status.reachable) options.onReachableChange?.(status.reachable)
      lastReachable = status.reachable
    }
    schedule()
  }

  const unsubscribe = store.subscribe(() => {
    if (!watched()) {
      clear()
      return
    }
    if (timer === null && !inFlight) {
      void tick()
      return
    }
    schedule()
  })
  if (watched()) void tick()
  return () => {
    stopped = true
    unsubscribe()
    clear()
  }
}

/**
 * Whether the host's `detail` sentence adds to the client copy. It does for
 * a mismatch (both homes / both versions) and an unsupported platform (the
 * tuple), and for a live daemon when no facts line already shows pid + home;
 * for missing / stopped / starting / installing it repeats the client's own
 * sentence in other words, so those hide it.
 * @param status - the host's status.
 * @param factsShown - whether the surface already renders the facts line.
 * @returns true when the detail line is worth a row.
 */
export function hostDetailShown(status: EngineStatus | null, factsShown: boolean): boolean {
  if (status === null || status.detail === '') return false
  switch (status.state) {
    case 'mismatch':
    case 'unsupported':
      return true
    case 'running':
    case 'attached':
      return !factsShown
    case 'missing':
    case 'stopped':
    case 'starting':
    case 'installing':
      return false
  }
}

// ------------------------------------------------------------------ text

/**
 * Keep both ends of a long path (`/home/…/.ccteam`), the full text going in
 * the element's `title`.
 * @param text - the text.
 * @param max - maximum length including the ellipsis.
 * @returns the shortened text.
 */
export function truncateMiddle(text: string, max: number): string {
  if (max < 3 || text.length <= max) return text
  const head = Math.ceil((max - 1) / 2)
  const tail = max - 1 - head
  return `${text.slice(0, head)}…${tail > 0 ? text.slice(text.length - tail) : ''}`
}

/**
 * Drop ANSI escape sequences (the daemon log is tracing's colored output) so
 * the tail reads as text.
 * @param text - one log line.
 * @returns the line without escapes.
 */
export function stripAnsi(text: string): string {
  // eslint-disable-next-line no-control-regex
  return text.replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, '')
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
