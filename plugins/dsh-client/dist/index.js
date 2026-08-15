import Schema from '@deepseek-ai/schemastery';
import { CcteamCompletionNotifier, CcteamMcpClient, registerCcteamTools } from './tools.js';
import { DEFAULT_DAEMON_URL, registerCcteamSettings } from './settings.js';
import { shouldStartTransport, startDshTransport } from './transport.js';
export const name = 'ccteam-client';
export const inject = ['agents', 'tools', 'settings'];
export const Config = Schema.object({
    daemonUrl: Schema.string().default(DEFAULT_DAEMON_URL).description('ccteam daemon URL.'),
    enrollment: Schema.string().default('').role('secret').description('Enrollment credential from ccteam config.'),
    connectionStatus: Schema.string().default('Not checked. If the daemon is unreachable, run: ccteam start'),
    boundProject: Schema.string().default(''),
    completionPollIntervalMs: Schema.number().default(5000),
    completionMaxPolls: Schema.number().default(720),
});
const PACKAGE_VERSION = '0.9.15-alpha.0';
export function apply(ctx, config = {}) {
    const bootBearer = process.env.CCTEAM_MCP_BEARER;
    delete process.env.CCTEAM_MCP_BEARER;
    const settings = registerCcteamSettings(ctx, {
        daemonUrl: config.daemonUrl,
        enrollment: config.enrollment,
        connectionStatus: config.connectionStatus,
        boundProject: config.boundProject,
    });
    const daemonUrl = () => config.daemonUrl ?? settings.get().daemonUrl ?? DEFAULT_DAEMON_URL;
    const credential = () => {
        if (bootBearer !== undefined && bootBearer.trim() !== '')
            return bootBearer;
        const enrolled = config.enrollment ?? settings.get().enrollment;
        return enrolled === undefined || enrolled.trim() === '' ? undefined : enrolled;
    };
    const client = new CcteamMcpClient({
        daemonUrl: daemonUrl(),
        credential,
        clientName: '@ccteam/dsh-client',
        clientVersion: PACKAGE_VERSION,
    });
    const notifier = new CcteamCompletionNotifier(client, {
        pollIntervalMs: config.completionPollIntervalMs,
        maxPolls: config.completionMaxPolls,
    });
    ctx.effect?.(() => () => {
        notifier.close();
        client.close();
    }, 'ccteam.mcp.client');
    registerCcteamTools(ctx, client, notifier);
    if (shouldStartTransport(process.env, bootBearer)) {
        startDshTransport(ctx, {
            version: PACKAGE_VERSION,
            approvalMode: process.env.CCTEAM_DSH_APPROVAL === 'hitl' ? 'hitl' : 'skip',
        });
    }
}
//# sourceMappingURL=index.js.map