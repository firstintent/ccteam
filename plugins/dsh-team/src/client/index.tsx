/**
 * Client half: the ccteam team panel, composed exclusively from DSH-native
 * material (owner decree 2026-08-21): components from
 * `@deepseek-ai/dsh-client-ui-primitives`, semantic `--dsw-alias-*` /
 * `--dsw-specific-*` tokens only, copy through the DSH locale service.
 * Zero imports from ccteam-web.
 *
 * Mount strategy (version-gated):
 *   1. entry button   → slot `sidebar.footer.action` (list, root)
 *   2. panel overlay  → slot `shell.overlay`         (list, root)
 *   3. fallback       → body-portal (older DSH without those slots)
 *
 * All network traffic goes through the host BFF (src/shared/contract.ts).
 */

export const name = 'ccteam-team'
export const inject = ['slots', 'locale']

export interface ClientContext {
  slots: {
    register(options: Record<string, unknown>, component: unknown): () => void
    inject(key: string, callback: () => unknown): () => void
  }
  effect?<T extends (() => void | Promise<void>) | void>(
    setup: () => T,
    label?: string,
  ): () => void
  logger?: { warn(message: string): void }
}

export function apply(_ctx: ClientContext): void {
  // TODO(DSH2-UI): version-gated slot registration + Panel tree
}
