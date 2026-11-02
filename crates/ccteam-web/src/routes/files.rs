//! #219: project-contained, read-only files and sandboxed HTML previews.
//! Preview capabilities carry the minting identity, never a web bearer. Even
//! cookieless requests from an opaque sandbox pass the ordinary project ACL.

use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Component, Path as FsPath, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{auth::Identity, state::AppState};

const MAX_PATHS: usize = 20_000;
const MAX_TEXT: u64 = 2 * 1024 * 1024;
const MAX_ASSET: u64 = 16 * 1024 * 1024;
const PREVIEW_TTL: Duration = Duration::from_secs(3600);
const PREVIEW_CSP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' http: https: data: blob:; style-src 'unsafe-inline' http: https: data:; img-src http: https: data: blob:; font-src http: https: data:; media-src http: https: data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";

const MARKDOWN_CSP: &str = "sandbox; default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src http: https:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewKind {
    Html,
    Markdown,
}

fn preview_kind(path: &FsPath) -> Option<PreviewKind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "html" | "htm" => Some(PreviewKind::Html),
        "md" | "markdown" => Some(PreviewKind::Markdown),
        _ => None,
    }
}

struct PreviewGrant {
    slug: String,
    directory: PathBuf,
    kind: PreviewKind,
    identity: Identity,
    expires: Instant,
}

#[derive(Default)]
pub(crate) struct PreviewRegistry(Mutex<HashMap<String, PreviewGrant>>);

impl PreviewRegistry {
    fn mint(
        &self,
        slug: String,
        identity: Identity,
        directory: PathBuf,
        kind: PreviewKind,
    ) -> Result<String, StatusCode> {
        let mut grants = self.0.lock().unwrap_or_else(|e| e.into_inner());
        grants.retain(|_, grant| grant.expires > Instant::now());
        if grants.len() >= 1024 {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        let mut random = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut random);
        let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
        grants.insert(
            token.clone(),
            PreviewGrant {
                slug,
                directory,
                kind,
                identity,
                expires: Instant::now() + PREVIEW_TTL,
            },
        );
        Ok(token)
    }

    fn principal(&self, slug: &str, token: &str) -> Option<Identity> {
        let grants = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let grant = grants.get(token)?;
        (grant.slug == slug && grant.expires > Instant::now()).then(|| grant.identity.clone())
    }

    fn scope(&self, slug: &str, token: &str) -> Option<(PathBuf, PreviewKind)> {
        let grants = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let grant = grants.get(token)?;
        (grant.slug == slug && grant.expires > Instant::now())
            .then(|| (grant.directory.clone(), grant.kind))
    }
}

/// Only preview-resource GET/HEADs can authenticate with this capability.
/// Normal cookies must not override it (including after a login switch).
pub(crate) fn preview_identity(
    app: &AppState,
    method: &Method,
    path: &str,
) -> Option<Option<Identity>> {
    let rest = path.strip_prefix("/api/v1/projects/")?;
    let (slug, rest) = rest.split_once('/')?;
    let rest = rest.strip_prefix("files/preview/")?;
    let (ticket, _) = rest.split_once('/')?;
    Some(if method == Method::GET || method == Method::HEAD {
        app.file_previews.principal(slug, ticket)
    } else {
        None
    })
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FileTree {
    pub root: String,
    pub paths: Vec<String>,
    pub truncated: bool,
    pub git_status: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FileQuery {
    path: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FileContent {
    pub path: String,
    pub size: u64,
    pub content: Option<String>,
    pub binary: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PreviewRequest {
    pub path: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PreviewLink {
    pub url: String,
    pub expires_in: u64,
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({"error": message.into()}))).into_response()
}

fn private_component(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        ".git"
            | ".ccteam"
            | ".ssh"
            | ".aws"
            | ".gnupg"
            | "node_modules"
            | "target"
            | ".next"
            | ".venv"
            | "__pycache__"
    ) || name == ".env"
        || (name.starts_with(".env.")
            && !name.ends_with(".example")
            && !name.ends_with(".sample")
            && !name.ends_with(".template"))
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || matches!(name.as_str(), "id_rsa" | "id_ed25519" | "credentials.json")
}

fn safe_relative(path: &FsPath) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|c| match c {
            Component::Normal(name) => name
                .to_str()
                .is_some_and(|s| !private_component(s) && !s.contains('\\')),
            _ => false,
        })
}

