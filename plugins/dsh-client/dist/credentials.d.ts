/**
 * Per-session ccteam identity carried by ACP `_meta.ccteam`.
 *
 * One DSH runtime serves many ccteam hires plus the human at the DSH web UI, so
 * a credential belongs to a session, never to the process. Nothing here ever
 * touches `process.env`.
 */
export interface CcteamSessionMeta {
    /** ccteam gateway session id (`s<N>`), for diagnostics only. */
    sid?: string;
    /** Per-session MCP bearer (`ccteam-sid:<sid>:<secret>`). */
    bearer?: string;
    /** Daemon base URL override for this session. */
    mcpUrl?: string;
    /** Tool-approval posture for turns this transport owns. */
    approvalMode?: 'skip' | 'hitl';
}
/** Plugin-scoped map of DSH session id → ccteam identity. */
export declare class SessionCredentialStore {
    private readonly entries;
    private readonly removalListeners;
    /** Overwrite the identity of one session (later `session/new` or `session/load` wins). */
    set(sessionId: string, meta: CcteamSessionMeta): void;
    get(sessionId: string | undefined): CcteamSessionMeta | undefined;
    /** Best-effort cleanup; publishes the removed entry so credential caches can drop it. */
    delete(sessionId: string): void;
    onRemoved(listener: (sessionId: string, removed: CcteamSessionMeta) => void): () => void;
    get size(): number;
}
/** Read `params._meta.ccteam` from an ACP request; returns undefined when absent. */
export declare function parseCcteamMeta(params: unknown): CcteamSessionMeta | undefined;
//# sourceMappingURL=credentials.d.ts.map