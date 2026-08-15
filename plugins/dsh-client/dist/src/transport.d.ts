import { type DshAgent } from './tools.js';
export interface DshAgents {
    create(options: {
        sessionId: string;
        meta?: {
            cwd?: string;
        };
        agentOptions?: unknown;
    }): Promise<DshAgentHandle>;
    resume(options: {
        resumeSessionId: string;
    }): Promise<DshAgentHandle>;
    get?(id: string): DshAgent | undefined;
}
export interface DshAgentHandle {
    agent: DshAgent;
    dispose?(): Promise<void> | void;
}
export interface TransportContext {
    agents: DshAgents;
    on?(event: string, handler: (...args: never[]) => unknown): () => void;
    effect?<T extends (() => void | Promise<void>) | void>(setup: () => T, label?: string): () => void;
    logger?: {
        warn(message: string): void;
    };
}
export interface DshTransportOptions {
    version: string;
    input?: NodeJS.ReadableStream;
    output?: NodeJS.WritableStream;
    approvalMode?: 'skip' | 'hitl';
}
export declare function shouldStartTransport(env: NodeJS.ProcessEnv, bootBearer: string | undefined): boolean;
export declare function startDshTransport(ctx: TransportContext, options: DshTransportOptions): void;
export declare class DshAcpServer {
    private readonly ctx;
    private readonly version;
    private readonly input;
    private readonly output;
    private readonly approvalMode;
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
    private newSession;
    private loadSession;
    private prompt;
    private onSessionEvent;
    private onAssistantMessage;
    private onAssistantChunk;
    private onAgentError;
    private onApprovalRequest;
    private requestPermission;
    private notify;
    private write;
    private writeError;
}
//# sourceMappingURL=transport.d.ts.map