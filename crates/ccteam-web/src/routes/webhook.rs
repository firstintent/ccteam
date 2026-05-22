//! V0.6.3 F141 — `POST /webhook/{project}/{token}` ingress.
//!
//! A thin HTTP→file entry point so external systems (CI, GitHub,
//! monitors) can trigger ccteam agents. The webhook is **not** a new
//! `Trigger` variant: it writes the request body (plus selected
//! headers as metadata) into `<project>/.ccteam/webhooks/<ts>-<rand>.json`
//! and an agent consumes it through the existing
//! `trigger: watch:.ccteam/webhooks/`. This keeps the Channel Layer a
//! dumb router with no embedded LLM (CLAUDE.md red line R1).
//!
//! ## Auth — token only, no HMAC
//!
//! The `{token}` path segment is constant-time compared against the
//! per-project secret persisted at `<project>/.ccteam/webhook-token`
//! (64 hex chars, mode 0600 — same shape as `~/.ccteam/web-token`).
//! The secret is generated lazily on the first request to a project
//! that does not yet have one. Because the token rides in the URL path
//! the deployment is expected to terminate HTTPS in front of ccteam;
//! request signing is intentionally left for a future revision.
//!
//! Wrong / missing token → 401 (nothing written). Oversized body →
//! 413. Untrusted payload: it is only ever written to disk — never
//! passed into a spawn argv — so the agent itself `Read`s the file.
//!
//! This route sits OUTSIDE the `auth_layer` bearer gate (it carries
//! its own per-project token) — it is mounted on the stateless router.

use std::path::Path;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use rand::RngCore;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

use crate::state::AppState;

/// Maximum accepted webhook body size. 256 KiB comfortably fits a
/// GitHub `push` / `pull_request` payload while bounding the on-disk
/// footprint of a hostile sender (CLAUDE.md: treat as untrusted input).
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// Request headers copied into the persisted file's `headers` metadata
/// block. Kept to a small allow-list so a hostile sender cannot bloat
/// the file with arbitrary header spam.
const FORWARDED_HEADERS: &[&str] = &[
    "content-type",
    "user-agent",
    "x-github-event",
    "x-github-delivery",
    "x-gitlab-event",
    "x-event-key",
];

/// Build the `POST /webhook/{project}/{token}` router. The
/// `DefaultBodyLimit` layer turns an oversized body into a 413 before
/// the handler runs.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/webhook/{project}/{token}", post(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

async fn handle(
    State(app): State<AppState>,
    AxumPath((project, token)): AxumPath<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Resolve the project directory. A slug that does not resolve to an
    // existing `.ccteam/` directory is rejected as 401 — we deliberately
    // do not distinguish "no such project" from "bad token" so a probe
    // cannot enumerate registered slugs.
    let ccteam_dir = app.paths.project_ccteam_dir(&project);
    if !ccteam_dir.is_dir() {
        return unauthorized();
    }

    let token_path = app.paths.project_webhook_token(&project);
    let secret = match generate_or_load_secret(&token_path) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(?err, project = %project, "webhook: secret load failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "webhook secret error").into_response();
        }
    };

    // Constant-time compare the path token against the stored secret.
    if !ct_eq(token.as_bytes(), secret.as_bytes()) {
        tracing::warn!(project = %project, "webhook: token mismatch → 401");
        return unauthorized();
    }

    // Valid token → persist the payload. The body is treated as
    // untrusted: it is parsed best-effort as JSON for a tidy nested
    // shape, falling back to a raw-string field when it is not JSON.
    let payload: Value = serde_json::from_slice(&body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()));

    let received_at = chrono::Utc::now();
    let record = json!({
        "received_at": received_at.to_rfc3339(),
        "project": project,
        "headers": forwarded_headers(&headers),
        "payload": payload,
    });

    let webhooks_dir = app.paths.project_webhooks_dir(&project);
    match write_record(&webhooks_dir, &received_at, &record) {
        Ok(path) => {
            tracing::info!(
                project = %project,
                file = %path.display(),
                bytes = body.len(),
                "webhook: payload accepted"
            );
            (
                StatusCode::ACCEPTED,
                Json(json!({"ok": true, "file": path.to_string_lossy()})),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(?err, project = %project, "webhook: write failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "webhook write error").into_response()
        }
    }
}

/// 401 plain-text response. Used for both bad-token and unknown-project
/// so a prober cannot tell the two apart.
fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
}

