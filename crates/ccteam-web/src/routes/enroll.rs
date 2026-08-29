//! Enrollment credentials over REST: the copy-button an EXTERNAL agent pastes.
//!
//! `ccteam_core::enroll` owns the credential itself (mint / list / verify /
//! revoke, wire form `ccteam-enroll:<id>:<secret>`). This module is the console
//! face of it, and it exists for one user motion: an operator on the web UI
//! wants a snippet a vendor CLI **on another machine** can paste and
//! immediately reach this daemon's `POST /mcp` with.
//!
//! Two decisions make that safe and make it work:
//!
//! - **Safety is SCOPE, not secrecy.** A snippet is meant to be handed out, so
//!   the sane default is [`EnrollScope::Project`]: pasted anywhere it still
//!   reaches exactly one workspace. There is deliberately no "universal"
//!   snippet: [`EnrollScope::User`] names no project, so a caller holding it
//!   must still name one per call and can only reach its own.
//! - **The URL is the host the operator is browsing.** See
//!   [`mcp_url_for_request`] — an external machine cannot reach `127.0.0.1`.
//!
//! **The route shape IS the ACL.** A project-scoped mint lives at
//! `POST /api/v1/projects/{slug}/enroll`, so `auth::project_acl_layer` — the
//! single REST choke point — gates it by path shape, before this module runs.
//! The alternative (one flat route naming the project in its BODY) type-checks
//! and even behaves correctly, but it needs a hand-written
//! `can_see_project` call that the NEXT project-addressed enroll route will not
//! inherit: a patch at the symptom instead of the layer (CLAUDE.md §三/§五).
//! The user-scoped mint stays flat because it addresses no project, and
//! list/revoke stay flat because they are owner-scoped, not project-addressed
//! (`Identity::can_see_owner`, checked here — that axis has no layer).
//!
//! Secrets: `GET` NEVER returns one (not even truncated — see
//! [`EnrollView::bearer_prefix`]); `POST` returns the full bearer exactly once,
//! because that is the only moment it can be copied. Same posture as the
//! satellite join token.
//!
//! **Ensure (idempotent) vs mint.** A human clicking a copy button wants a new
//! snippet every time; a PROGRAM that boots repeatedly (the DSH plugin, a CI
//! runner) wants *the* credential for its slot, or it leaves a new record on
//! disk per restart. Both routes therefore take `ensure` + `label` in the body
//! and go through `ccteam_core::enroll::ensure_in`, keyed by
//! (identity, scope, label) — the same function the machine credential and the
//! Hosts "register MCP" button use.
//!
//! That flag lives in the BODY rather than in a new `PUT /api/v1/enroll/{label}`
//! for three reasons, in order of weight: (a) the scope lives in the ROUTE, so
//! putting ensure in the shared body gives the project-scoped mint the same
//! semantics through the same gate, and the next project-addressed enroll route
//! inherits it — a `{label}` path would cover the flat route only and need a
//! second one later; (b) a label is free text ("rob's laptop") and a path
//! segment is a lossy, escaping-sensitive place to put it; (c) `{label}` cannot
//! even sit next to the existing `DELETE /api/v1/enroll/{id}` — axum's router
//! refuses two different capture names at the same position.
//!
//! An ensure that RESOLVES to an existing record answers `200` with no bearer:
//! the secret is not recoverable, by construction. The caller compares
//! [`EnrollView::bearer_prefix`] against the credential it holds; if it holds
//! none, `rotate: true` mints a replacement and revokes the old one.

use std::path::Path;

