import { describe, expect, it, vi, afterEach } from 'vitest'
import { apply } from '../src/index.js'

interface FakeTool {
  name: string
  execute(args: unknown, exec: unknown): Promise<unknown>
}

function makeCtx(settings?: Record<string, unknown>) {
  const tools: FakeTool[] = []
  const cleanups: unknown[] = []
  const ctx = {
    tools: {
      register: vi.fn((tool: FakeTool) => {
        tools.push(tool)
        return vi.fn()
      }),
    },
    settings: {
      register: vi.fn(() => ({
        get: () => ({
          daemonUrl: 'http://daemon.test',
          enrollment: '',
          connectionStatus: '',
          boundProject: '',
          ...settings,
        }),
      })),
    },
    agents: {},
    on: vi.fn(() => vi.fn()),
    effect: vi.fn((setup: () => unknown) => {
      cleanups.push(setup())
      return vi.fn()
    }),
    logger: { warn: vi.fn() },
  }
  return { ctx, tools, cleanups }
}

afterEach(() => {
  vi.unstubAllGlobals()
  delete process.env.CCTEAM_MCP_BEARER
  delete process.env.CCTEAM_DSH_TRANSPORT
})

describe('apply', () => {
  it('scrubs CCTEAM_MCP_BEARER synchronously and keeps using the captured bearer', async () => {
    process.env.CCTEAM_MCP_BEARER = 'ccteam-sid:s1:secret'
    const seenAuth: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
      seenAuth.push(String((init.headers as Record<string, string>).authorization))
      const body = JSON.parse(String(init.body)) as { method: string }
      if (body.method === 'initialize') {
        return new Response(JSON.stringify({
          jsonrpc: '2.0',
          id: 'init',
          result: { protocolVersion: '2024-11-05', capabilities: {}, serverInfo: { name: 'ccteam' } },
        }), {
          headers: { 'Mcp-Session-Id': 'mcp-process-1' },
        })
      }
      return new Response(JSON.stringify({
        jsonrpc: '2.0',
        id: 'call',
        result: { content: [{ type: 'text', text: '{"ok":true}' }], isError: false },
      }))
    }))

    const { ctx, tools } = makeCtx()
    apply(ctx)

    expect(process.env.CCTEAM_MCP_BEARER).toBeUndefined()

    const status = tools.find(tool => tool.name === 'status')
    expect(status).toBeDefined()
    await status!.execute({}, { agent: { session: { id: 'dsh-parent' }, followup: vi.fn() } })
    expect(seenAuth).toEqual([
      'Bearer ccteam-sid:s1:secret',
      'Bearer ccteam-sid:s1:secret',
    ])
  })

  it('does not start the transport for enrollment-only hand-started mode', () => {
    const { ctx } = makeCtx({ enrollment: 'ccteam-enroll:e1:secret' })
    apply(ctx)

    expect(ctx.on).not.toHaveBeenCalled()
  })

  it('registers exactly the eight original ccteam tool names', () => {
    const { ctx, tools } = makeCtx()
    apply(ctx)

    expect(tools.map(tool => tool.name).sort()).toEqual([
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
})
