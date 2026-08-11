//! `ToolSurfaceMode` as an EXECUTABLE delivery contract, driven off
//! `AGENT_PROBE_SPECS`.
//!
//! Before this test the mode was documentation: it said "ccteam writes no
//! global config for a bridge vendor" but said nothing about what a **managed
//! spawn** must hand the session. Pi shipped with its endpoint inherited from
//! the parent process instead of resolved, which no per-vendor test noticed —
//! the delivery contract had no home.
//!
//! So the table below is the home: every vendor in `AGENT_PROBE_SPECS` must
//! declare which dialect carries its session MCP endpoint, and each dialect is
//! asserted to actually carry a non-empty URL + the session principal. A
//! seventh vendor added to the registry fails this test until it declares one
//! — which is the point.

use ccteam_core::host_registry::{ToolSurfaceMode, AGENT_PROBE_SPECS};
use ccteam_harness::execution::mcp_config::{
    project_acp_mcp_servers, project_bridge_child_env, project_claude_mcp_json,
    project_codex_thread_config, SessionMcpEndpoint, BRIDGE_MCP_BEARER_ENV, BRIDGE_MCP_URL_ENV,
};

const URL: &str = "http://127.0.0.1:9100/mcp";
const SID: &str = "s77";
const SECRET: &str = "sekret";
const BEARER: &str = "ccteam-sid:s77:sekret";

/// The vendor dialects a managed session's endpoint can be projected into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// Claude: `.ccteam/chat/<sid>/mcp.json` + `--mcp-config`.
    ClaudeMcpJson,
    /// Codex: `thread/start` `config.mcp_servers`.
    CodexThreadConfig,
    /// Grok / OpenCode / Kimi: ACP `session/new` `mcpServers[]`.
    AcpMcpServers,
    /// Pi: child env the ccteam bridge extension reads on load.
    BridgeChildEnv,
}

/// Which dialect each vendor's adapter actually uses. Keep in sync with the
/// adapter — an unlisted vendor is a hard failure, not a silent skip.
fn dialect_of(vendor: &str) -> Option<Dialect> {
    match vendor {
        "claude" => Some(Dialect::ClaudeMcpJson),
        "codex" => Some(Dialect::CodexThreadConfig),
        "grok" | "opencode" | "kimi" => Some(Dialect::AcpMcpServers),
        "pi" => Some(Dialect::BridgeChildEnv),
        _ => None,
    }
}

/// `(url, authorization)` as the vendor would actually receive it.
fn delivered(dialect: Dialect, ep: &SessionMcpEndpoint) -> (String, String) {
    match dialect {
        Dialect::ClaudeMcpJson => {
            let v = project_claude_mcp_json(ep);
            let srv = &v["mcpServers"]["ccteam"];
            (
                srv["url"].as_str().unwrap_or_default().to_string(),
                srv["headers"]["Authorization"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        Dialect::CodexThreadConfig => {
            let v = project_codex_thread_config(ep);
            let srv = &v["mcp_servers"]["ccteam"];
            (
                srv["url"].as_str().unwrap_or_default().to_string(),
                srv["http_headers"]["Authorization"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        Dialect::AcpMcpServers => {
            let servers = project_acp_mcp_servers(ep);
            assert_eq!(servers.len(), 1, "exactly one ccteam ACP entry");
            let srv = &servers[0];
            let authorization = srv["headers"]
                .as_array()
                .expect("ACP requires headers[]")
                .iter()
                .find(|h| h["name"] == "Authorization")
                .and_then(|h| h["value"].as_str())
                .unwrap_or_default()
                .to_string();
            (
                srv["url"].as_str().unwrap_or_default().to_string(),
                authorization,
            )
        }
        Dialect::BridgeChildEnv => {
            let env = project_bridge_child_env(ep);
            let get = |key: &str| {
                env.iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            };
            // The bridge speaks raw MCP over HTTP, so its env holds the bare
            // bearer; normalise to the header form the native dialects carry.
            (
                get(BRIDGE_MCP_URL_ENV),
                format!("Bearer {}", get(BRIDGE_MCP_BEARER_ENV)),
            )
        }
    }
}

/// Every registered vendor delivers the SAME endpoint — same url, same
/// principal — regardless of `tool_surface`. The bridge is a dialect, not a
/// second identity model and not a second security model.
#[test]
fn every_vendor_delivers_the_session_endpoint_in_its_own_dialect() {
    let ep = SessionMcpEndpoint::at(URL, SID, SECRET).expect("principal + url");
    for spec in AGENT_PROBE_SPECS {
        let dialect = dialect_of(spec.vendor).unwrap_or_else(|| {
            panic!(
                "vendor `{}` is registered but declares no session MCP dialect — \
                 add it to `dialect_of` (and give its adapter a projection) so its \
                 managed sessions actually get the ccteam tool face",
                spec.vendor
            )
        });
        let (url, authorization) = delivered(dialect, &ep);
        assert_eq!(url, URL, "{} must carry the resolved url", spec.vendor);
        assert_eq!(
            authorization,
            format!("Bearer {BEARER}"),
            "{} must carry this session's principal",
            spec.vendor
        );
    }
}

/// The `tool_surface` split is exactly "global config vs managed-spawn only".
/// A bridge vendor gets no global vendor-config write (red line: ccteam does
/// not touch pi's config) and therefore MUST get its endpoint at spawn — which
/// is why its dialect is the one that is mandatory at spawn time.
#[test]
fn bridge_vendors_are_spawn_delivery_only() {
    let ep = SessionMcpEndpoint::at(URL, SID, SECRET).expect("principal + url");
    for spec in AGENT_PROBE_SPECS {
        match spec.tool_surface {
            ToolSurfaceMode::NativeMcpConfig => {
                assert!(
                    spec.tool_surface_notice().is_none(),
                    "{}: native config vendors advertise no bridge caveat",
                    spec.vendor
                );
                assert_ne!(
                    dialect_of(spec.vendor),
                    Some(Dialect::BridgeChildEnv),
                    "{}: a native-config vendor must not use the bridge dialect",
                    spec.vendor
                );
            }
            ToolSurfaceMode::ManagedSessionBridge => {
                assert_eq!(
                    dialect_of(spec.vendor),
                    Some(Dialect::BridgeChildEnv),
                    "{}: a bridge vendor has no native MCP config surface, so its \
                     endpoint can only arrive through the bridge dialect",
                    spec.vendor
                );
                let env = project_bridge_child_env(&ep);
                for key in [BRIDGE_MCP_URL_ENV, BRIDGE_MCP_BEARER_ENV] {
                    let value = env
                        .iter()
                        .find(|(k, _)| k == key)
                        .map(|(_, v)| v.as_str())
                        .unwrap_or_default();
                    assert!(
                        !value.is_empty(),
                        "{}: `{key}` is mandatory for a managed spawn",
                        spec.vendor
                    );
                }
            }
        }
    }
}

/// No principal → no tool face, uniformly. A session without a per-session
/// secret must not be handed an unauthenticated endpoint in ANY dialect.
#[test]
fn a_session_without_a_principal_gets_no_endpoint() {
    assert!(SessionMcpEndpoint::at(URL, SID, "").is_none());
    assert!(SessionMcpEndpoint::at(URL, "", SECRET).is_none());
    assert!(SessionMcpEndpoint::at("", SID, SECRET).is_none());
}