use axum::{
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use ccteam_core::enroll::{self, EnrollCredential, EnrollScope};

use crate::auth::Identity;
use crate::state::AppState;

/// One credential as the console sees it. **No secret, in any form.**
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnrollView {
    /// Ops-visible record id (also the middle field of the bearer).
    pub id: String,
    /// `user` (this machine's user) | `project` (pinned to one workspace).
    pub scope: String,
    /// The pinned project slug; `null` for a user-scoped credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// ccteam identity every session this credential creates will belong to.
    pub owner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: String,
    /// `ccteam-enroll:<id>:` — the grep-able HEAD of the bearer, so an operator
    /// can match a record against a config they already pasted. It carries no
    /// secret bytes at all: a truncated secret would be a real (if small) leak
    /// for zero extra usefulness, since the id already identifies the record.
    pub bearer_prefix: String,
}

impl EnrollView {
    fn of(cred: &EnrollCredential) -> Self {
        Self {
            id: cred.id.clone(),
            scope: match cred.scope {
                EnrollScope::User => "user".to_string(),
                EnrollScope::Project { .. } => "project".to_string(),
            },
            project: cred.scope.project().map(str::to_string),
            owner: cred.owner.clone(),
            label: cred.label.clone(),
            created_at: cred.created_at.to_rfc3339(),
            bearer_prefix: format!("{}{}:", enroll::ENROLL_BEARER_PREFIX, cred.id),
        }
    }
}

/// `GET /api/v1/enroll` response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnrollListResponse {
    pub credentials: Vec<EnrollView>,
}

/// Mint body, shared by both mint routes. There is no `scope` discriminator and
/// no `project`: the ROUTE says which scope this is, and the path says which
/// project — so neither can disagree with the gate that authorized the call.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct MintEnrollForm {
    /// Free-text reminder of where the snippet went ("rob's laptop", "ci").
    /// With `ensure` it is the KEY, so it must be non-empty there.
    #[serde(default)]
    pub label: Option<String>,
    /// Get-or-mint instead of mint: idempotent per (caller identity, scope,
    /// `label`). `201` + bearer when this call created the credential, `200`
    /// and NO bearer when it resolved to one that already existed.
    #[serde(default)]
    pub ensure: bool,
    /// With `ensure`: mint a replacement and revoke the old record. For a
    /// caller that lost its secret — the only way back, since a stored secret
    /// is never readable again.
    #[serde(default)]
    pub rotate: bool,
}

/// One vendor's ready-to-paste config, produced by the writer that owns that
/// vendor's shape (see [`render_snippets`]).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnrollSnippet {
    /// `claude` | `codex` | `grok` | `opencode` | `kimi`.
    pub vendor: String,
    /// `json` | `toml` — tells the operator (and the UI) how to merge it.
    pub format: String,
    /// Where that vendor reads it, on the PASTING machine. A display hint, not
    /// a path on this host: the whole point is that the agent is elsewhere.
    pub path: String,
    /// The config text. Merge it into an existing file — every writer behind
    /// this is merge-not-clobber, and so is the paste.
    pub body: String,
}

/// `ensure` response when the credential ALREADY EXISTED (`200`). No bearer and
/// no snippets: the secret was returned once, at mint time, and is not
/// recoverable — so this says *which* record answers to that key and lets the
/// caller check whether the one it holds is the same one.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnsuredEnrollment {
    pub credential: EnrollView,
    /// Always `false` here — a created one answers `201` with the full bearer.
    pub created: bool,
    /// The `POST /mcp` endpoint this credential is used against.
    pub url: String,
    /// See [`MintedEnrollment::insecure_transport`].
    pub insecure_transport: bool,
}

/// `POST /api/v1/enroll` response — the ONE place a secret is returned.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MintedEnrollment {
    pub credential: EnrollView,
    /// `ccteam-enroll:<id>:<secret>`. Shown once; not recoverable from `GET`.
    pub bearer: String,
    /// The `POST /mcp` endpoint the snippets point at.
    pub url: String,
    pub snippets: Vec<EnrollSnippet>,
    /// True when this snippet would carry the credential in CLEAR TEXT: plain
    /// HTTP (no TLS) to a non-loopback host. The daemon computes the fact; the
    /// UI owns the wording.
    ///
    /// (Schema docs land verbatim in `/api/v1/openapi.json`, which the docs-UI
    /// test scans for absolute URLs — so no scheme literals in here.)
    pub insecure_transport: bool,
}

/// 404, never 403: the same shape `project_acl_layer` and `handle_project` use,
/// so a caller cannot probe for the existence of something it may not see.
///
/// (Scope is still never validated here — moving it onto the route removed
/// every "which scope did you mean / where is the project field" branch this
/// module used to own. The one 400 below is about the request's own shape, not
/// about what the caller may reach: see [`bad_request`].)
fn not_found(msg: impl Into<String>) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": msg.into() }))).into_response()
}

