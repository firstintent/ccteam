import { afterEach, describe, expect, it, vi } from 'vitest'
import { apply } from '../src/index.js'
import { AcpClient, makeFakeCtx, shortSocketPath } from './fakes.js'

interface WireCall {
  authorization: string
  sessionId: string | undefined
  method: string
}

function stubDaemon(): WireCall[] {
  const calls: WireCall[] = []
  const sessionIds = new Map<string, string>()
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
    const headers = init.headers as Record<string, string>
    const body = JSON.parse(String(init.body)) as { method: string; id: string }
    calls.push({
      authorization: headers.authorization,
      sessionId: headers['mcp-session-id'],
      method: body.method,
    })
    if (body.method === 'initialize') {
      const issued = sessionIds.get(headers.authorization)
        ?? `mcp-${sessionIds.size + 1}`
      sessionIds.set(headers.authorization, issued)
      return new Response(JSON.stringify({
        jsonrpc: '2.0',
        id: body.id,
        result: { protocolVersion: '2024-11-05', capabilities: {}, serverInfo: { name: 'ccteam' } },
      }), { headers: { 'Mcp-Session-Id': issued } })
    }
    return new Response(JSON.stringify({
      jsonrpc: '2.0',
      id: body.id,
      result: { content: [{ type: 'text', text: '{"ok":true}' }], isError: false },
    }))
  }))
  return calls
}

const closers: (() => void)[] = []

afterEach(() => {
  vi.unstubAllGlobals()
  for (const close of closers.splice(0)) close()
  delete process.env.CCTEAM_MCP_BEARER
  delete process.env.CCTEAM_DSH_TRANSPORT
  delete process.env.CCTEAM_DSH_APPROVAL
})

describe('apply', () => {
  it('registers exactly the eight original ccteam tool names', () => {
    const h = makeFakeCtx()
    apply(h.ctx as never)

    expect(h.tools.map(tool => tool.name).sort()).toEqual([
      'chat_send_file',
      'grok_claude_codex_kimi',
      'session_collect',
      'session_dispatch',
      'session_list',
      'session_spawn',
      'session_stop',
      'status',
    ])
  })

  it('runs tool-surface-only (mode 2) on the enrollment credential with no listeners', async () => {
    const calls = stubDaemon()
    const h = makeFakeCtx({ settings: { enrollment: 'ccteam-enroll:e1:secret' } })
    apply(h.ctx as never)

    expect(h.ctx.on).not.toHaveBeenCalled()

    const status = h.tools.find(tool => tool.name === 'status')!
    await status.execute({}, { agent: { id: 'dsh-1', session: { id: 'dsh-1' }, followup: vi.fn() } })
    expect(calls.map(call => call.authorization)).toEqual([
      'Bearer ccteam-enroll:e1:secret',
      'Bearer ccteam-enroll:e1:secret',
    ])
  })

  it('never takes a credential or a transport switch from the environment', async () => {
    process.env.CCTEAM_MCP_BEARER = 'ccteam-sid:s99:from-env'
    process.env.CCTEAM_DSH_TRANSPORT = '1'
    process.env.CCTEAM_DSH_APPROVAL = 'hitl'
    const calls = stubDaemon()
    const h = makeFakeCtx({ settings: { enrollment: 'ccteam-enroll:e1:secret' } })
    apply(h.ctx as never)

    // No transport: the env switch is gone, only `transportSocket` starts one.
    expect(h.ctx.on).not.toHaveBeenCalled()

    const status = h.tools.find(tool => tool.name === 'status')!
    await status.execute({}, { agent: { id: 'dsh-1', session: { id: 'dsh-1' }, followup: vi.fn() } })
    expect(calls.every(call => call.authorization === 'Bearer ccteam-enroll:e1:secret')).toBe(true)
    expect(calls.some(call => call.authorization.includes('from-env'))).toBe(false)
  })

  it('routes each session to its own daemon identity and falls back to enrollment', async () => {
    const calls = stubDaemon()
    const socketPath = shortSocketPath()
    const h = makeFakeCtx({ settings: { enrollment: 'ccteam-enroll:e1:secret' } })
    apply(h.ctx as never, { transportSocket: socketPath })

    const client = await AcpClient.connect(socketPath)
    closers.push(() => client.close())

    const first = await client.request('session/new', {
      cwd: '/tmp/one',
      _meta: { ccteam: { sid: 's1', bearer: 'ccteam-sid:s1:aaa' } },
    })
    const second = await client.request('session/new', {
      cwd: '/tmp/two',
      _meta: { ccteam: { sid: 's2', bearer: 'ccteam-sid:s2:bbb' } },
    })

    const status = h.tools.find(tool => tool.name === 'status')!
    await status.execute({}, { agent: h.agents.get(first.sessionId as string) })
    await status.execute({}, { agent: h.agents.get(second.sessionId as string) })
    await status.execute({}, { agent: { id: 'hand-started', session: { id: 'hand-started' } } })

    const toolCalls = calls.filter(call => call.method === 'tools/call')
    expect(toolCalls.map(call => call.authorization)).toEqual([
      'Bearer ccteam-sid:s1:aaa',
      'Bearer ccteam-sid:s2:bbb',
      'Bearer ccteam-enroll:e1:secret',
    ])
    // Distinct credentials are distinct daemon identities: distinct MCP sessions.
    const mcpSessions = toolCalls.map(call => call.sessionId)
    expect(new Set(mcpSessions).size).toBe(3)
    expect(calls.filter(call => call.method === 'initialize')).toHaveLength(3)

    // Re-running a session's tool reuses its own MCP session, no re-initialize.
    await status.execute({}, { agent: h.agents.get(first.sessionId as string) })
    expect(calls.filter(call => call.method === 'initialize')).toHaveLength(3)
    expect(calls.at(-1)?.sessionId).toBe(mcpSessions[0])

    // No credential ever reaches the process environment.
    expect(Object.values(process.env).some(value => (value ?? '').includes('ccteam-sid:'))).toBe(false)
  })
})
