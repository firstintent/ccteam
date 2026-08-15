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
    followup?(message: unknown): void;
    whenIdle?(): Promise<void>;
    cancel?(cause: {
        kind: string;
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
    maybeNotify(toolName: string, args: unknown, result: McpToolResult, exec: ToolRunContext): void;
}
interface McpToolDefinition {
    name: string;
    description: string;
    inputSchema: unknown;
}
export declare const CCTEAM_TOOL_DEFINITIONS: McpToolDefinition[];
export declare function registerCcteamTools(ctx: ToolRegistryContext, client: CcteamMcpClient, notifier?: DelegationNotifier): void;
export declare class CcteamCompletionNotifier implements DelegationNotifier {
    private readonly client;
    private readonly pollIntervalMs;
    private readonly maxPolls;
    private readonly sleep;
    private closed;
    constructor(client: CcteamMcpClient, options?: {
        pollIntervalMs?: number;
        maxPolls?: number;
        sleep?: (ms: number) => Promise<void>;
    });
    maybeNotify(toolName: string, args: unknown, result: McpToolResult, exec: ToolRunContext): void;
    private pollAndFollowup;
    close(): void;
}
export declare function createUserTextMessage(text: string): unknown;
export {};
//# sourceMappingURL=tools.d.ts.map