/// 400 for a body that cannot mean anything — `ensure` with no key. Unlike a
/// 404 here, it reveals nothing: the caller is being told about ITS OWN
/// request, before any lookup happens.
fn bad_request(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg.into() })),
    )
        .into_response()
}

/// Whether `identity` may see this credential at all. Owner is the axis (a
/// credential belongs to the identity that minted it, and every session it
/// creates inherits that owner), NOT the pinned project: if the project is
/// later removed, its owner must still be able to find and revoke the
/// credential — hiding it would leave a live credential with no way to kill it.
fn can_see(identity: &Identity, cred: &EnrollCredential) -> bool {
    identity.can_see_owner(Some(cred.owner.as_str()))
}

/// `GET /api/v1/enroll` — every credential the caller owns, secrets redacted.
#[utoipa::path(
    get,
    path = "/api/v1/enroll",
    tag = "enroll",
    responses(
        (status = 200, description = "Credentials the caller owns (NO secrets)", body = EnrollListResponse),
    ),
)]
pub(crate) async fn handle_list_enrollments(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    let root = app.paths.root.clone();
    let creds = match tokio::task::spawn_blocking(move || enroll::list_in(&root)).await {
        Ok(c) => c,
        Err(err) => return worker_error(err),
    };
    let credentials = creds
        .iter()
        .filter(|c| can_see(&identity, c))
        .map(EnrollView::of)
        .collect();
    Json(EnrollListResponse { credentials }).into_response()
}

/// `POST /api/v1/enroll` — mint (or, with `ensure`, get-or-mint) a credential
/// for THIS MACHINE'S USER (names no project, so the holder must name one per
/// call and can only reach its own).
///
/// No project in the path or the body ⇒ nothing for the project ACL to gate,
/// and nothing for this handler to check beyond the caller being authenticated:
/// the credential is minted into the caller's OWN owner tag, so it can never
/// widen what that identity already reaches.
#[utoipa::path(
    post,
    path = "/api/v1/enroll",
    tag = "enroll",
    request_body = MintEnrollForm,
    responses(
        (status = 201, description = "Minted (or `ensure` created it); carries the bearer + per-vendor snippets", body = MintedEnrollment),
        (status = 200, description = "`ensure` resolved to an existing credential; NO bearer (not recoverable) — compare `bearer_prefix`, or retry with `rotate`", body = EnsuredEnrollment),
        (status = 400, description = "`ensure` with no label, or `rotate` without `ensure`"),
    ),
)]
pub(crate) async fn handle_mint_enrollment(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    Json(form): Json<MintEnrollForm>,
) -> Response {
    mint_and_render(&app, &identity, EnrollScope::User, &headers, form).await
}

/// `POST /api/v1/projects/{slug}/enroll` — mint a credential pinned to ONE
/// project. This is the copy-button's product: pasted anywhere, it still reaches
/// only this workspace.
///
/// **Authorization happened before this function ran.** The path shape
/// `/api/v1/projects/{slug}/*` is what `auth::project_acl_layer` matches, so a
/// caller who cannot see `slug` never reaches the handler — which is why there
/// is no `can_see_project` call in here, and why the next project-addressed
/// enroll route is covered without anyone remembering to add one. The only
/// check left is resource existence (a 404 about the world, not about
/// permission), exactly as the sibling project routes do it.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{slug}/enroll",
    tag = "enroll",
    params(("slug" = String, Path, description = "Project the credential is pinned to")),
    request_body = MintEnrollForm,
    responses(
        (status = 201, description = "Minted (or `ensure` created it); carries the bearer + per-vendor snippets", body = MintedEnrollment),
        (status = 200, description = "`ensure` resolved to an existing credential; NO bearer (not recoverable)", body = EnsuredEnrollment),
        (status = 400, description = "`ensure` with no label, or `rotate` without `ensure`"),
        (status = 404, description = "Unknown project, or not visible to the caller (the project ACL layer answers first)"),
    ),
)]
pub(crate) async fn handle_mint_project_enrollment(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    AxumPath(slug): AxumPath<String>,
    headers: HeaderMap,
    Json(form): Json<MintEnrollForm>,
) -> Response {
    if !app.paths.project_state(&slug).exists() {
        return not_found(format!("project not found: {slug}"));
    }
    let scope = EnrollScope::Project { slug };
    mint_and_render(&app, &identity, scope, &headers, form).await
}

