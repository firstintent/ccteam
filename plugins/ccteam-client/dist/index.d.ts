import Schema from '@deepseek-ai/schemastery';
import { type CcteamSettings } from './settings.js';
export declare const name = "ccteam-client";
export declare const inject: string[];
export interface Config extends Partial<CcteamSettings> {
    completionPollIntervalMs?: number;
    completionMaxPolls?: number;
    transportSocket?: string;
}
export declare const Config: Schema<Config>;
export interface ApplyContext {
    tools: {
        register(definition: unknown): () => void;
    };
    agents: unknown;
    settings?: {
        register<T>(ns: string, schema: Schema<T>, options?: {
            applies?: 'live' | 'restart';
            base?: Partial<T>;
        }): {
            get(): T;
        };
    };
    agentDefaultModel?: {
        currentSelection(): {
            provider?: string;
            model?: string;
        } | undefined;
    };
    workspaceRegistry?: unknown;
    on?(event: string, handler: (...args: never[]) => unknown): () => void;
    effect?<T extends (() => void | Promise<void>) | void>(setup: () => T, label?: string): () => void;
    logger?: {
        warn(message: string): void;
    };
}
export declare function apply(ctx: ApplyContext, config?: Config): void;
//# sourceMappingURL=index.d.ts.map