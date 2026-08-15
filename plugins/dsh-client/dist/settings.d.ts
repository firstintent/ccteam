import Schema from '@deepseek-ai/schemastery';
export declare const DEFAULT_DAEMON_URL = "http://127.0.0.1:7331";
export declare const SETTINGS_NAMESPACE = "ccteam-client";
export interface CcteamSettings {
    daemonUrl: string;
    enrollment: string;
    connectionStatus: string;
    boundProject: string;
}
export declare const CcteamSettingsSchema: Schema<CcteamSettings>;
export interface SettingsScope<T> {
    get(): T;
    watch?(callback: (next: T, prev: T) => void | Promise<void>): () => void;
}
export interface SettingsContext {
    settings?: {
        register<T>(ns: string, schema: Schema<T>, options?: {
            applies?: 'live' | 'restart';
            base?: Partial<T>;
        }): SettingsScope<T>;
    };
}
export declare function registerCcteamSettings(ctx: SettingsContext, base?: Partial<CcteamSettings>): SettingsScope<CcteamSettings>;
//# sourceMappingURL=settings.d.ts.map