/// What the blocking half resolved to. `Created` is the only variant that can
/// carry a bearer, and it carries it exactly once.
enum Resolved {
    Created(EnrollCredential, Vec<EnrollSnippet>),
    Reused(EnrollCredential),
}

/// Mint-or-ensure + render, shared by both routes. Takes the scope as a decided
/// fact: whichever route resolved it has already been authorized for it, and
/// the owner is the CALLER's — never a value from the body — so neither mode
/// can reach another identity's pool.
async fn mint_and_render(
    app: &AppState,
    identity: &Identity,
    scope: EnrollScope,
    headers: &HeaderMap,
    form: MintEnrollForm,
) -> Response {
    let label = form
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if form.ensure && label.is_none() {
        return bad_request(
            "ensure needs a non-empty label: it is the key the credential is looked up by",
        );
    }
    if form.rotate && !form.ensure {
        return bad_request(
            "rotate only applies to ensure: a plain mint is already a new credential",
        );
    }
    let url = mcp_url_for_request(headers, app);
    let owner = identity.owner_tag();
    let root = app.paths.root.clone();
    let render_url = url.clone();
    let (ensure, rotate) = (form.ensure, form.rotate);

    let resolved = tokio::task::spawn_blocking(move || -> anyhow::Result<Resolved> {
        let cred = if ensure {
            let ensured = enroll::ensure_in(&root, scope, &owner, label.as_deref(), rotate)?;
            if !ensured.created {
                // Nothing to render: the secret was handed out at mint time and
                // is not recoverable. The id (and its bearer prefix) is the
                // whole answer.
                return Ok(Resolved::Reused(ensured.credential));
            }
            ensured.credential
        } else {
            enroll::mint_in(&root, scope, &owner, label)?
        };
        // The bearer only exists inside the record, so minting has to come
        // first — which means a rendering failure would otherwise leave a
        // live credential nobody was ever shown. Roll it back so a failed
        // mint leaves nothing behind. (After a `rotate` that also means the
        // key is left EMPTY rather than holding an unusable orphan: the old
        // record is gone by the caller's own request, and the retry mints a
        // credential the caller actually receives.)
        match render_snippets(&render_url, &cred.bearer()) {
            Ok(snippets) => Ok(Resolved::Created(cred, snippets)),
            Err(err) => {
                let _ = enroll::revoke_in(&root, &cred.id);
                Err(err)
            }
        }
    })
    .await;

    match resolved {
        Ok(Ok(Resolved::Created(cred, snippets))) => (
            StatusCode::CREATED,
            Json(MintedEnrollment {
                credential: EnrollView::of(&cred),
                bearer: cred.bearer(),
                insecure_transport: is_insecure_transport(&url),
                url,
                snippets,
            }),
        )
            .into_response(),
        Ok(Ok(Resolved::Reused(cred))) => (
            StatusCode::OK,
            Json(EnsuredEnrollment {
                credential: EnrollView::of(&cred),
                created: false,
                insecure_transport: is_insecure_transport(&url),
                url,
            }),
        )
            .into_response(),
        Ok(Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("mint enrollment: {err}")})),
        )
            .into_response(),
        Err(err) => worker_error(err),
    }
}

/// `DELETE /api/v1/enroll/{id}` — revoke. Every process holding this bearer
/// fails closed on its next request; no other credential is disturbed.
#[utoipa::path(
    delete,
    path = "/api/v1/enroll/{id}",
    tag = "enroll",
    params(("id" = String, Path, description = "Credential id (the middle field of the bearer)")),
    responses(
        (status = 200, description = "Revoked; `{ok, id}`", body = serde_json::Value),
        (status = 404, description = "No such credential, or not the caller's"),
    ),
)]
pub(crate) async fn handle_revoke_enrollment(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let root = app.paths.root.clone();
    let lookup_id = id.clone();
    let existing =
        match tokio::task::spawn_blocking(move || enroll::load_in(&root, &lookup_id)).await {
            Ok(c) => c,
            Err(err) => return worker_error(err),
        };
    // Another owner's credential is indistinguishable from a missing one.
    match existing {
        Some(cred) if can_see(&identity, &cred) => {}
        _ => return not_found(format!("no such enrollment credential: {id}")),
    }
    let root = app.paths.root.clone();
    let revoke_id = id.clone();
    match tokio::task::spawn_blocking(move || enroll::revoke_in(&root, &revoke_id)).await {
        Ok(Ok(true)) => Json(json!({"ok": true, "id": id})).into_response(),
        Ok(Ok(false)) => not_found(format!("no such enrollment credential: {id}")),
        Ok(Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("revoke: {err}")})),
        )
            .into_response(),
        Err(err) => worker_error(err),
    }
}

