import { randomUUID } from 'node:crypto';
import { createUserTextMessage } from './tools.js';
class RpcError extends Error {
    code;
    data;
    constructor(message, code = -32603, data) {
        super(message);
        this.name = 'RpcError';
        this.code = code;
        this.data = data;
    }
}
export function shouldStartTransport(env, bootBearer) {
    return env.CCTEAM_DSH_TRANSPORT === '1' && typeof bootBearer === 'string' && bootBearer.startsWith('ccteam-sid:');
}
export function startDshTransport(ctx, options) {
    const server = new DshAcpServer(ctx, options);
    if (typeof ctx.effect === 'function') {
        ctx.effect(() => server.start(), 'ccteam.dsh.transport');
    }
    else {
        server.start();
    }
}
export class DshAcpServer {
    ctx;
    version;
    input;
    output;
    approvalMode;
    sessions = new Map();
    pendingClientRequests = new Map();
    buffer = '';
    closed = false;
    constructor(ctx, options) {
        this.ctx = ctx;
        this.version = options.version;
        this.input = options.input ?? process.stdin;
        this.output = options.output ?? process.stdout;
        this.approvalMode = options.approvalMode ?? 'skip';
    }
    start() {
        const onData = (chunk) => this.receive(chunk);
        const onClose = () => { this.closed = true; };
        this.input.on('data', onData);
        this.input.on('end', onClose);
        this.input.on('error', onClose);
        const offSession = this.ctx.on?.('session/event', ((session, event) => {
            this.onSessionEvent(session, event);
        }));
        const offError = this.ctx.on?.('agent/error', ((payload) => {
            this.onAgentError(payload);
        }));
        const offApproval = this.ctx.on?.('approval/request', ((request, next) => {
            return this.onApprovalRequest(request, next);
        }));
        return async () => {
            this.closed = true;
            this.input.off('data', onData);
            this.input.off('end', onClose);
            this.input.off('error', onClose);
            offSession?.();
            offError?.();
            offApproval?.();
            const records = [...this.sessions.values()];
            this.sessions.clear();
            await Promise.allSettled(records.map(async (record) => {
                record.agent.cancel?.({ kind: 'user' });
                await record.dispose?.();
            }));
        };
    }
    receive(chunk) {
        this.buffer += chunk.toString();
        for (;;) {
            const newline = this.buffer.indexOf('\n');
            if (newline < 0)
                break;
            const line = this.buffer.slice(0, newline).trim();
            this.buffer = this.buffer.slice(newline + 1);
            if (line.length === 0)
                continue;
            void this.handleLine(line).catch(error => {
                this.ctx.logger?.warn(`ccteam dsh transport line failed: ${errorMessage(error)}`);
            });
        }
    }
    async handleLine(line) {
        let message;
        try {
            message = JSON.parse(line);
        }
        catch {
            this.writeError(null, -32700, 'parse error');
            return;
        }
        if (message.method === undefined) {
            this.handleClientResponse(message);
            return;
        }
        if (message.id === undefined) {
            await this.handleNotification(message);
            return;
        }
        try {
            const result = await this.handleRequest(message.method, message.params);
            this.write({ jsonrpc: '2.0', id: message.id, result });
        }
        catch (error) {
            const rpc = error instanceof RpcError ? error : new RpcError(errorMessage(error));
            this.writeError(message.id, rpc.code, rpc.message, rpc.data);
        }
    }
    handleClientResponse(message) {
        if (message.id === undefined)
            return;
        const pending = this.pendingClientRequests.get(String(message.id));
        if (pending === undefined)
            return;
        this.pendingClientRequests.delete(String(message.id));
        if (message.error !== undefined) {
            pending.reject(new RpcError(jsonString(message.error), -32603, message.error));
        }
        else {
            pending.resolve(message.result);
        }
    }
    async handleNotification(message) {
        if (message.method === 'session/cancel') {
            const params = asRecord(message.params);
            const sessionId = stringField(params, 'sessionId');
            if (sessionId === undefined)
                return;
            const record = this.sessions.get(sessionId);
            if (record === undefined)
                return;
            record.agent.cancel?.({ kind: 'user' });
            record.inflight?.resolve({ stopReason: 'cancelled', _meta: { stopReason: 'cancelled' } });
            record.inflight = undefined;
        }
    }
    async handleRequest(method, params) {
        switch (method) {
            case 'initialize':
                return {
                    protocolVersion: '0.4',
                    agentInfo: { name: 'ccteam-dsh-client', version: this.version },
                    agentCapabilities: { loadSession: true },
                    authMethods: [],
                };
            case 'session/new':
                return this.newSession(params);
            case 'session/load':
                return this.loadSession(params);
            case 'session/prompt':
                return this.prompt(params);
            case 'session/cancel':
                await this.handleNotification({ method, params });
                return {};
            default:
                throw new RpcError(`method not found: ${method}`, -32601);
        }
    }
    async newSession(params) {
        const body = requireRecord(params, 'session/new params');
        const cwd = requireString(body, 'cwd');
        const sessionId = randomUUID();
        let handle;
        try {
            handle = await this.ctx.agents.create({
                sessionId,
                meta: { cwd },
                agentOptions: body.agentOptions,
            });
        }
        catch (error) {
            throw errorToRpc(error);
        }
        this.sessions.set(sessionId, {
            agent: handle.agent,
            dispose: handle.dispose?.bind(handle),
        });
        return { sessionId };
    }
    async loadSession(params) {
        const body = requireRecord(params, 'session/load params');
        const sessionId = requireString(body, 'sessionId');
        let handle;
        try {
            handle = await this.ctx.agents.resume({ resumeSessionId: sessionId });
        }
        catch (error) {
            throw errorToRpc(error);
        }
        this.sessions.set(sessionId, {
            agent: handle.agent,
            dispose: handle.dispose?.bind(handle),
        });
        return { sessionId };
    }
    async prompt(params) {
        const body = requireRecord(params, 'session/prompt params');
        const sessionId = requireString(body, 'sessionId');
        const record = this.sessions.get(sessionId);
        if (record === undefined)
            throw new RpcError(`unknown session: ${sessionId}`, -32602);
        if (record.inflight !== undefined)
            throw new RpcError('a prompt is already in flight for this session', -32602);
        const text = acpPromptToText(body.prompt);
        if (text.trim() === '')
            throw new RpcError('empty prompt', -32602);
        const result = await new Promise((resolve, reject) => {
            record.inflight = {
                resolve,
                reject,
                usage: {},
            };
            try {
                if (typeof record.agent.followup !== 'function' || typeof record.agent.whenIdle !== 'function') {
                    throw new Error('agent cannot accept prompts');
                }
                record.agent.followup(createUserTextMessage(text));
            }
            catch (error) {
                record.inflight = undefined;
                reject(errorToRpc(error, 'prompt was not queued'));
                return;
            }
            void record.agent.whenIdle().then(() => {
                const inflight = record.inflight;
                if (inflight === undefined)
                    return;
                record.inflight = undefined;
                resolve(promptResultFromReason(inflight.endReason, inflight.usage));
            }, error => {
                record.inflight = undefined;
                reject(errorToRpc(error, 'turn failed'));
            });
        });
        return result;
    }
    onSessionEvent(session, event) {
        const sessionId = sessionIdFromSession(session);
        if (sessionId === undefined)
            return;
        const record = this.sessions.get(sessionId);
        if (record === undefined)
            return;
        const ev = asRecord(event);
        const type = typeof ev.type === 'string' ? ev.type : '';
        const data = asRecord(ev.data);
        switch (type) {
            case 'assistant/message':
                this.onAssistantMessage(sessionId, data, record);
                break;
            case 'assistant/chunk':
                this.onAssistantChunk(sessionId, data, record);
                break;
            case 'tool/call':
                this.notify({
                    sessionId,
                    update: {
                        sessionUpdate: 'tool_call',
                        toolCallId: stringField(data, 'callId') ?? 'tool',
                        name: stringField(data, 'name') ?? 'tool',
                        title: stringField(data, 'name') ?? 'tool',
                        rawInput: parseMaybeJson(data.arguments),
                        status: 'pending',
                    },
                });
                break;
            case 'tool/result':
                this.notify({
                    sessionId,
                    update: {
                        sessionUpdate: 'tool_call_update',
                        toolCallId: stringField(asRecord(asRecord(data.message).source), 'callId') ?? 'tool',
                        status: 'completed',
                        content: toolResultText(data),
                        isError: toolResultIsError(data),
                    },
                });
                break;
            case 'turn/end':
                this.notify({
                    sessionId,
                    update: { sessionUpdate: 'turn_completed' },
                });
                if (record.inflight !== undefined) {
                    const reason = asRecord(data).reason;
                    if (isErrorReason(reason)) {
                        const failure = asRecord(reason).error;
                        record.inflight.reject(new RpcError(`turn failed: ${failureMessage(failure)}`, -32603, failure));
                        record.inflight = undefined;
                    }
                    else {
                        record.inflight.endReason = reason;
                    }
                }
                break;
        }
    }
    onAssistantMessage(sessionId, data, record) {
        const message = asRecord(data.message);
        const content = message.content;
        if (Array.isArray(content)) {
            for (const block of content) {
                const normalized = asRecord(block);
                if (normalized.type === 'text' && typeof normalized.text === 'string' && normalized.text.length > 0) {
                    this.notify({
                        sessionId,
                        update: {
                            sessionUpdate: 'agent_message_chunk',
                            content: { type: 'text', text: normalized.text },
                        },
                    });
                }
            }
        }
        accumulateUsage(record.inflight?.usage, asRecord(data.usage));
    }
    onAssistantChunk(sessionId, data, record) {
        const chunk = asRecord(data.chunk);
        if (chunk.type === 'reasoning-delta' && typeof chunk.text === 'string' && chunk.text.length > 0) {
            this.notify({
                sessionId,
                update: {
                    sessionUpdate: 'agent_thought_chunk',
                    content: { type: 'text', text: chunk.text },
                },
            });
        }
        else if (chunk.type === 'usage') {
            accumulateUsage(record.inflight?.usage, asRecord(chunk.usage));
            const usage = normalizeUsage(asRecord(chunk.usage));
            this.notify({
                sessionId,
                update: {
                    sessionUpdate: 'usage_update',
                    used: usage.inputTokens,
                    size: usage.contextWindow,
                },
            });
        }
    }
    onAgentError(payload) {
        const body = asRecord(payload);
        const agent = body.agent;
        const sessionId = typeof agent?.session?.id === 'string' ? agent.session.id : undefined;
        if (sessionId === undefined)
            return;
        const record = this.sessions.get(sessionId);
        if (record === undefined)
            return;
        const inflight = record.inflight;
        if (inflight === undefined)
            return;
        record.inflight = undefined;
        inflight.reject(errorToRpc(body.error, 'turn failed'));
    }
    onApprovalRequest(request, next) {
        const body = asRecord(request);
        const agent = body.agent;
        const sessionId = typeof agent?.session?.id === 'string' ? agent.session.id : undefined;
        if (sessionId === undefined || this.sessions.get(sessionId)?.agent !== agent) {
            return next();
        }
        if (this.approvalMode !== 'hitl') {
            return 'allowed-once';
        }
        return this.requestPermission(sessionId, stringField(body, 'callId') ?? 'tool').then(result => {
            const outcome = asRecord(asRecord(result).outcome);
            const optionId = stringField(outcome, 'optionId') ?? stringField(asRecord(result), 'optionId');
            if (stringField(outcome, 'outcome') === 'cancelled')
                return 'cancelled';
            return optionId === 'allow-once' ? 'allowed-once' : 'rejected';
        });
    }
    requestPermission(sessionId, toolCallId) {
        const id = randomUUID();
        const promise = new Promise((resolve, reject) => {
            this.pendingClientRequests.set(id, { resolve, reject });
        });
        this.write({
            jsonrpc: '2.0',
            id,
            method: 'session/request_permission',
            params: {
                sessionId,
                toolCall: { toolCallId },
                options: [
                    { optionId: 'allow-once', name: 'Allow once', kind: 'allow_once' },
                    { optionId: 'reject-once', name: 'Reject', kind: 'reject_once' },
                ],
            },
        });
        return promise;
    }
    notify(params) {
        this.write({
            jsonrpc: '2.0',
            method: 'session/update',
            params,
        });
    }
    write(message) {
        if (this.closed)
            return;
        this.output.write(`${JSON.stringify(message)}\n`);
    }
    writeError(id, code, message, data) {
        this.write({
            jsonrpc: '2.0',
            id,
            error: {
                code,
                message,
                ...(data === undefined ? {} : { data }),
            },
        });
    }
}
function acpPromptToText(prompt) {
    if (!Array.isArray(prompt))
        return '';
    const parts = [];
    for (const block of prompt) {
        const item = asRecord(block);
        if (item.type === 'text' && typeof item.text === 'string') {
            parts.push(item.text);
        }
        else if (item.type === 'resource_link') {
            parts.push(`\n[resource_link name=${JSON.stringify(item.name)} uri=${JSON.stringify(item.uri)}]\n`);
        }
        else {
            throw new RpcError('only text and resource_link prompt content is supported', -32602);
        }
    }
    return parts.join('');
}
function promptResultFromReason(reason, usage) {
    const stopReason = turnEndToStopReason(reason);
    const meta = {
        stopReason,
        ...usage.inputTokens === undefined ? {} : { inputTokens: usage.inputTokens },
        ...usage.outputTokens === undefined ? {} : { outputTokens: usage.outputTokens },
        ...usage.cachedReadTokens === undefined ? {} : { cachedReadTokens: usage.cachedReadTokens },
        ...usage.reasoningTokens === undefined ? {} : { reasoningTokens: usage.reasoningTokens },
    };
    return {
        stopReason,
        _meta: meta,
        usage: {
            ...usage.inputTokens === undefined ? {} : { inputTokens: usage.inputTokens },
            ...usage.outputTokens === undefined ? {} : { outputTokens: usage.outputTokens },
            ...usage.cachedReadTokens === undefined ? {} : { cachedInputTokens: usage.cachedReadTokens },
            ...usage.reasoningTokens === undefined ? {} : { reasoningTokens: usage.reasoningTokens },
        },
    };
}
function turnEndToStopReason(reason) {
    const value = asRecord(reason);
    switch (value.kind) {
        case 'completed':
            return 'end_turn';
        case 'max-tokens':
            return 'max_tokens';
        case 'max-turn-requests':
            return 'max_turn_requests';
        case 'blocked':
            return 'refusal';
        case 'interrupted':
            return 'cancelled';
        case 'aborted':
            return 'end_turn';
        case 'error':
            return 'end_turn';
        default:
            return 'end_turn';
    }
}
function accumulateUsage(target, source) {
    if (target === undefined)
        return;
    const usage = normalizeUsage(source);
    target.inputTokens = add(target.inputTokens, usage.inputTokens);
    target.outputTokens = add(target.outputTokens, usage.outputTokens);
    target.cachedReadTokens = add(target.cachedReadTokens, usage.cachedReadTokens);
    target.reasoningTokens = add(target.reasoningTokens, usage.reasoningTokens);
}
function normalizeUsage(source) {
    return {
        inputTokens: numberField(source, 'inputTokens'),
        outputTokens: numberField(source, 'outputTokens'),
        cachedReadTokens: numberField(source, 'cacheReadTokens') ?? numberField(source, 'cachedReadTokens'),
        reasoningTokens: numberField(source, 'reasoningTokens'),
        contextWindow: numberField(source, 'contextWindow') ?? numberField(source, 'size'),
    };
}
function add(left, right) {
    if (right === undefined)
        return left;
    return (left ?? 0) + right;
}
function sessionIdFromSession(session) {
    const header = asRecord(asRecord(session).header);
    return stringField(header, 'id') ?? stringField(asRecord(session), 'id');
}
function toolResultText(data) {
    const content = asRecord(asRecord(data.message).content);
    if (Array.isArray(asRecord(data.message).content)) {
        return { type: 'text', text: JSON.stringify(asRecord(data.message).content) };
    }
    return { type: 'text', text: JSON.stringify(content) };
}
function toolResultIsError(data) {
    const message = asRecord(data.message);
    const content = message.content;
    if (Array.isArray(content)) {
        return content.some(block => asRecord(block).isError === true);
    }
    return data.error !== undefined;
}
function isErrorReason(reason) {
    return asRecord(reason).kind === 'error';
}
function failureMessage(error) {
    const body = asRecord(error);
    if (typeof body.message === 'string')
        return body.message;
    return jsonString(error);
}
function errorToRpc(error, prefix) {
    if (error instanceof RpcError)
        return error;
    const body = asRecord(error);
    const code = typeof body.code === 'string' ? body.code : undefined;
    const message = errorMessage(error);
    const full = prefix === undefined ? message : `${prefix}: ${message}`;
    return new RpcError(full, -32603, code === undefined ? undefined : { code });
}
function requireRecord(value, label) {
    if (!isRecord(value))
        throw new RpcError(`${label} must be an object`, -32602);
    return value;
}
function requireString(value, key) {
    const field = stringField(value, key);
    if (field === undefined)
        throw new RpcError(`${key} must be a string`, -32602);
    return field;
}
function stringField(value, key) {
    const field = value[key];
    return typeof field === 'string' ? field : undefined;
}
function numberField(value, key) {
    const field = value[key];
    return typeof field === 'number' && Number.isFinite(field) ? field : undefined;
}
function parseMaybeJson(value) {
    if (typeof value !== 'string')
        return value;
    try {
        return JSON.parse(value);
    }
    catch {
        return value;
    }
}
function asRecord(value) {
    return isRecord(value) ? value : {};
}
function isRecord(value) {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
}
function jsonString(value) {
    try {
        return JSON.stringify(value);
    }
    catch {
        return String(value);
    }
}
function errorMessage(error) {
    if (error instanceof Error)
        return error.message;
    if (isRecord(error) && typeof error.message === 'string')
        return error.message;
    return String(error);
}
//# sourceMappingURL=transport.js.map