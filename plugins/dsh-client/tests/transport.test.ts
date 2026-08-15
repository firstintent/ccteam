import { PassThrough } from 'node:stream'
import { describe, expect, it, vi } from 'vitest'
import { shouldStartTransport, startDshTransport } from '../src/transport.js'

function makeRpcHarness() {
  const input = new PassThrough()
  const output = new PassThrough()
  const lines: unknown[] = []
  output.on('data', chunk => {
    for (const line of chunk.toString().split('\n')) {
      if (line.trim() !== '') lines.push(JSON.parse(line))
    }
  })
  return {
    input,
    output,
    lines,
    send: (message: unknown) => input.write(`${JSON.stringify(message)}\n`),
    waitFor: async (count: number) => {
      for (let i = 0; i < 100; i++) {
        if (lines.length >= count) return
        await new Promise(resolve => setTimeout(resolve, 5))
      }
      throw new Error(`timed out waiting for ${count} lines, saw ${lines.length}`)
    },
  }
}

function makeTransportCtx() {
  const handlers = new Map<string, (...args: unknown[]) => unknown>()
  let idleResolve!: () => void
  const idle = new Promise<void>(resolve => { idleResolve = resolve })
  const agent = {
    session: { id: '' },
    followup: vi.fn(),
    whenIdle: vi.fn(() => idle),
    cancel: vi.fn(),
  }
  const ctx = {
    agents: {
      create: vi.fn(async (options: { sessionId: string }) => {
        agent.session.id = options.sessionId
        return { agent, dispose: vi.fn() }
      }),
      resume: vi.fn(),
    },
    on: vi.fn((event: string, handler: (...args: unknown[]) => unknown) => {
      handlers.set(event, handler)
      return vi.fn()
    }),
    effect: vi.fn((setup: () => unknown) => {
      setup()
      return vi.fn()
    }),
    logger: { warn: vi.fn() },
  }
  return { ctx, handlers, agent, idleResolve }
}

describe('transport gate', () => {
  it('requires both the env switch and a sid-family bearer', () => {
    expect(shouldStartTransport({ CCTEAM_DSH_TRANSPORT: '1' }, undefined)).toBe(false)
    expect(shouldStartTransport({ CCTEAM_DSH_TRANSPORT: '1' }, 'ccteam-enroll:e1:secret')).toBe(false)
    expect(shouldStartTransport({}, 'ccteam-sid:s1:secret')).toBe(false)
    expect(shouldStartTransport({ CCTEAM_DSH_TRANSPORT: '1' }, 'ccteam-sid:s1:secret')).toBe(true)
  })
})

describe('DSH ACP transport', () => {
  it('serves initialize, session/new, prompt result, and session/update frames', async () => {
    const rpc = makeRpcHarness()
    const { ctx, handlers, agent, idleResolve } = makeTransportCtx()
    startDshTransport(ctx, { version: '1.2.3', input: rpc.input, output: rpc.output })

    rpc.send({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} })
    await rpc.waitFor(1)
    expect(rpc.lines[0]).toMatchObject({
      id: 1,
      result: {
        agentInfo: { name: 'ccteam-dsh-client', version: '1.2.3' },
        agentCapabilities: { loadSession: true },
      },
    })

    rpc.send({
      jsonrpc: '2.0',
      id: 2,
      method: 'session/new',
      params: { cwd: '/tmp/work', agentOptions: { model: 'deepseek-reasoner' } },
    })
    await rpc.waitFor(2)
    const newResult = rpc.lines[1] as { result: { sessionId: string } }
    expect(ctx.agents.create).toHaveBeenCalledWith({
      sessionId: newResult.result.sessionId,
      meta: { cwd: '/tmp/work' },
      agentOptions: { model: 'deepseek-reasoner' },
    })

    rpc.send({
      jsonrpc: '2.0',
      id: 3,
      method: 'session/prompt',
      params: {
        sessionId: newResult.result.sessionId,
        prompt: [{ type: 'text', text: 'hello' }],
      },
    })
    await new Promise(resolve => setTimeout(resolve, 5))
    expect(agent.followup).toHaveBeenCalledWith(expect.objectContaining({
      role: 'user',
      content: [{ type: 'text', text: 'hello' }],
    }))

    handlers.get('session/event')?.(
      { id: newResult.result.sessionId },
      {
        type: 'assistant/message',
        data: {
          message: { content: [{ type: 'text', text: 'partial answer' }] },
          usage: { inputTokens: 4, outputTokens: 2 },
        },
      },
    )
    handlers.get('session/event')?.(
      { id: newResult.result.sessionId },
      {
        type: 'turn/end',
        data: { reason: { kind: 'max-tokens' } },
      },
    )
    idleResolve()

    await rpc.waitFor(5)
    expect(rpc.lines).toContainEqual(expect.objectContaining({
      method: 'session/update',
      params: {
        sessionId: newResult.result.sessionId,
        update: {
          sessionUpdate: 'agent_message_chunk',
          content: { type: 'text', text: 'partial answer' },
        },
      },
    }))
    expect(rpc.lines).toContainEqual(expect.objectContaining({
      method: 'session/update',
      params: {
        sessionId: newResult.result.sessionId,
        update: { sessionUpdate: 'turn_completed' },
      },
    }))
    expect(rpc.lines).toContainEqual(expect.objectContaining({
      id: 3,
      result: expect.objectContaining({
        stopReason: 'max_tokens',
        _meta: expect.objectContaining({ stopReason: 'max_tokens', inputTokens: 4, outputTokens: 2 }),
      }),
    }))
  })
})