fn project_root(app: &AppState, slug: &str) -> Result<PathBuf, (StatusCode, &'static str)> {
    if !app.paths.project_state(slug).exists() {
        return Err((StatusCode::NOT_FOUND, "Project not found"));
    }
    let config = ccteam_core::config::load(&app.paths.root).map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Project catalog is unreadable",
        )
    })?;
    if config
        .projects
        .iter()
        .any(|p| p.slug == slug && p.host != "local")
    {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "File browsing is not yet supported for remote projects",
        ));
    }
    app.paths
        .project_dir(slug)
        .canonicalize()
        .map_err(|_| (StatusCode::NOT_FOUND, "Project directory not found"))
}

fn resolve(root: &FsPath, relative: &str) -> Result<PathBuf, StatusCode> {
    if !safe_relative(FsPath::new(relative)) {
        return Err(StatusCode::FORBIDDEN);
    }
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let inside = path.strip_prefix(root).map_err(|_| StatusCode::FORBIDDEN)?;
    if !safe_relative(inside) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(path)
}

fn read_bounded(path: &FsPath, max: u64) -> Result<Vec<u8>, StatusCode> {
    // Check before opening: opening a FIFO can block before File::metadata.
    if !std::fs::metadata(path).is_ok_and(|meta| meta.is_file()) {
        return Err(StatusCode::NOT_FOUND);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| StatusCode::NOT_FOUND)?;
    let meta = file.metadata().map_err(|_| StatusCode::NOT_FOUND)?;
    if !meta.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }
    if meta.len() > max {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if bytes.len() as u64 > max {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    Ok(bytes)
}

fn walk(root: &FsPath) -> std::io::Result<(Vec<String>, bool)> {
    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0;
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries {
            visited += 1;
            if visited > MAX_PATHS {
                paths.sort();
                return Ok((paths, true));
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            // Never traverse symlinks: cycles and out-of-project targets must
            // not make listing unbounded or reveal private path names.
            if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
                continue;
            }
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            if !safe_relative(relative) {
                continue;
            }
            let Some(relative) = relative.to_str() else {
                continue;
            };
            let mut name = relative.replace('\\', "/");
            if kind.is_dir() {
                name.push('/');
                pending.push(path);
            }
            paths.push(name);
        }
    }
    paths.sort();
    Ok((paths, false))
}

async fn git_status(root: &FsPath) -> HashMap<String, String> {
    use tokio::io::AsyncReadExt;
    let mut command = tokio::process::Command::new("git");
    command
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-C"])
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=normal"])
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let read_status = async move {
        let mut child = command.spawn().ok()?;
        let stdout = child.stdout.take()?;
        let mut output = Vec::new();
        stdout
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut output)
            .await
            .ok()?;
        if output.len() > 2 * 1024 * 1024 {
            return None;
        }
        child.wait().await.ok()?.success().then_some(output)
    };
    let Ok(Some(output)) = tokio::time::timeout(Duration::from_secs(3), read_status).await else {
        return HashMap::new();
    };
    let mut result = HashMap::new();
    let mut entries = output.split(|b| *b == 0);
    while let Some(entry) = entries.next() {
        let Ok(entry) = std::str::from_utf8(entry) else {
            continue;
        };
        if entry.len() < 4 {
            continue;
        }
        let code = &entry[..2];
        let path = &entry[3..];
        if code.contains('R') || code.contains('C') {
            entries.next();
        }
        if !safe_relative(FsPath::new(path)) {
            continue;
        }
        let status = if code == "??" {
            "untracked"
        } else if code.contains('U') {
            "modified"
        } else if code.contains('R') {
            "renamed"
        } else if code.contains('D') {
            "deleted"
        } else if code.contains('A') {
            "added"
        } else {
            "modified"
        };
        result.insert(path.to_string(), status.to_string());
    }
    result
}

#[utoipa::path(get, path = "/api/v1/projects/{slug}/files", tag = "projects",
    params(("slug" = String, Path)), responses((status = 200, body = FileTree, description = "Bounded project file tree"), (status = 501, description = "Remote project")))]
