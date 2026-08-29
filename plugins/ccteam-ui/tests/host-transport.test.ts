import { mkdirSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { randomUUID } from 'node:crypto'
import { afterEach, describe, expect, it } from 'vitest'
import { SessionCredentialStore } from '../src/credentials.js'
import { startDshSocketTransport } from '../src/transport.js'
import { AcpClient, makeFakeCtx, settle, shortSocketPath, waitFor } from './host-cordis-fakes.js'

const teardowns: (() => Promise<void> | void)[] = []

afterEach(async () => {
  for (const teardown of teardowns.splice(0).reverse()) await teardown()
})

async function startTransport(options?: Parameters<typeof makeFakeCtx>[0] & { socketPath?: string }) {
  const harness = makeFakeCtx(options)
  const credentials = new SessionCredentialStore()
  const socketPath = options?.socketPath ?? shortSocketPath()
  const stop = startDshSocketTransport(harness.ctx as never, {
    version: '9.9.9',
    socketPath,
    credentials,
  })
  teardowns.push(stop)
  return { ...harness, credentials, socketPath, stop }
}

async function connectClient(socketPath: string): Promise<AcpClient> {
  const client = await waitForConnect(socketPath)
  teardowns.push(() => client.close())
  return client
}

async function waitForConnect(socketPath: string): Promise<AcpClient> {
  let lastError: unknown
  for (let i = 0; i < 100; i++) {
    try {
      return await AcpClient.connect(socketPath)
    } catch (error) {
      lastError = error
      await new Promise(resolve => setTimeout(resolve, 5))
    }
  }
  throw new Error(`could not connect to ${socketPath}: ${String(lastError)}`)
}

async function isPending(promise: Promise<unknown>): Promise<boolean> {
  const pending = Symbol('pending')
  const settled = promise.then(() => 'settled' as const, () => 'settled' as const)
  const raced = await Promise.race([
    settled,
    new Promise(resolve => setTimeout(() => resolve(pending), 25)),
  ])
  return raced === pending
}

function queuedMessageId(agentFollowup: { mock: { calls: unknown[][] } }): string {
  const message = agentFollowup.mock.calls.at(-1)?.[0] as { id: string }
  return message.id
}

describe('DSH ACP socket transport', () => {
  it('serves initialize, session/new with a workspace mount, and an owned turn', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)

    const init = await client.request('initialize', {})
    expect(init).toMatchObject({
      agentInfo: { name: 'ccteam-dsh-client', version: '9.9.9' },
      agentCapabilities: { loadSession: true },
    })

    const created = await client.request('session/new', {
      cwd: '/tmp/work',
      agentOptions: { model: 'deepseek-reasoner' },
      _meta: { ccteam: { sid: 's7', bearer: 'ccteam-sid:s7:secret' } },
    })
    const sessionId = created.sessionId as string
    expect(h.create).toHaveBeenCalledWith({
      sessionId,
      meta: { cwd: '/tmp/work', agentPreset: 'standard' },
      agentOptions: { provider: 'aliyun', model: 'deepseek-reasoner' },
      setup: expect.any(Function),
    })
    expect(h.workspaceCreate).toHaveBeenCalledWith('/tmp/work')
    expect(h.workspaces.get('/tmp/work')?.attachSession).toHaveBeenCalledWith(sessionId)
    expect(h.credentials.get(sessionId)?.bearer).toBe('ccteam-sid:s7:secret')

    const agent = h.agents.get(sessionId)!
    const prompt = client.request('session/prompt', {
      sessionId,
      prompt: [{ type: 'text', text: 'hello' }],
    })
    await waitFor(() => agent.followup.mock.calls.length === 1, 'followup')
    const messageId = queuedMessageId(agent.followup)

    h.sessionEvent(sessionId, 'turn/start', { turn: 1 })
    h.sessionEvent(sessionId, 'user/message', { id: messageId, role: 'user', content: [{ type: 'text', text: 'hello' }] })
    h.sessionEvent(sessionId, 'assistant/chunk', { turn: 1, step: 0, chunk: { type: 'reasoning-delta', text: 'thinking' } })
    h.sessionEvent(sessionId, 'assistant/message', {
      turn: 1,
      step: 0,
      message: { content: [{ type: 'text', text: 'partial answer' }] },
      usage: { inputTokens: 4, outputTokens: 2 },
    })
    h.sessionEvent(sessionId, 'turn/end', { turn: 1, reason: { kind: 'max-tokens' } })

    const result = await prompt
    expect(result).toMatchObject({
      stopReason: 'max_tokens',
      _meta: { stopReason: 'max_tokens', inputTokens: 4, outputTokens: 2 },
    })
    expect(client.updates).toContainEqual({
      sessionId,
      update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'partial answer' } },
    })
    expect(client.updates).toContainEqual({
      sessionId,
      update: { sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: 'thinking' } },
    })
    expect(client.updates).toContainEqual({
      sessionId,
      update: { sessionUpdate: 'turn_completed' },
    })
  })

  it('fills omitted agentOptions from DSH default model settings', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)

    const created = await client.request('session/new', { cwd: '/tmp/work' })
    expect(h.create).toHaveBeenCalledWith({
      sessionId: created.sessionId,
      meta: { cwd: '/tmp/work', agentPreset: 'standard' },
      agentOptions: { provider: 'aliyun', model: 'deepseek-v4-pro' },
      setup: expect.any(Function),
    })
    expect(created.models.currentModelId).toBe('aliyun/deepseek-v4-pro')
  })

  it('mounts the vendor default preset on a bare session/new', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)

    await client.request('session/new', { cwd: '/tmp/work' })

    const request = h.create.mock.calls[0]![0] as {
      meta: { agentPreset?: string }
      setup?: (agentCtx: unknown) => Promise<void>
    }
    expect(request.meta.agentPreset).toBe('standard')
    expect(request.setup).toBeTypeOf('function')
    await request.setup!('agent-ctx')
    expect(h.agentPresets.mount).toHaveBeenCalledWith('agent-ctx', 'standard')
  })

  it('mounts the ccteam-requested preset from _meta.ccteam.agentPreset', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)

    await client.request('session/new', {
      cwd: '/tmp/work',
      _meta: { ccteam: { sid: 's9', bearer: 'ccteam-sid:s9:x', agentPreset: 'code' } },
    })

    const request = h.create.mock.calls[0]![0] as {
      meta: { agentPreset?: string }
      setup?: (agentCtx: unknown) => Promise<void>
    }
    expect(h.agentPresets.resolve).toHaveBeenCalledWith('code')
    expect(request.meta.agentPreset).toBe('code')
    await request.setup!('agent-ctx')
    expect(h.agentPresets.mount).toHaveBeenCalledWith('agent-ctx', 'code')
  })

  it('refuses an explicit preset when the runtime has no agentPresets service', async () => {
    const h = await startTransport({ presets: false })
    const client = await connectClient(h.socketPath)

    await expect(
      client.request('session/new', {
        cwd: '/tmp/work',
        _meta: { ccteam: { agentPreset: 'code' } },
      }),
    ).rejects.toThrow(/agentPresets/)
    expect(h.create).not.toHaveBeenCalled()
  })

  it('creates bare with a warning when nothing was requested and no roster exists', async () => {
    const h = await startTransport({ presets: false })
    const client = await connectClient(h.socketPath)

    const created = await client.request('session/new', { cwd: '/tmp/work' })
    expect(created.sessionId).toBeTypeOf('string')
    const request = h.create.mock.calls[0]![0] as { setup?: unknown; meta: { agentPreset?: string } }
    expect(request.setup).toBeUndefined()
    expect(request.meta.agentPreset).toBeUndefined()
    expect(h.warnings.some(line => line.includes('agentPresets'))).toBe(true)
  })

  it('re-mounts the STORED preset on resume, ignoring the _meta request', async () => {
    const h = await startTransport({
      persistence: {
        meta: { agentPreset: 'minimal' },
        events: [
          { type: 'agent-preset/selected', data: { agentPreset: 'cordis' } },
          { type: 'turn/start', data: { turn: 1 } },
        ],
      },
    })
    const client = await connectClient(h.socketPath)

    await client.request('session/load', {
      sessionId: 'persisted-1',
      _meta: { ccteam: { agentPreset: 'code' } },
    })

    const request = h.resume.mock.calls[0]![0] as { setup?: (agentCtx: unknown) => Promise<void> }
    expect(request.setup).toBeTypeOf('function')
    await request.setup!('agent-ctx')
    // The newest agent-preset/selected event outranks the creation header,
    // and the ccteam-side request never overrides vendor storage.
    expect(h.agentPresets.mount).toHaveBeenCalledWith('agent-ctx', 'cordis')
  })

  it('does not mount anything when session/load reuses a live agent', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)
    const created = await client.request('session/new', { cwd: '/tmp/work' })
    h.agentPresets.mount.mockClear()

    await client.request('session/load', { sessionId: created.sessionId })

    expect(h.resume).not.toHaveBeenCalled()
    expect(h.agentPresets.mount).not.toHaveBeenCalled()
  })

  it('pins danger-full-access on a skip-posture hire', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)

    const created = await client.request('session/new', {
      cwd: '/tmp/work',
      _meta: { ccteam: { sid: 's8', bearer: 'ccteam-sid:s8:x', approvalMode: 'skip' } },
    })

    const agent = h.agents.get(created.sessionId as string)!
    expect(h.permissionPresets.set).toHaveBeenCalledWith(agent.session, 'danger-full-access')
  })

  it('keeps the vendor default permission preset on a hitl hire', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)

    await client.request('session/new', {
      cwd: '/tmp/work',
      _meta: { ccteam: { sid: 's8', bearer: 'ccteam-sid:s8:x', approvalMode: 'hitl' } },
    })

    expect(h.permissionPresets.set).not.toHaveBeenCalled()
  })

  it('creates the session with a warning when permissionPresets is absent', async () => {
    const h = await startTransport({ permissions: false })
    const client = await connectClient(h.socketPath)

    const created = await client.request('session/new', { cwd: '/tmp/work' })
    expect(created.sessionId).toBeTypeOf('string')
    expect(h.warnings.some(line => line.includes('permissionPresets'))).toBe(true)
  })

  it('creates the session without a workspaceRegistry and warns', async () => {
    const h = await startTransport({ workspaces: false })
    const client = await connectClient(h.socketPath)

    const created = await client.request('session/new', { cwd: '/tmp/work' })
    expect(created.sessionId).toBeTypeOf('string')
    expect(h.warnings.some(line => line.includes('workspaceRegistry'))).toBe(true)
  })

  it('keeps the session when the workspace attach fails', async () => {
    const h = await startTransport({ attachFails: true })
    const client = await connectClient(h.socketPath)

    const created = await client.request('session/new', { cwd: '/tmp/work' })
    expect(created.sessionId).toBeTypeOf('string')
    expect(h.warnings.some(line => line.includes('could not mount'))).toBe(true)
  })

  it('does not forward or resolve on a human-initiated turn', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)
    const created = await client.request('session/new', { cwd: '/tmp/work' })
    const sessionId = created.sessionId as string
    const agent = h.agents.get(sessionId)!

    const prompt = client.request('session/prompt', {
      sessionId,
      prompt: [{ type: 'text', text: 'ccteam task' }],
    })
    await waitFor(() => agent.followup.mock.calls.length === 1, 'followup')
    const messageId = queuedMessageId(agent.followup)

    // A human types in the DSH web UI while our message waits in the inbox.
    h.sessionEvent(sessionId, 'turn/start', { turn: 1 })
    h.sessionEvent(sessionId, 'user/message', { id: 'human-message', role: 'user', content: [{ type: 'text', text: 'hi' }] })
    h.sessionEvent(sessionId, 'assistant/chunk', { turn: 1, step: 0, chunk: { type: 'reasoning-delta', text: 'human thought' } })
    h.sessionEvent(sessionId, 'assistant/message', {
      turn: 1,
      step: 0,
      message: { content: [{ type: 'text', text: 'human answer' }] },
      usage: { inputTokens: 100, outputTokens: 100 },
    })
    h.sessionEvent(sessionId, 'tool/call', { turn: 1, step: 0, callId: 'human-call', name: 'bash', arguments: '{}' })
    h.sessionEvent(sessionId, 'tool/result', { turn: 1, step: 0, message: { source: { callId: 'human-call' }, content: [] } })
    h.sessionEvent(sessionId, 'turn/end', { turn: 1, reason: { kind: 'completed' } })

    await settle()
    expect(client.updates).toEqual([])
    expect(await isPending(prompt)).toBe(true)

    // Now the turn that claimed OUR message.
    h.sessionEvent(sessionId, 'turn/start', { turn: 2 })
    h.sessionEvent(sessionId, 'user/message', { id: messageId, role: 'user', content: [{ type: 'text', text: 'ccteam task' }] })
    h.sessionEvent(sessionId, 'assistant/message', {
      turn: 2,
      step: 0,
      message: { content: [{ type: 'text', text: 'ccteam answer' }] },
      usage: { inputTokens: 7, outputTokens: 3 },
    })
    h.sessionEvent(sessionId, 'tool/call', { turn: 2, step: 0, callId: 'ours', name: 'bash', arguments: '{"a":1}' })
    h.sessionEvent(sessionId, 'tool/result', { turn: 2, step: 0, message: { source: { callId: 'ours' }, content: [] } })
    h.sessionEvent(sessionId, 'turn/end', { turn: 2, reason: { kind: 'completed' } })

    const result = await prompt
    expect(result).toMatchObject({
      stopReason: 'end_turn',
      _meta: { inputTokens: 7, outputTokens: 3 },
    })
    const texts = JSON.stringify(client.updates)
    expect(texts).toContain('ccteam answer')
    expect(texts).not.toContain('human answer')
    expect(texts).not.toContain('human thought')
    expect(texts).not.toContain('human-call')
    expect(client.updates.filter(update => (update.update as { sessionUpdate: string }).sessionUpdate === 'turn_completed'))
      .toHaveLength(1)
  })

  it('cancels the active owned turn through the agent', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)
    const created = await client.request('session/new', { cwd: '/tmp/work' })
    const sessionId = created.sessionId as string
    const agent = h.agents.get(sessionId)!

    const prompt = client.request('session/prompt', { sessionId, prompt: [{ type: 'text', text: 'go' }] })
    await waitFor(() => agent.followup.mock.calls.length === 1, 'followup')
    const messageId = queuedMessageId(agent.followup)
    h.sessionEvent(sessionId, 'turn/start', { turn: 1 })
    h.sessionEvent(sessionId, 'user/message', { id: messageId, role: 'user', content: [] })

    client.notify('session/cancel', { sessionId })

    expect(await prompt).toMatchObject({ stopReason: 'cancelled' })
    expect(agent.cancel).toHaveBeenCalledWith({ kind: 'user' })
    expect(agent.inbox.remove).not.toHaveBeenCalled()
  })

  it('cancels a still-queued prompt through the inbox', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)
    const created = await client.request('session/new', { cwd: '/tmp/work' })
    const sessionId = created.sessionId as string
    const agent = h.agents.get(sessionId)!

    const prompt = client.request('session/prompt', { sessionId, prompt: [{ type: 'text', text: 'go' }] })
    await waitFor(() => agent.followup.mock.calls.length === 1, 'followup')
    const messageId = queuedMessageId(agent.followup)

    client.notify('session/cancel', { sessionId })

    expect(await prompt).toMatchObject({ stopReason: 'cancelled' })
    expect(agent.inbox.remove).toHaveBeenCalledWith(messageId)
    expect(agent.cancel).not.toHaveBeenCalled()
  })

  it('answers approvals only for the owned active turn', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)
    const created = await client.request('session/new', { cwd: '/tmp/work' })
    const sessionId = created.sessionId as string
    const agent = h.agents.get(sessionId)!

    // A human turn is running and no ccteam prompt is inflight: pass through.
    h.sessionEvent(sessionId, 'turn/start', { turn: 1 })
    expect(await h.requestApproval({ agent, toolName: 'bash', callId: 'human-call' })).toBe('unavailable')
    h.sessionEvent(sessionId, 'turn/end', { turn: 1, reason: { kind: 'completed' } })

    const prompt = client.request('session/prompt', { sessionId, prompt: [{ type: 'text', text: 'go' }] })
    await waitFor(() => agent.followup.mock.calls.length === 1, 'followup')
    const messageId = queuedMessageId(agent.followup)

    // Queued but not yet running: still not ours to answer.
    expect(await h.requestApproval({ agent, toolName: 'bash', callId: 'queued' })).toBe('unavailable')

    h.sessionEvent(sessionId, 'turn/start', { turn: 2 })
    h.sessionEvent(sessionId, 'user/message', { id: messageId, role: 'user', content: [] })
    expect(await h.requestApproval({ agent, toolName: 'bash', callId: 'ours' })).toBe('allowed-once')

    h.sessionEvent(sessionId, 'turn/end', { turn: 2, reason: { kind: 'completed' } })
    await prompt
    // The turn closed: a later human turn's approval passes through again.
    expect(await h.requestApproval({ agent, toolName: 'bash', callId: 'later' })).toBe('unavailable')
  })

  it('routes hitl approvals to the ccteam client for the owned turn', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)
    const created = await client.request('session/new', {
      cwd: '/tmp/work',
      _meta: { ccteam: { bearer: 'ccteam-sid:s3:secret', approvalMode: 'hitl' } },
    })
    const sessionId = created.sessionId as string
    const agent = h.agents.get(sessionId)!

    const prompt = client.request('session/prompt', { sessionId, prompt: [{ type: 'text', text: 'go' }] })
    await waitFor(() => agent.followup.mock.calls.length === 1, 'followup')
    const messageId = queuedMessageId(agent.followup)
    h.sessionEvent(sessionId, 'turn/start', { turn: 1 })
    h.sessionEvent(sessionId, 'user/message', { id: messageId, role: 'user', content: [] })

    client.permissionResponder = () => ({ outcome: { outcome: 'selected', optionId: 'reject-once' } })
    expect(await h.requestApproval({ agent, toolName: 'bash', callId: 'ours' })).toBe('rejected')

    h.sessionEvent(sessionId, 'turn/end', { turn: 1, reason: { kind: 'completed' } })
    await prompt
  })

  it('reuses a live agent on session/load instead of resuming', async () => {
    const h = await startTransport()
    const creator = await connectClient(h.socketPath)
    const created = await creator.request('session/new', { cwd: '/tmp/work' })
    const sessionId = created.sessionId as string

    const loader = await connectClient(h.socketPath)
    const loaded = await loader.request('session/load', {
      sessionId,
      _meta: { ccteam: { bearer: 'ccteam-sid:s9:secret' } },
    })
    expect(loaded.sessionId).toBe(sessionId)
    expect(h.resume).not.toHaveBeenCalled()
    expect(h.credentials.get(sessionId)?.bearer).toBe('ccteam-sid:s9:secret')
  })

  it('resumes a cold session on session/load', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)

    const loaded = await client.request('session/load', { sessionId: 'cold-session' })
    expect(loaded.sessionId).toBe('cold-session')
    expect(h.resume).toHaveBeenCalledWith(expect.objectContaining({ resumeSessionId: 'cold-session' }))
  })

  it('isolates concurrent connections', async () => {
    const h = await startTransport()
    const first = await connectClient(h.socketPath)
    const second = await connectClient(h.socketPath)

    const firstSession = (await first.request('session/new', { cwd: '/tmp/one' })).sessionId as string
    const secondSession = (await second.request('session/new', { cwd: '/tmp/two' })).sessionId as string
    expect(firstSession).not.toBe(secondSession)

    await expect(second.request('session/prompt', {
      sessionId: firstSession,
      prompt: [{ type: 'text', text: 'not yours' }],
    })).rejects.toThrow(/unknown session/)

    // Events of the first connection's session never reach the second peer.
    const firstAgent = h.agents.get(firstSession)!
    const prompt = first.request('session/prompt', { sessionId: firstSession, prompt: [{ type: 'text', text: 'go' }] })
    await waitFor(() => firstAgent.followup.mock.calls.length === 1, 'followup')
    const messageId = queuedMessageId(firstAgent.followup)
    h.sessionEvent(firstSession, 'turn/start', { turn: 1 })
    h.sessionEvent(firstSession, 'user/message', { id: messageId, role: 'user', content: [] })
    h.sessionEvent(firstSession, 'assistant/message', { turn: 1, step: 0, message: { content: [{ type: 'text', text: 'only for one' }] } })
    h.sessionEvent(firstSession, 'turn/end', { turn: 1, reason: { kind: 'completed' } })
    await prompt

    expect(JSON.stringify(first.updates)).toContain('only for one')
    expect(second.updates).toEqual([])
  })

  it('leaves agents live and un-cancelled when a connection closes', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)
    const created = await client.request('session/new', { cwd: '/tmp/work' })
    const sessionId = created.sessionId as string
    const agent = h.agents.get(sessionId)!

    client.close()
    await settle(10)

    expect(agent.cancel).not.toHaveBeenCalled()
    expect(h.agents.get(sessionId)).toBe(agent)
  })

  it('drops credentials when the session is disposed', async () => {
    const h = await startTransport()
    const client = await connectClient(h.socketPath)
    const created = await client.request('session/new', {
      cwd: '/tmp/work',
      _meta: { ccteam: { bearer: 'ccteam-sid:s4:secret' } },
    })
    const sessionId = created.sessionId as string
    expect(h.credentials.get(sessionId)?.bearer).toBe('ccteam-sid:s4:secret')

    h.emit('session/disposed', { id: sessionId })
    expect(h.credentials.get(sessionId)).toBeUndefined()
  })

  it('warns instead of throwing when the socket cannot be bound', async () => {
    const busy = join(tmpdir(), `cct-dir-${randomUUID().slice(0, 8)}`)
    mkdirSync(busy, { recursive: true })
    const h = await startTransport({ socketPath: busy })
    await waitFor(() => h.warnings.some(line => line.includes('cannot listen on')), 'listen failure warning')
    expect(h.warnings.some(line => line.includes('cannot listen on'))).toBe(true)
  })
})
