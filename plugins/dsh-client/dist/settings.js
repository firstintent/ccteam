import Schema from '@deepseek-ai/schemastery';
export const DEFAULT_DAEMON_URL = 'http://127.0.0.1:7331';
export const SETTINGS_NAMESPACE = 'ccteam-client';
export const CcteamSettingsSchema = Schema.object({
    daemonUrl: Schema.string().default(DEFAULT_DAEMON_URL).description('ccteam daemon URL.'),
    enrollment: Schema.string().default('').role('secret').description('Enrollment credential from ccteam config.'),
    connectionStatus: Schema.string().default('Not checked. If the daemon is unreachable, run: ccteam start'),
    boundProject: Schema.string().default(''),
});
export function registerCcteamSettings(ctx, base) {
    if (ctx.settings === undefined) {
        return {
            get: () => ({
                daemonUrl: base?.daemonUrl ?? DEFAULT_DAEMON_URL,
                enrollment: base?.enrollment ?? '',
                connectionStatus: base?.connectionStatus ?? 'Settings service unavailable. If the daemon is unreachable, run: ccteam start',
                boundProject: base?.boundProject ?? '',
            }),
        };
    }
    return ctx.settings.register(SETTINGS_NAMESPACE, CcteamSettingsSchema, {
        applies: 'restart',
        base,
    });
}
//# sourceMappingURL=settings.js.map