pub(crate) async fn handle_files(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> Response {
    let root = match project_root(&app, &slug) {
        Ok(p) => p,
        Err((s, message)) => return error(s, message),
    };
    let walk_root = root.clone();
    let paths = tokio::task::spawn_blocking(move || walk(&walk_root));
    let status = git_status(&root).await;
    match paths.await {
        Ok(Ok((paths, truncated))) => Json(FileTree {
            root: root.to_string_lossy().into_owned(),
            paths,
            truncated,
            git_status: status,
        })
        .into_response(),
        _ => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not list project files",
        ),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{slug}/file", tag = "projects",
    params(("slug" = String, Path), ("path" = String, Query)), responses((status = 200, body = FileContent, description = "UTF-8 source or binary metadata"), (status = 413, description = "File exceeds 2 MiB")))]
pub(crate) async fn handle_file(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<FileQuery>,
) -> Response {
    let root = match project_root(&app, &slug) {
        Ok(p) => p,
        Err((s, message)) => return error(s, message),
    };
    let path = match resolve(&root, &query.path) {
        Ok(p) => p,
        Err(s) => return error(s, "File is unavailable or outside the browsable project"),
    };
    let result = tokio::task::spawn_blocking(move || read_bounded(&path, MAX_TEXT)).await;
    match result {
        Ok(Ok(bytes)) => {
            let size = bytes.len() as u64;
            let content = if bytes.contains(&0) {
                None
            } else {
                String::from_utf8(bytes).ok()
            };
            Json(FileContent {
                path: query.path,
                size,
                binary: content.is_none(),
                content,
            })
            .into_response()
        }
        Ok(Err(s)) => error(s, "File is unavailable or exceeds the 2 MiB source limit"),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "Could not read file"),
    }
}

#[utoipa::path(post, path = "/api/v1/projects/{slug}/files/preview", tag = "projects",
    params(("slug" = String, Path)), request_body = PreviewRequest,
    responses((status = 200, body = PreviewLink, description = "One-hour read-only preview capability")))]
pub(crate) async fn handle_mint_preview(
    State(app): State<AppState>,
    Path(slug): Path<String>,
    Extension(identity): Extension<Identity>,
    Json(body): Json<PreviewRequest>,
) -> Response {
    let root = match project_root(&app, &slug) {
        Ok(p) => p,
        Err((s, message)) => return error(s, message),
    };
    let path = match resolve(&root, &body.path) {
        Ok(path) if path.is_file() && preview_kind(&path).is_some() => path,
        _ => return error(StatusCode::NOT_FOUND, "Preview document not found"),
    };
    let kind = preview_kind(&path).expect("preview kind checked above");
    let directory = path.parent().unwrap_or(&root).to_path_buf();
    let ticket = match app
        .file_previews
        .mint(slug.clone(), identity, directory, kind)
    {
        Ok(t) => t,
        Err(s) => return error(s, "Too many open previews"),
    };
    // URL path encoding is required for spaces, #, ?, Unicode, and filenames
    // that might otherwise escape the capability's project resource route.
    let encoded = body
        .path
        .split('/')
        .map(|part| {
            part.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/");
    Json(PreviewLink {
        url: format!("/api/v1/projects/{slug}/files/preview/{ticket}/{encoded}"),
        expires_in: PREVIEW_TTL.as_secs(),
    })
    .into_response()
}

#[utoipa::path(get, path = "/api/v1/projects/{slug}/files/preview/{ticket}/{*path}", tag = "projects",
    params(("slug" = String, Path), ("ticket" = String, Path), ("path" = String, Path)),
    responses((status = 200, description = "Sandboxed preview or relative asset")))]
