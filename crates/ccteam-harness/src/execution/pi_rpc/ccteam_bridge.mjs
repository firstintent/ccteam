const TOOL_PREFIX = "ccteam_";
const PERMISSION_TITLE = "__ccteam_permission_v1__";
const MAX_PERMISSION_BYTES = 64 * 1024;

function requiredEnv(name) {
  const value = process.env[name];
  if (!value) throw new Error(`Pi ccteam bridge requires ${name}`);
  return value;
}

function textFromContent(content) {
  return (Array.isArray(content) ? content : [])
    .filter((item) => item && item.type === "text" && typeof item.text === "string")
    .map((item) => item.text)
    .join("\n");
}

export default function ccteamBridge(pi) {
  const endpoint = requiredEnv("CCTEAM_MCP_HTTP_URL");
  const bearer = requiredEnv("CCTEAM_MCP_BEARER");
  let rpcId = 0;

  async function mcp(method, params, notification = false) {
    const body = { jsonrpc: "2.0", method, params };
    if (!notification) body.id = `pi-${++rpcId}`;
    const response = await fetch(endpoint, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${bearer}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    });
    if (!response.ok) {
      throw new Error(`ccteam MCP ${method} failed: HTTP ${response.status}`);
    }
    if (notification || response.status === 202) return undefined;
    const payload = await response.json();
    if (payload.error) {
      throw new Error(`ccteam MCP ${method} failed: ${payload.error.message || "unknown error"}`);
    }
    return payload.result;
  }

  if (process.env.CCTEAM_PERMISSION_MODE === "hitl") {
    pi.on("tool_call", async (event, ctx) => {
      const envelope = JSON.stringify({
        toolCallId: event.toolCallId,
        toolName: event.toolName,
        input: event.input,
      });
      if (new TextEncoder().encode(envelope).byteLength > MAX_PERMISSION_BYTES) {
        return { block: true, reason: "payload too large" };
      }
      const confirmed = await ctx.ui.confirm(PERMISSION_TITLE, envelope, { timeout: 120000 });
      if (!confirmed) {
        return { block: true, reason: "Tool call denied by ccteam HITL policy" };
      }
    });
  }

  pi.on("session_start", async (_event, ctx) => {
    await mcp("initialize", {
      protocolVersion: "2025-03-26",
      capabilities: {},
      clientInfo: { name: "ccteam-pi-bridge", version: "1" },
    });
    await mcp("notifications/initialized", {}, true);
    const listed = await mcp("tools/list", {});
    if (!listed || !Array.isArray(listed.tools)) {
      throw new Error("ccteam MCP tools/list returned no tools array");
    }

    const registered = [];
    for (const tool of listed.tools) {
      if (!tool || typeof tool.name !== "string" || !tool.inputSchema) {
        throw new Error("ccteam MCP tools/list returned an invalid tool");
      }
      const piName = `${TOOL_PREFIX}${tool.name}`;
      pi.registerTool({
        name: piName,
        label: tool.title || tool.name,
        description: tool.description || tool.name,
        parameters: tool.inputSchema,
        async execute(_toolCallId, params) {
          const result = await mcp("tools/call", { name: tool.name, arguments: params });
          if (result && result.isError) {
            throw new Error(textFromContent(result.content) || `ccteam tool ${tool.name} failed`);
          }
          return {
            content: result && Array.isArray(result.content) ? result.content : [],
            details: { mcpTool: tool.name },
          };
        },
      });
      registered.push(piName);
    }
    ctx.ui.setStatus("ccteam.bridge", `ready:${registered.join(",")}`);
  });
}
