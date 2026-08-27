import type { SessionCredentialStore } from './credentials.js';
export interface ContentBlock {
    type: string;
    text?: string;
    [key: string]: unknown;
}
export interface McpToolResult {
    content: ContentBlock[];
    isError: boolean;
    [key: string]: unknown;
}
export interface CcteamMcpClientOptions {
    daemonUrl: string;
    credential: () => string | undefined;
    clientName: string;
    clientVersion: string;
    fetchImpl?: typeof fetch;
}
export declare class CcteamMcpClient {
    private readonly daemonUrl;
    private readonly credential;
    private readonly clientName;
    private readonly clientVersion;
    private readonly fetchImpl;
    private sessionId;
    private initializing;
    private closed;
    constructor(options: CcteamMcpClientOptions);
    initialize(): Promise<void>;
    callTool(name: string, args: unknown): Promise<McpToolResult>;
    private request;
    close(): void;
    private assertOpen;
}
/**
 * One MCP client per distinct daemon credential: a bearer IS an identity on the
 * daemon, so two hires sharing one `Mcp-Session-Id` would share one ledger node.
 * The enrollment client (mode 2, hand-installed plugin) stays shared.
 */
export declare class CcteamMcpClientPool {
    private readonly daemonUrl;
    private readonly enrollment;
    private readonly credentials;
    private readonly clientName;
    private readonly clientVersion;
    private readonly fetchImpl;
    private readonly byCredential;
    private enrollmentClient;
    private readonly offRemoved;
    constructor(options: {
        daemonUrl: () => string;
        enrollment: () => string | undefined;
        credentials?: SessionCredentialStore;
        clientName: string;
        clientVersion: string;
        fetchImpl?: typeof fetch;
    });
    /** Resolve the caller: its own session bearer when known, else enrollment. */
    clientFor(exec: ToolRunContext): CcteamMcpClient;
    private forEnrollment;
    private forCredential;
    private build;
    private urlFor;
    private key;
    close(): void;
}
export declare function sessionIdOfAgent(agent: DshAgent | undefined): string | undefined;
export interface ToolRunContext {
    agent?: DshAgent;
    signal?: AbortSignal;
    [key: string]: unknown;
}
export interface DshAgent {
    id?: string;
    session?: {
        id?: string;
        events?: unknown[];
    };
    inbox?: {
        remove?(messageId: string): boolean;
    };
    followup?(message: unknown): void;
    whenIdle?(): Promise<void>;
    cancel?(cause: {
        kind: string;
    }, options?: {
        keepInbox?: boolean;
    }): void;
    [key: string]: unknown;
}
export interface ToolRegistryContext {
    tools: {
        register(definition: DshToolDefinition): () => void;
    };
    effect?<T extends (() => void | Promise<void>) | void>(setup: () => T, label?: string): () => void;
}
export interface DshToolDefinition {
    name: string;
    description: string;
    parameters: unknown;
    output: {
        schema: Record<string, unknown>;
        render(args: unknown, value: McpToolResult): ContentBlock[];
    };
    execute(args: unknown, exec: ToolRunContext): Promise<McpToolResult>;
}
export interface DelegationNotifier {
    maybeNotify(toolName: string, args: unknown, result: McpToolResult, exec: ToolRunContext, client: CcteamMcpClient): void;
}
/** Resolves which daemon identity a tool call runs under. */
export type McpClientForExec = (exec: ToolRunContext) => CcteamMcpClient;
interface McpToolDefinition {
    name: string;
    description: string;
    inputSchema: unknown;
}
export declare const CCTEAM_TOOL_DEFINITIONS: McpToolDefinition[];
export declare function registerCcteamTools(ctx: ToolRegistryContext, clientFor: McpClientForExec, notifier?: DelegationNotifier): void;
export declare class CcteamCompletionNotifier implements DelegationNotifier {
    private readonly pollIntervalMs;
    private readonly maxPolls;
    private readonly sleep;
    private closed;
    constructor(options?: {
        pollIntervalMs?: number;
        maxPolls?: number;
        sleep?: (ms: number) => Promise<void>;
    });
    maybeNotify(toolName: string, args: unknown, result: McpToolResult, exec: ToolRunContext, client: CcteamMcpClient): void;
    private pollAndFollowup;
    close(): void;
}
/** A ccteam-minted user turn; its `id` is what turn attribution binds on. */
export interface UserTextMessage {
    readonly id: string;
    readonly role: 'user';
    readonly content: readonly [{
        readonly type: 'text';
        readonly text: string;
    }];
    readonly source: {
        readonly kind: 'user';
    };
}
export declare function createUserTextMessage(text: string): UserTextMessage;
export {};
//# sourceMappingURL=tools.d.ts.map