pub(crate) async fn handle_preview(
    State(app): State<AppState>,
    Path((slug, ticket, relative)): Path<(String, String, String)>,
) -> Response {
    let root = match project_root(&app, &slug) {
        Ok(p) => p,
        Err((s, message)) => return error(s, message),
    };
    let path = match resolve(&root, &relative) {
        Ok(p) => p,
        Err(s) => return error(s, "Preview resource unavailable"),
    };
    let Some((directory, grant_kind)) = app.file_previews.scope(&slug, &ticket) else {
        return error(StatusCode::UNAUTHORIZED, "Preview expired");
    };
    // A page (and any third-party script it includes) can read its ticket.
    // It grants only web assets below that page's directory, never arbitrary
    // Rust, configuration or other project source. Markdown grants additionally
    // allow rendered Markdown links; HTML grants retain their web-assets scope.
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let markdown = preview_kind(&path) == Some(PreviewKind::Markdown);
    if !path.starts_with(directory)
        || (markdown && grant_kind != PreviewKind::Markdown)
        || !(markdown
            || matches!(
                extension.as_str(),
                "html"
                    | "htm"
                    | "css"
                    | "js"
                    | "mjs"
                    | "json"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "webp"
                    | "svg"
                    | "ico"
                    | "woff"
                    | "woff2"
                    | "ttf"
                    | "otf"
                    | "wasm"
                    | "mp4"
                    | "webm"
                    | "mp3"
                    | "ogg"
            ))
    {
        return error(
            StatusCode::FORBIDDEN,
            "Resource is outside this document preview's asset scope",
        );
    }
    let mime = if markdown {
        "text/html".to_string()
    } else {
        mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string()
    };
    let result = tokio::task::spawn_blocking(move || {
        let bytes = read_bounded(&path, if markdown { MAX_TEXT } else { MAX_ASSET })?;
        if !markdown {
            return Ok(bytes);
        }
        let source = std::str::from_utf8(&bytes).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
        Ok(render_markdown_document(
            source,
            &path.file_name().unwrap_or_default().to_string_lossy(),
        )
        .into_bytes())
    })
    .await;
    let mut response = match result {
        Ok(Ok(bytes)) => (
            [(header::CONTENT_TYPE, format!("{mime}; charset=utf-8"))],
            bytes,
        )
            .into_response(),
        Ok(Err(s)) => error(
            s,
            if markdown {
                "Markdown preview unavailable: expected UTF-8 text up to 2 MiB"
            } else {
                "Preview resource unavailable or exceeds 16 MiB"
            },
        ),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "Could not read preview"),
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(if markdown { MARKDOWN_CSP } else { PREVIEW_CSP }),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // Opaque sandbox module scripts use CORS without cookies. The random
    // resource path is their ONLY credential; no global API CORS is opened.
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

fn markdown_url_is_safe(url: &str) -> bool {
    let normalized: String = url
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    if normalized.contains('\\') {
        return false;
    }
    let first_segment = normalized.split(['/', '#', '?']).next().unwrap_or_default();
    match first_segment.split_once(':') {
        Some((scheme, _)) => matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto" | "tel"
        ),
        None => true,
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Filter Markdown parser events before rendering: raw HTML stays visible as
/// text, links retain a small protocol set, and no scripts are enabled. The
/// response-level sandbox also holds when the document is opened separately.
fn render_markdown_document(source: &str, title: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser, Tag, TagEnd};
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM;
    let mut events: Vec<_> = Parser::new_ext(source, options)
        .map(|event| match event {
            Event::Html(html) | Event::InlineHtml(html) => Event::Text(html),
            Event::Start(mut tag) => {
                if let Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } = &mut tag {
                    if !markdown_url_is_safe(dest_url) {
                        *dest_url = "#".into();
                    }
                }
                Event::Start(tag)
            }
            other => other,
        })
        .collect();
    let mut heading_counts = HashMap::<String, usize>::new();
    let mut used_heading_ids = HashSet::new();
    for index in 0..events.len() {
        if !matches!(events[index], Event::Start(Tag::Heading { .. })) {
            continue;
        }
        let text: String = events[index + 1..]
            .iter()
            .take_while(|e| !matches!(e, Event::End(TagEnd::Heading(_))))
            .filter_map(|e| match e {
                Event::Text(s) | Event::Code(s) => Some(s.as_ref()),
                _ => None,
            })
            .collect();
        let slug: String = text
            .chars()
            .flat_map(char::to_lowercase)
            .filter_map(|c| {
                if c.is_alphanumeric() || matches!(c, '-' | '_') {
                    Some(c)
                } else if c.is_whitespace() {
                    Some('-')
                } else {
                    None
                }
            })
            .collect();
        let base = if slug.trim_matches('-').is_empty() {
            "section"
        } else {
            slug.trim_matches('-')
        };
        let count = heading_counts.entry(base.to_string()).or_default();
        let mut id = if *count == 0 {
            base.to_string()
        } else {
            format!("{base}-{count}")
        };
        while !used_heading_ids.insert(id.clone()) {
            *count += 1;
            id = format!("{base}-{count}");
        }
        *count += 1;
        if let Event::Start(Tag::Heading { id: heading_id, .. }) = &mut events[index] {
            *heading_id = Some(id.into());
        }
    }
    let mut body = String::new();
    html::push_html(&mut body, events.into_iter());
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\"><title>{}</title><style>{MARKDOWN_STYLE}</style></head><body><main>{body}</main></body></html>", escape_html(title))
}