fn worker_error(err: tokio::task::JoinError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": format!("worker: {err}")})),
    )
        .into_response()
}

// =====================================================================
// URL — the host the operator is actually browsing
// =====================================================================

/// The `POST /mcp` URL to put in a snippet.
///
/// **The request's own `Host` header wins**, and that is deliberate:
/// `resolve_mcp_http_url` answers a different question — "how does a vendor
/// process *on this machine* reach the daemon" — so it maps a wildcard bind to
/// `127.0.0.1`. That is right for a managed session and useless in a snippet
/// the operator is about to paste on another box. The host in the address bar
/// is by construction one that already reaches this daemon from where the
/// operator sits, so it is the only defensible answer. The recorded bind stays
/// as the fallback for a request that carried no usable `Host` (HTTP/1.0, an
/// odd proxy) — a wrong-but-local URL beats no URL.
fn mcp_url_for_request(headers: &HeaderMap, app: &AppState) -> String {
    match usable_host(headers) {
        Some(host) => format!("{}://{host}/mcp", forwarded_scheme(headers)),
        None => {
            ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&app.paths.root.join("run"))
        }
    }
}

/// The `Host` header, if it can be spliced into a URL as-is. Anything with
/// structure of its own (`/`, `?`, `#`, `@`, whitespace) is rejected rather
/// than embedded: the result is copied into a config file by a human, so a
/// crafted Host must not be able to redirect the credential somewhere else.
fn usable_host(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::HOST)?
        .to_str()
        .ok()?
        .trim()
        .to_string();
    if raw.is_empty() || raw.len() > 255 {
        return None;
    }
    if raw
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '?' | '#' | '@' | '"' | '\''))
    {
        return None;
    }
    Some(raw)
}

/// `https` when a reverse proxy in front of the daemon says the browser spoke
/// TLS; the daemon itself only serves plain HTTP, so without that header the
/// honest answer is `http` (and [`is_insecure_transport`] then warns).
fn forwarded_scheme(headers: &HeaderMap) -> &'static str {
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .unwrap_or_default();
    if proto.eq_ignore_ascii_case("https") {
        "https"
    } else {
        "http"
    }
}

