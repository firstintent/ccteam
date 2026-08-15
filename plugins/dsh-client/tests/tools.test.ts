import { describe, expect, it, vi } from 'vitest'
import { CcteamMcpClient, CCTEAM_TOOL_DEFINITIONS, registerCcteamTools } from '../src/tools.js'

describe('ccteam tools', () => {
  it('keeps the original MCP tool registration count and names', () => {
    const names = CCTEAM_TOOL_DEFINITIONS.map(tool => tool.name).sort()
    expect(CCTEAM_TOOL_DEFINITIONS).toHaveLength(8)
    expect(names).toEqual([
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

  it('registers every tool through ctx.tools.register', () => {
    const registered: string[] = []
    const client = {
      callTool: vi.fn(),
    } as unknown as CcteamMcpClient
    registerCcteamTools({
      tools: {
        register: (tool) => {
          registered.push(tool.name)
          return () => undefined
        },
      },
      effect: (setup) => {
        setup()
        return () => undefined
      },
    }, client)

    expect(registered.sort()).toEqual(CCTEAM_TOOL_DEFINITIONS.map(tool => tool.name).sort())
  })

  it('initializes over streamable HTTP, captures Mcp-Session-Id, and echoes it on tool calls', async () => {
    const requests: RequestInit[] = []
    const methods: string[] = []
    const fetchImpl = vi.fn(async (_url: string, init: RequestInit) => {
      requests.push(init)
      const body = JSON.parse(String(init.body)) as { method: string; id: string }
      methods.push(body.method)
      if (body.method === 'initialize') {
        return new Response(JSON.stringify({
          jsonrpc: '2.0',
          id: body.id,
          result: { serverInfo: { name: 'ccteam' } },
        }), { headers: { 'Mcp-Session-Id': 'session-from-daemon' } })
      }
      return new Response(JSON.stringify({
        jsonrpc: '2.0',
        id: body.id,
        result: { content: [{ type: 'text', text: 'pong' }], isError: false },
      }))
    })

    const client = new CcteamMcpClient({
      daemonUrl: 'http://127.0.0.1:7331/',
      credential: () => 'ccteam-enroll:e1:secret',
      clientName: 'test-client',
      clientVersion: '0',
      fetchImpl,
    })

    const result = await client.callTool('status', {})

    expect(result.content).toEqual([{ type: 'text', text: 'pong' }])
    expect(methods).toEqual(['initialize', 'tools/call'])
    expect((requests[0]!.headers as Record<string, string>).authorization).toBe('Bearer ccteam-enroll:e1:secret')
    expect((requests[0]!.headers as Record<string, string>)['mcp-session-id']).toBeUndefined()
    expect((requests[1]!.headers as Record<string, string>).authorization).toBe('Bearer ccteam-enroll:e1:secret')
    expect((requests[1]!.headers as Record<string, string>)['mcp-session-id']).toBe('session-from-daemon')
  })

  it('surfaces daemon tool errors as daemon content', async () => {
    const client = new CcteamMcpClient({
      daemonUrl: 'http://127.0.0.1:7331',
      credential: () => 'ccteam-enroll:e1:secret',
      clientName: 'test-client',
      clientVersion: '0',
      fetchImpl: vi.fn(async (_url: string, init: RequestInit) => {
        const body = JSON.parse(String(init.body)) as { method: string; id: string }
        if (body.method === 'initialize') {
          return new Response(JSON.stringify({ jsonrpc: '2.0', id: body.id, result: {} }))
        }
        return new Response(JSON.stringify({
          jsonrpc: '2.0',
          id: body.id,
          result: {
            content: [{ type: 'text', text: 'name a workspace: alpha, beta' }],
            isError: true,
          },
        }))
      }),
    })

    const result = await client.callTool('session_spawn', { task: 'x' })

    expect(result.isError).toBe(true)
    expect(result.content[0]?.text).toBe('name a workspace: alpha, beta')
  })
})