const MARKDOWN_STYLE: &str = r#"
:root{color-scheme:light dark;--bg:#fff;--fg:#24292f;--muted:#57606a;--border:#d0d7de;--soft:#f6f8fa;--link:#0969da}
@media(prefers-color-scheme:dark){:root{--bg:#111318;--fg:#e6edf3;--muted:#9da7b3;--border:#30363d;--soft:#1c2129;--link:#79b8ff}}
*{box-sizing:border-box}html{scroll-behavior:smooth}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.75 system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;overflow-wrap:anywhere}
main{max-width:960px;margin:auto;padding:clamp(20px,4vw,48px)}h1,h2,h3,h4,h5,h6{line-height:1.3;margin:1.5em 0 .65em;scroll-margin-top:20px}h1{font-size:2em}h2{font-size:1.5em;border-bottom:1px solid var(--border);padding-bottom:.3em}main>:first-child{margin-top:0}
p,ul,ol,pre,blockquote,table{margin:0 0 1em}li>p{margin:.3em 0}a{color:var(--link);text-underline-offset:3px}a:hover{text-decoration-thickness:2px}img{max-width:100%;height:auto}blockquote{margin-left:0;border-left:3px solid var(--border);padding:0 1em;color:var(--muted)}
code{font: .88em/1.65 ui-monospace,SFMono-Regular,Consolas,monospace;background:var(--soft);padding:.15em .35em;border-radius:4px}pre{overflow:auto;padding:16px;border:1px solid var(--border);border-radius:8px;background:var(--soft)}pre code{padding:0;background:none;white-space:pre}
table{display:block;max-width:100%;overflow:auto;border-collapse:collapse}th,td{padding:7px 12px;border:1px solid var(--border)}th{background:var(--soft);text-align:left}input[type=checkbox]{margin-right:.5em;accent-color:var(--link)}hr{border:0;border-top:1px solid var(--border);margin:1.5em 0}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_escapes_html_titles_and_unsafe_urls_and_assigns_unique_anchors() {
        let html = render_markdown_document("# Same\n\n# Same\n\n# Same-1\n\n[x](jav&#x61;script:alert%281%29)\n\n<svg onload=alert(1)>\n", "<&>.md");
        assert!(html.contains("<title>&lt;&amp;&gt;.md</title>"));
        for id in ["same", "same-1", "same-1-1"] {
            assert_eq!(html.matches(&format!("id=\"{id}\"")).count(), 1);
        }
        assert!(!html.contains("<svg") && !html.contains("href=\"javascript:"));
        for url in [
            "javascript:alert(1)",
            "java\nscript:alert(1)",
            "data:text/html,evil",
            "file:///etc/passwd",
        ] {
            assert!(!markdown_url_is_safe(url));
        }
        for url in [
            "next.md",
            "assets/a.svg",
            "#details",
            "https://example.com/docs",
        ] {
            assert!(markdown_url_is_safe(url));
        }
    }

    #[test]
    fn paths_reject_private_and_traversal_including_symlink_targets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("good.rs"), "source").unwrap();
        std::fs::write(dir.path().join(".env"), "secret").unwrap();
        assert!(resolve(dir.path(), "good.rs").is_ok());
        for path in [
            "../good.rs",
            "/etc/passwd",
            ".env",
            "foo/../good.rs",
            "foo\\bar",
        ] {
            assert!(resolve(dir.path(), path).is_err(), "{path}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join(".env"), dir.path().join("alias.txt"))
                .unwrap();
            assert_eq!(
                resolve(dir.path(), "alias.txt").unwrap_err(),
                StatusCode::FORBIDDEN
            );
        }
    }

    #[test]
    fn expired_and_cross_project_preview_capabilities_fail_closed() {
        let registry = PreviewRegistry::default();
        let token = registry
            .mint(
                "one".into(),
                Identity::admin(),
                PathBuf::from("/project/web"),
                PreviewKind::Html,
            )
            .unwrap();
        assert!(registry.principal("one", &token).is_some());
        assert!(registry.principal("two", &token).is_none());
        registry.0.lock().unwrap().get_mut(&token).unwrap().expires = Instant::now();
        assert!(registry.principal("one", &token).is_none());
    }
}