/// Whether pasting this snippet puts a credential on the wire in clear text:
/// plain `http://` to anything but loopback. Loopback is exempt because the
/// bytes never leave the machine.
fn is_insecure_transport(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    // Drop the port. An IPv6 authority is bracketed and full of colons, so the
    // bracket — not the last colon — is what ends the host there.
    let host = match authority.rfind(']') {
        Some(end) => &authority[..=end],
        None => authority
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(authority),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    !matches!(host, "127.0.0.1" | "localhost" | "::1")
}

// =====================================================================
// Snippets — rendered BY the writers that own each vendor's shape
// =====================================================================

/// Signature shared by all five vendor writers in
/// `ccteam_core::mcp_register`.
type ConfigWriter = fn(&Path, &str, &str) -> anyhow::Result<()>;

/// vendor · format · where the PASTING machine keeps it · temp filename ·
/// the writer that owns the shape.
///
/// `pi` is absent on purpose: its tool surface is
/// `ManagedSessionBridge`, so ccteam writes it no config at all and a
/// hand-started `pi` has no ccteam tools by design (red line: vendor config
/// footprint = only our own MCP registration, and pi has none).
const DIALECTS: &[(&str, &str, &str, &str, ConfigWriter)] = &[
    (
        "claude",
        "json",
        "~/.claude.json",
        "claude.json",
        ccteam_core::mcp_register::install_mcp_into,
    ),
    (
        "codex",
        "toml",
        "~/.codex/config.toml",
        "codex.toml",
        ccteam_core::mcp_register::install_codex_mcp_into,
    ),
    (
        "grok",
        "toml",
        "~/.grok/config.toml",
        "grok.toml",
        ccteam_core::mcp_register::install_grok_mcp_into,
    ),
    (
        "opencode",
        "json",
        "~/.config/opencode/opencode.json",
        "opencode.json",
        ccteam_core::mcp_register::install_opencode_mcp_into,
    ),
    (
        "kimi",
        "json",
        "~/.kimi-code/mcp.json",
        "kimi.json",
        ccteam_core::mcp_register::install_kimi_mcp_into,
    ),
];

/// Render one snippet per vendor by **running the real writer** into a scratch
/// dir and reading the file back.
///
/// Why not build the JSON/TOML here: `ccteam_core::mcp_register` is the single
/// owner of every vendor's config shape, and it expresses that shape by
/// writing the file — `type` vs none, `headers` vs Codex's `http_headers`,
/// OpenCode's `type:"remote"` under `mcp.<name>`, Grok's `enabled`. A
/// hand-rolled second copy of those shapes drifts silently and the operator
/// finds out only when the pasted config does not work (the SPA had exactly
/// that bug: a `transport:"http"` key Claude does not read). Going through the
/// writer makes the snippet byte-identical to what `ccteam config` would have
/// written, forever, with no test to keep two copies honest.
///
/// The scratch dir is fresh + removed; each writer starts from a missing file,
/// so the body is the MINIMAL entry (merge-not-clobber has nothing to merge).
fn render_snippets(url: &str, bearer: &str) -> anyhow::Result<Vec<EnrollSnippet>> {
    render_snippets_in(&std::env::temp_dir(), url, bearer)
}

/// [`render_snippets`] with an explicit scratch parent. The seam exists so the
/// "no credential-bearing file survives" property can be asserted on a private
/// directory, rather than by counting entries in a shared `/tmp` that every
/// other concurrent test also writes to.
fn render_snippets_in(
    scratch_parent: &Path,
    url: &str,
    bearer: &str,
) -> anyhow::Result<Vec<EnrollSnippet>> {
    let dir = scratch_dir_in(scratch_parent);
    std::fs::create_dir_all(&dir)?;
    let rendered = (|| -> anyhow::Result<Vec<EnrollSnippet>> {
        let mut out = Vec::with_capacity(DIALECTS.len());
        for (vendor, format, path_hint, filename, write) in DIALECTS {
            let file = dir.join(filename);
            write(&file, url, bearer)?;
            let body = std::fs::read_to_string(&file)?;
            out.push(EnrollSnippet {
                vendor: (*vendor).to_string(),
                format: (*format).to_string(),
                path: (*path_hint).to_string(),
                body: body.trim_end().to_string(),
            });
        }
        Ok(out)
    })();
    // Never leave a credential-bearing file behind, on success or failure.
    let _ = std::fs::remove_dir_all(&dir);
    rendered
}

/// A private, per-call scratch directory. Unique by pid + nanos + a counter so
/// two concurrent mints cannot collide or read each other's half-written file
/// (the clock alone is not enough: two threads can read the same nanosecond).
fn scratch_dir_in(parent: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        "ccteam-enroll-snippet-{}-{nanos}-{seq}",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn usable_host_rejects_anything_with_url_structure() {
        assert_eq!(
            usable_host(&headers(&[("host", "box.example:7331")])).as_deref(),
            Some("box.example:7331")
        );
        // A crafted Host must not be able to point the snippet elsewhere.
        assert!(usable_host(&headers(&[("host", "evil.example/mcp?x=1")])).is_none());
        assert!(usable_host(&headers(&[("host", "user@evil.example")])).is_none());
        assert!(usable_host(&headers(&[("host", "a b")])).is_none());
        assert!(usable_host(&HeaderMap::new()).is_none());
    }

    #[test]
    fn forwarded_scheme_only_upgrades_on_an_explicit_https_hop() {
        assert_eq!(forwarded_scheme(&HeaderMap::new()), "http");
        assert_eq!(
            forwarded_scheme(&headers(&[("x-forwarded-proto", "https")])),
            "https"
        );
        // First hop wins (proxy chains append).
        assert_eq!(
            forwarded_scheme(&headers(&[("x-forwarded-proto", "https, http")])),
            "https"
        );
        assert_eq!(
            forwarded_scheme(&headers(&[("x-forwarded-proto", "http")])),
            "http"
        );
    }

    #[test]
    fn insecure_transport_is_plain_http_off_loopback() {
        assert!(is_insecure_transport("http://box.example:7331/mcp"));
        assert!(is_insecure_transport("http://192.168.1.5:7331/mcp"));
        assert!(!is_insecure_transport("https://box.example/mcp"));
        assert!(!is_insecure_transport("http://127.0.0.1:7331/mcp"));
        assert!(!is_insecure_transport("http://localhost:7331/mcp"));
        assert!(!is_insecure_transport("http://[::1]:7331/mcp"));
        // Portless forms too — an IPv6 authority is full of colons, so the
        // port split must not eat half the address.
        assert!(!is_insecure_transport("http://[::1]/mcp"));
        assert!(!is_insecure_transport("http://localhost/mcp"));
        assert!(is_insecure_transport("http://[2001:db8::5]/mcp"));
    }

    /// The snippets must be what ccteam itself would write — asserted by
    /// feeding each one back to that vendor's own "is ccteam registered?"
    /// predicate, which parses the dialect and checks the credential family.
    #[test]
    fn every_snippet_round_trips_through_its_vendors_registration_check() {
        use ccteam_core::mcp_register as reg;
        let bearer = "ccteam-enroll:deadbeefdeadbeef:sekritsekrit";
        let snippets = render_snippets("http://box.example:7331/mcp", bearer).unwrap();
        assert_eq!(
            snippets.len(),
            5,
            "five vendors take a config: {snippets:?}"
        );
        let tmp = tempfile::TempDir::new().unwrap();
        for s in &snippets {
            assert!(s.body.contains(bearer), "{} lost the bearer", s.vendor);
            assert!(
                s.body.contains("box.example:7331"),
                "{} lost the url",
                s.vendor
            );
            let file = tmp.path().join(format!("{}.{}", s.vendor, s.format));
            std::fs::write(&file, &s.body).unwrap();
            let accepted = match s.vendor.as_str() {
                "claude" => reg::claude_mcp_registered(&file),
                "codex" => reg::codex_mcp_registered(&file),
                "grok" => reg::grok_mcp_registered(&file),
                "opencode" => reg::opencode_mcp_registered(&file),
                "kimi" => reg::kimi_mcp_registered(&file),
                other => panic!("unknown dialect {other}"),
            };
            assert!(
                accepted,
                "{} snippet is not a valid ccteam registration:\n{}",
                s.vendor, s.body
            );
        }
    }

    /// Rendering writes real config files (that is the point — the writers own
    /// the shapes), so every one of them carries the credential. None may
    /// outlive the call.
    #[test]
    fn render_snippets_leaves_no_credential_bearing_file_behind() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bearer = "ccteam-enroll:aaaaaaaaaaaaaaaa:sekrit";
        let out = render_snippets_in(tmp.path(), "http://x:1/mcp", bearer).unwrap();
        assert_eq!(out.len(), DIALECTS.len());
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        assert!(
            leftovers.is_empty(),
            "scratch dir must be removed, found {leftovers:?}"
        );
    }

    #[test]
    fn a_view_never_carries_secret_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("home");
        let cred = enroll::mint_in(
            &root,
            EnrollScope::Project {
                slug: "alpha".into(),
            },
            "user:web-api",
            Some("laptop".into()),
        )
        .unwrap();
        let body = serde_json::to_string(&EnrollView::of(&cred)).unwrap();
        assert!(!body.contains(&cred.secret), "secret leaked: {body}");
        assert!(body.contains(&cred.id));
        assert!(body.contains("\"scope\":\"project\""));
        assert!(body.contains("\"project\":\"alpha\""));
        // The prefix stops at the separator — no secret bytes at all.
        assert!(body.contains(&format!("ccteam-enroll:{}:", cred.id)));
    }
}
