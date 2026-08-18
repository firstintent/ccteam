import { type SessionCredentialStore } from './credentials.js';
import { type DshAgent } from './tools.js';
export interface DshAgents {
    create(options: {
        sessionId: string;
        meta?: {
            cwd?: string;
            agentPreset?: string;
        };
        agentOptions?: unknown;
        setup?: (agentCtx: unknown) => Promise<void>;
    }): Promise<DshAgentHandle>;
    resume(options: {
        resumeSessionId: string;
        agentOptions?: unknown;
        setup?: (agentCtx: unknown) => Promise<void>;
    }): Promise<DshAgentHandle>;
    get?(id: string): DshAgent | undefined;
}
export interface DshAgentHandle {
    agent: DshAgent;
    dispose?(): Promise<void> | void;
}
export interface DshWorkspace {
    attachSession(sessionId: string): Promise<void>;
}
/**
 * DSH's agent-preset roster (Cordis service `agentPresets`,
 * `@deepseek-ai/dsh-agent-presets`). The web bundle DISABLES the host-plane
 * vendor tools and moves them behind presets, so an agent created without
 * `setup: mount(...)` has no bash/read/write at all — only globally-registered
 * tools like ccteam's own (real-machine `unknown tool "bash"` regression).
 * Shipped ids: `standard` | `code` (PTC) | `minimal` | `cordis` (creator).
 */
export interface DshAgentPresets {
    resolve(id?: string): Promise<{
        id?: string;
    } | undefined>;
    mount(agentCtx: unknown, id?: string): Promise<unknown>;
}
export interface DshWorkspaceRegistry {
    create(path: string, title?: string): Promise<DshWorkspace>;
}
export interface TransportContext {
    agents: DshAgents;
    /**
     * Optional service lookup (`ctx.get`). Cordis THROWS on `ctx.workspaceRegistry`
     * when the service is not in this plugin's `inject` list — and it cannot be:
     * `dsh-workspace` ships in the web-app bundle only, so a hard inject would
     * dead-lock plugin activation on every non-web profile (mode 2). `ctx.get`
     * is the vendor's own optional accessor (dsh-host-apiproxy uses it for
     * `sessionPersistence`).
     */
    get?(name: string): unknown;
    agentDefaultModel?: {
        currentSelection(): {
            provider?: string;
            model?: string;
        } | undefined;
    };
    on?(event: string, handler: (...args: never[]) => unknown): () => void;
    effect?<T extends (() => void | Promise<void>) | void>(setup: () => T, label?: string): () => void;
    logger?: {
        warn(message: string): void;
    };
}
export interface DshSocketTransportOptions {
    version: string;
    /** Unix socket path this plugin listens on for ccteam ACP peers. */
    socketPath: string;
    credentials: SessionCredentialStore;
}
export interface DshTransportOptions {
    version: string;
    input: NodeJS.ReadableStream;
    output: NodeJS.WritableStream;
    credentials?: SessionCredentialStore;
    workspaces?: WorkspaceMounter;
}
/**
 * Serializes `workspaceRegistry.create` the way the DSH host does: concurrent
 * creates of one path would otherwise race to own the same canonical directory.
 */
export declare class WorkspaceMounter {
    private readonly ctx;
    private chain;
    constructor(ctx: TransportContext);
    mount(cwd: string, sessionId: string): Promise<void>;
}
/** Unix-socket ACP listener: one isolated {@link DshAcpServer} per connection. */
export declare class DshSocketTransport {
    private readonly ctx;
    private readonly options;
    private readonly workspaces;
    private readonly peers;
    private server;
    private offDisposed;
    private closed;
    constructor(ctx: TransportContext, options: DshSocketTransportOptions);
    /** Bind the socket. Never throws: a bind failure warns and leaves the plugin working. */
    listen(): Promise<void>;
    private accept;
    close(): Promise<void>;
}
/**
 * Start the socket transport, scoped to the plugin effect when available.
 * @returns a teardown that closes the listener and its peers.
 */
export declare function startDshSocketTransport(ctx: TransportContext, options: DshSocketTransportOptions): () => Promise<void>;
export declare class DshAcpServer {
    private readonly ctx;
    private readonly version;
    private readonly input;
    private readonly output;
    private readonly credentials;
    private readonly workspaces;
    private readonly sessions;
    private readonly pendingClientRequests;
    private buffer;
    private closed;
    constructor(ctx: TransportContext, options: DshTransportOptions);
    start(): () => Promise<void>;
    private receive;
    private handleLine;
    private handleClientResponse;
    private handleNotification;
    private handleRequest;
    private presetService;
    /**
     * Preset for a NEW session: the ccteam-requested id when present, else the
     * vendor's own default. Without the roster service, an explicit request is a
     * hard error (silently creating a toolless agent is the exact bug this
     * exists to prevent), while no request degrades to a bare create with a
     * warning — an ACP-bundle-style runtime keeps its host-plane tools.
     */
    private composeCreatePreset;
    /** Re-mount the STORED preset when resuming a persisted session. */
    private composeResumeSetup;
    /**
     * The preset a persisted session was using: the newest
     * `agent-preset/selected` event wins (a blank-session switch in the DSH UI),
     * else the creation header's `agentPreset`; `undefined` mounts the vendor
     * default. Mirrors the vendor's own `resolveSessionPreset` fold — the
     * package is not importable from this plugin, so the two-field fold is
     * reimplemented against the same durable data.
     */
    private storedPreset;
    private newSession;
    private loadSession;
    private prompt;
    private onSessionEvent;
    /** True while `turn` is the turn that claimed this transport's queued message. */
    private ownsTurn;
    /** True while the owned turn is also the turn currently running on the agent. */
    private ownsActiveTurn;
    private ownsToolResult;
    private onAssistantMessage;
    private onAssistantChunk;
    private onAgentError;
    private onApprovalRequest;
    private requestPermission;
    private notify;
    private write;
    private writeError;
    private resolveAgentOptions;
}
//# sourceMappingURL=transport.d.ts.map