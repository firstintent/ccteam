import { describe, expect, it, vi } from 'vitest'
import { SessionCredentialStore } from '../src/credentials.js'
import { CcteamMcpClient, CcteamMcpClientPool, CCTEAM_TOOL_DEFINITIONS, registerCcteamTools } from '../src/tools.js'

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
    }, () => client)

    expect(registered.sort()).toEqual(CCTEAM_TOOL_DEFINITIONS.map(tool => tool.name).sort())
  })

  it('pools one MCP client per distinct credential and shares the enrollment client', () => {
    const credentials = new SessionCredentialStore()
    credentials.set('session-a', { bearer: 'ccteam-sid:s1:aaa' })
    credentials.set('session-b', { bearer: 'ccteam-sid:s2:bbb' })
    credentials.set('session-c', { bearer: 'ccteam-sid:s1:aaa' })
    const pool = new CcteamMcpClientPool({
      daemonUrl: () => 'http://daemon.test',
      enrollment: () => 'ccteam-enroll:e1:secret',
      credentials,
      clientName: 'test-client',
      clientVersion: '0',
    })

    const a = pool.clientFor({ agent: { id: 'session-a' } })
    const b = pool.clientFor({ agent: { id: 'session-b' } })
    const c = pool.clientFor({ agent: { id: 'session-c' } })
    const enrolled = pool.clientFor({ agent: { id: 'unknown-session' } })
    const enrolledAgain = pool.clientFor({})

    expect(a).not.toBe(b)
    expect(a).toBe(c)
    expect(enrolled).toBe(enrolledAgain)
    expect(enrolled).not.toBe(a)

    // A disposed session's client is dropped, and a later one is rebuilt.
    credentials.delete('session-a')
    credentials.set('session-a', { bearer: 'ccteam-sid:s1:aaa' })
    expect(pool.clientFor({ agent: { id: 'session-a' } })).not.toBe(a)

    pool.close()
  })

  it('dials the /mcp endpoint exactly once when _meta.mcpUrl already names it', async () => {
    // The Rust side sends the ENDPOINT url (`http://…:7331/mcp`) in
    // `_meta.ccteam.mcpUrl`. The pool must normalize it to a base — forwarding
    // it verbatim double-suffixed every per-session call to `/mcp/mcp`, which
    // is not the exempt MCP route: with web auth enabled the daemon answered a
    // plain-text 401 `auth required` (owner-reported real-machine regression).
    const urls: string[] = []
    const fetchImpl = vi.fn(async (url: string, init: RequestInit) => {
      urls.push(url)
      const body = JSON.parse(String(init.body)) as { method: string; id: string }
      return new Response(JSON.stringify({
        jsonrpc: '2.0',
        id: body.id,
        result: body.method === 'initialize'
          ? {}
          : { content: [{ type: 'text', text: 'pong' }], isError: false },
      }))
    })
    const credentials = new SessionCredentialStore()
    credentials.set('session-a', {
      bearer: 'ccteam-sid:s1:aaa',
      mcpUrl: 'http://127.0.0.1:7331/mcp',
    })
    const pool = new CcteamMcpClientPool({
      daemonUrl: () => 'http://daemon.test',
      enrollment: () => 'ccteam-enroll:e1:secret',
      credentials,
      clientName: 'test-client',
      clientVersion: '0',
      fetchImpl,
    })

    await pool.clientFor({ agent: { id: 'session-a' } }).callTool('status', {})

    expect(urls.length).toBeGreaterThan(0)
    for (const url of urls) {
      expect(url).toBe('http://127.0.0.1:7331/mcp')
    }
    pool.close()
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
