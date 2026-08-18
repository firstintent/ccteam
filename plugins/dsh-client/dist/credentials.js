/** Plugin-scoped map of DSH session id → ccteam identity. */
export class SessionCredentialStore {
    entries = new Map();
    removalListeners = new Set();
    /** Overwrite the identity of one session (later `session/new` or `session/load` wins). */
    set(sessionId, meta) {
        this.entries.set(sessionId, meta);
    }
    get(sessionId) {
        if (sessionId === undefined)
            return undefined;
        return this.entries.get(sessionId);
    }
    /** Best-effort cleanup; publishes the removed entry so credential caches can drop it. */
    delete(sessionId) {
        const removed = this.entries.get(sessionId);
        if (removed === undefined)
            return;
        this.entries.delete(sessionId);
        for (const listener of this.removalListeners) {
            try {
                listener(sessionId, removed);
            }
            catch {
                // a listener failure must not break session teardown
            }
        }
    }
    onRemoved(listener) {
        this.removalListeners.add(listener);
        return () => {
            this.removalListeners.delete(listener);
        };
    }
    get size() {
        return this.entries.size;
    }
}
/** Read `params._meta.ccteam` from an ACP request; returns undefined when absent. */
export function parseCcteamMeta(params) {
    const meta = asRecord(asRecord(params)._meta).ccteam;
    if (!isRecord(meta))
        return undefined;
    const parsed = {};
    const sid = trimmedString(meta.sid);
    if (sid !== undefined)
        parsed.sid = sid;
    const bearer = trimmedString(meta.bearer);
    if (bearer !== undefined)
        parsed.bearer = bearer;
    const mcpUrl = trimmedString(meta.mcpUrl);
    if (mcpUrl !== undefined)
        parsed.mcpUrl = mcpUrl;
    parsed.approvalMode = meta.approvalMode === 'hitl' ? 'hitl' : 'skip';
    return parsed;
}
function trimmedString(value) {
    if (typeof value !== 'string')
        return undefined;
    const trimmed = value.trim();
    return trimmed === '' ? undefined : trimmed;
}
function asRecord(value) {
    return isRecord(value) ? value : {};
}
function isRecord(value) {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
}
//# sourceMappingURL=credentials.js.map