/// Constant-time byte-slice equality (length-safe). `subtle`'s `ct_eq`
/// short-circuits only on differing lengths, which does not leak
/// secret content — what matters for the token-guess threat model.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Pick the allow-listed request headers into a JSON object.
fn forwarded_headers(headers: &HeaderMap) -> Value {
    let mut map = serde_json::Map::new();
    for name in FORWARDED_HEADERS {
        if let Some(val) = headers.get(*name) {
            if let Ok(s) = val.to_str() {
                map.insert((*name).to_string(), Value::String(s.to_string()));
            }
        }
    }
    Value::Object(map)
}

/// Generate-or-load the per-project webhook secret. Mirrors
/// `token::generate_or_load_token` (the `~/.ccteam/web-token` flow):
/// 32 random bytes hex-encoded, file created mode 0600 on Unix via
/// `create_new` so a racing writer cannot be clobbered.
pub fn generate_or_load_secret(path: &Path) -> std::io::Result<String> {
    if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        let trimmed = raw.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
        // Empty file (interrupted write) — fall through to regenerate.
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    let hex = hex_encode(&buf);

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(hex.as_bytes())?;
            f.flush().ok();
            Ok(hex)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            // A concurrent request won the create race — read theirs.
            Ok(std::fs::read_to_string(path)?.trim().to_string())
        }
        Err(err) => Err(err),
    }
}

/// Lowercase hex encoding (no extra crate).
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Atomically write the webhook record to
/// `<webhooks_dir>/<ts>-<rand>.json` (`.tmp` + rename). Returns the
/// final path. The timestamp prefix sorts files by arrival; the random
/// suffix avoids collisions within the same second.
fn write_record(
    webhooks_dir: &Path,
    received_at: &chrono::DateTime<chrono::Utc>,
    record: &Value,
) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(webhooks_dir)?;
    let stamp = received_at
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .replace(':', "");
    let mut rand_bytes = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut rand_bytes);
    let suffix = hex_encode(&rand_bytes);
    let file = webhooks_dir.join(format!("{stamp}-{suffix}.json"));
    let tmp = webhooks_dir.join(format!(".{stamp}-{suffix}.json.tmp"));

    let body = serde_json::to_vec_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &file)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn router_compiles() {
        let _: axum::Router<crate::state::AppState> = super::router();
    }

    #[test]
    fn ct_eq_matches_and_rejects() {
        assert!(ct_eq(b"deadbeef", b"deadbeef"));
        assert!(!ct_eq(b"deadbeef", b"deadbeee"));
        assert!(!ct_eq(b"dead", b"deadbeef"));
    }

    #[test]
    fn generate_secret_is_64_hex_chars_and_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".ccteam").join("webhook-token");
        let first = generate_or_load_secret(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        let second = generate_or_load_secret(&path).unwrap();
        assert_eq!(first, second, "load is idempotent");
    }

    #[cfg(unix)]
    #[test]
    fn generated_secret_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("webhook-token");
        generate_or_load_secret(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn write_record_creates_json_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("webhooks");
        let now = chrono::Utc::now();
        let rec = json!({"payload": {"x": 1}});
        let path = write_record(&dir, &now, &rec).unwrap();
        assert!(path.exists());
        assert_eq!(path.extension().unwrap(), "json");
        let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back["payload"]["x"], 1);
    }

    #[test]
    fn forwarded_headers_keeps_only_allow_list() {
        let mut h = HeaderMap::new();
        h.insert("x-github-event", "push".parse().unwrap());
        h.insert("x-secret-internal", "leak".parse().unwrap());
        let v = forwarded_headers(&h);
        assert_eq!(v["x-github-event"], "push");
        assert!(v.get("x-secret-internal").is_none());
    }
}
