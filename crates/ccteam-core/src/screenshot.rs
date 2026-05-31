//! V0.2.2 F38 — terminal screenshot pipeline (vt100 + imageproc DIY).
//!
//! Pipeline (`docs/versions/v0-2-2/prd.md` §7.2.3):
//!
//! ```text
//! tmux capture-pane -e ──▶ vt100::Parser ──▶ cell grid
//!         │                       │             │
//!         │                       ▼             ▼
//!         └──▶ pane dims      Screen      imageproc::drawing
//!                                              │
//!                                              ▼
//!                                       <project>/.ccteam/
//!                                       screenshots/<utc>.png
//! ```
//!
//! **Pure Rust**, no system C deps:
//! - `vt100`        — terminal state machine.
//! - `image`        — `RgbImage` + PNG encode.
//! - `imageproc`    — `draw_filled_rect_mut` / `draw_text_mut`.
//! - `ab_glyph`     — TTF font parsing.
//!
//! Font strategy: vendored JetBrains Mono Regular (OFL) is baked into
//! the binary via `include_bytes!`. The `CCTEAM_SCREENSHOT_FONT_TTF`
//! env var can override at runtime (e.g. for CJK / emoji-covering
//! fallback fonts). `LICENSES.md` carries the OFL notice.
//!
//! **Architecture red line** (CLAUDE.md §3): the ANSI byte stream
//! captured here is for *rendering only*. It MUST NOT feed any phase
//! classification / state-machine logic — `progress.jsonl` is the
//! single source of truth for orchestrator state. This module reads
//! tmux as a black box and emits a PNG; no caller propagates the
//! `vt100::Screen` upward.
//!
//! **Graceful degrade red line** (PRD §7.2.5): rendering NEVER
//! aborts the enclosing path. Every fallible step returns
//! `Ok(None)` + a `tracing::warn!`. The `catch_unwind` boundary
//! around vt100 + imageproc covers theoretical panics so an
//! upstream crate-level bug can't take the orchestrator down.

use std::borrow::Cow;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use anyhow::{Context, Result};
use chrono::Utc;
use image::{Rgb, RgbImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut};
use imageproc::rect::Rect;
use vt100::Parser;

use ccteam_harness::MuxSessionId;

use crate::paths::CcteamPaths;
use crate::state::ProjectState;
use crate::tmux::session_name_for_project;

pub mod ansi_palette;

use ansi_palette::ANSI_256;

/// JetBrains Mono Regular (OFL) baked in at compile time. ~270 KB.
/// See `crates/ccteam-core/assets/fonts/JetBrainsMono-Regular.ttf`.
const VENDORED_TTF: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// Env var: when set to an absolute path, overrides the vendored font
/// at runtime (e.g. swap to a CJK / emoji-covering fallback).
pub const FONT_ENV: &str = "CCTEAM_SCREENSHOT_FONT_TTF";

/// Default pixel padding around the rendered grid.
const PADDING: u32 = 8;

/// Default font scale. Picks ~14 px text — readable but compact for a
/// 200×50 pane (rough budget: 200 * 8.4 + 16 ≈ 1700 px wide).
const FONT_PX: f32 = 14.0;

/// Default fallback bg / fg for cells that have `Color::Default`.
/// Matches a dark terminal theme.
const DEFAULT_BG: Rgb<u8> = Rgb([30, 30, 30]);
const DEFAULT_FG: Rgb<u8> = Rgb([204, 204, 204]);

/// Terminal grid dimensions used when `query_pane_dims` returns None
/// (session missing, tmux malformed output). 80×24 is the
/// historically-safe default.
const FALLBACK_DIMS: (u16, u16) = (24, 80);

/// Outcome of `render_screenshot`. `None` carries no PNG path; `Some`
/// is the absolute path of the file just written. The MCP / smoke
/// surfaces wrap `Option<PathBuf>` into `{ok, path?, reason?}`.
pub type ScreenshotResult = Result<Option<PathBuf>>;

/// Drive an async future to completion from ANY calling context —
/// plain-sync, inside a current-thread tokio runtime (the async MCP
/// tool dispatch reaches `render_screenshot` synchronously while a
/// runtime drives the thread), or inside a multi-thread runtime.
///
/// The future runs on a dedicated scoped OS thread that has no ambient
/// reactor, so `Runtime::block_on` can never collide with a running
/// runtime ("Cannot start a runtime from within a runtime"). `thread::
/// scope` lets the future borrow from the caller's stack — the borrows
/// outlive the joined thread.
fn block_on_isolated<F, T>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = T> + Send,
    T: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("build screenshot driver runtime")?;
                Ok(rt.block_on(fut))
            })
            .join()
            .map_err(|_| anyhow::anyhow!("screenshot driver thread panicked"))?
    })
}

/// V0.2.2 F38 entry point. Capture the active pane of the project's
/// tmux session, render to a PNG under
/// `<project>/.ccteam/screenshots/<utc>.png`, return the path.
///
/// Returns `Ok(None)` for any non-panic failure (session missing, pane
/// query failed, font parse failed, IO failed, unknown mux backend).
/// Each path logs a `tracing::warn!` with reason. Panics from `vt100` /
/// `imageproc` are caught and converted to `Ok(None)`.
///
/// **V0.8 G5** — capture + pane-dims route through the
/// [`ccteam_harness::PaneBackend`] trait (`ccteam_harness::terminal_from_env()`) so the
/// configured backend (`CCTEAM_MUX_BACKEND=tmux|rmux`) is honored
/// instead of hard-calling tmux. Under the tmux backend (the opt-out,
/// `CCTEAM_MUX_BACKEND=tmux`) the behavior is byte-for-byte identical
/// (TmuxBackend wraps the same `tmux capture-pane -e` / `display-message`
/// calls).
///
/// **rmux ANSI gap** — under `CCTEAM_MUX_BACKEND=rmux`,
/// `PaneBackend::capture(.., with_ansi=true)` currently returns rendered
/// PLAIN TEXT (rmux's `PaneSnapshot` is a parsed cell grid; no public
/// byte-level capture-pane shim exists yet). The PNG still renders the
/// text, just without color/attribute fidelity. Cell-grid→ANSI
/// re-serialization belongs in `ccteam-harness::rmux_backend` — see
/// `TODO(V0.9-rmux-ansi-capture)` there. We accept degraded screenshots
/// under rmux for V0.8: degraded-but-working beats silently-broken.
///
/// **Runtime note** — this sync fn drives the async trait via
/// [`block_on_isolated`], which runs the backend calls on a dedicated
/// scoped thread with its own runtime. It is therefore safe from ANY
/// caller: plain-sync (CLI), inside a current-thread runtime (the async
/// MCP tool dispatch), inside a multi-thread runtime, or a
/// `spawn_blocking` worker. No `spawn_blocking` wrapper is required at
/// call sites.
pub fn render_screenshot(
    paths: &CcteamPaths,
    slug: &str,
    sid: Option<&str>,
    lines: usize,
) -> ScreenshotResult {
    let session_name = match sid {
        Some(sid) => session_name_for_project_session(paths, slug, sid),
        None => session_name_for_project(paths, slug),
    };

    // Select the configured mux backend. A garbage CCTEAM_MUX_BACKEND
    // value errors — per the graceful-degrade red line (module doc),
    // rendering NEVER aborts the enclosing path, so map Err → Ok(None).
    // NB: rmux's `terminal_from_env()` lazily connects a daemon per call
    // (the documented no-cache policy); under rmux each screenshot pays
    // that connect cost. Acceptable for the screenshot surface —
    // flagged for any future hot-path follow-up.
    let backend = match ccteam_harness::terminal_from_env() {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!("screenshot: mux backend selection failed: {err:#}");
            return Ok(None);
        }
    };
    let id = MuxSessionId::new(session_name.clone());

    // Drive capture + pane-dims on a dedicated thread with its own
    // current-thread runtime (see [`block_on_isolated`]). Safe from ANY
    // caller — plain-sync CLI, the async MCP tool dispatch, or web's
    // spawn_blocking — because the spawned thread has no ambient reactor
    // for `block_on` to collide with. (A prior revision built the
    // runtime inline and panicked "Cannot start a runtime from within a
    // runtime" the moment the async MCP screenshot handler reached here.)
    let (capture_res, dims_res) = match block_on_isolated(async {
        let cap = backend.capture(&id, lines, true).await;
        let dims = backend.pane_dims(&id).await;
        (cap, dims)
    }) {
        Ok(pair) => pair,
        Err(err) => {
            tracing::warn!("screenshot: backend driver thread failed: {err:#}");
            return Ok(None);
        }
    };

    // 1. capture pane output (ANSI escapes preserved on tmux; plain
    //    text under rmux — see rmux ANSI gap above).
    let ansi_bytes = match capture_res {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => {
            tracing::warn!(
                "screenshot: capture returned no output for slug `{slug}` \
                 session `{session_name}` (session missing or backend failed)"
            );
            return Ok(None);
        }
        Err(err) => {
            tracing::warn!(
                "screenshot: capture failed for slug `{slug}` \
                 session `{session_name}`: {err:#}"
            );
            return Ok(None);
        }
    };

    // 2. pane dims (rows × cols) — fall back to 80×24 when query fails.
    let (rows, cols) = match dims_res {
        Ok(Some((r, c))) => (r as usize, c as usize),
        _ => (FALLBACK_DIMS.0 as usize, FALLBACK_DIMS.1 as usize),
    };

    // 3. font bytes — env override > vendored fallback.
    let ttf_bytes: Cow<[u8]> = match std::env::var(FONT_ENV) {
        Ok(p) => match std::fs::read(&p) {
            Ok(b) => Cow::Owned(b),
            Err(err) => {
                tracing::warn!(
                    "screenshot: {FONT_ENV} = `{p}` is set but unreadable \
                     ({err:#}); falling back to vendored font"
                );
                Cow::Borrowed(VENDORED_TTF)
            }
        },
        Err(_) => Cow::Borrowed(VENDORED_TTF),
    };
    let font = match FontRef::try_from_slice(&ttf_bytes) {
        Ok(f) => f,
        Err(err) => {
            tracing::warn!("screenshot: ttf parse failed: {err:?}");
            return Ok(None);
        }
    };

    // 4. catch_unwind around vt100 parse + imageproc render — these
    //    libs theoretically may panic on adversarial input. Wrap the
    //    whole closure in AssertUnwindSafe because RgbImage / Parser
    //    aren't UnwindSafe by default.
    let render_outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        render_to_png_bytes(&font, &ansi_bytes, rows, cols)
    }));
    let img = match render_outcome {
        Ok(Some(img)) => img,
        Ok(None) => {
            tracing::warn!("screenshot: render returned no image (zero-sized grid?)");
            return Ok(None);
        }
        Err(panic) => {
            tracing::warn!(
                "screenshot: vt100/imageproc render panicked: {}",
                describe_panic(&panic)
            );
            return Ok(None);
        }
    };

    // 5. ensure dir + save (non-panic IO failure → Ok(None)).
    let out = paths.project_screenshot_path(slug, Utc::now());
    if let Some(parent) = out.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                "screenshot: create dir {} failed: {err:#}",
                parent.display()
            );
            return Ok(None);
        }
    }
    if let Err(err) = img.save(&out) {
        tracing::warn!("screenshot: save {} failed: {err:#}", out.display());
        return Ok(None);
    }
    Ok(Some(out))
}

fn session_name_for_project_session(paths: &CcteamPaths, slug: &str, sid: &str) -> String {
    ProjectState::load(&paths.project_state(slug))
        .ok()
        .and_then(|state| {
            state
                .sessions
                .get(sid)
                .map(|record| record.tmux_session.clone())
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| format!("ccteam-{slug}-{sid}"))
}

/// Inner render path — pure compute over the ANSI byte stream + font.
/// No IO. Returns `None` only when the configured grid is zero-sized.
fn render_to_png_bytes(
    font: &FontRef<'_>,
    ansi_bytes: &[u8],
    rows: usize,
    cols: usize,
) -> Option<RgbImage> {
    if rows == 0 || cols == 0 {
        return None;
    }

    // 1. terminal state machine.
    let mut parser = Parser::new(rows as u16, cols as u16, 0);
    parser.process(ansi_bytes);
    let screen = parser.screen();

    // 2. cell metrics — pick a tight integer cell size.
    //    Per-glyph widths in monospace TTFs land within a fraction of
    //    a px of advance_width; we round up so successive cells never
    //    overlap.
    let scale = PxScale::from(FONT_PX);
    let scaled = font.as_scaled(scale);
    // h_advance for 'M' is the canonical monospace width.
    let glyph_id = font.glyph_id('M');
    let cell_w = scaled.h_advance(glyph_id).ceil().max(1.0) as u32;
    // Use full font height (ascent - descent + line_gap) so cells stack
    // without overlap.
    let cell_h = (scaled.ascent() - scaled.descent() + scaled.line_gap())
        .ceil()
        .max(1.0) as u32;

    // 3. allocate canvas.
    let img_w = cols as u32 * cell_w + 2 * PADDING;
    let img_h = rows as u32 * cell_h + 2 * PADDING;
    let mut img = RgbImage::from_pixel(img_w, img_h, DEFAULT_BG);

    // 4. paint each cell.
    for r in 0..rows {
        for c in 0..cols {
            let cell = match screen.cell(r as u16, c as u16) {
                Some(c) => c,
                None => continue,
            };
            let mut bg = vt100_color_to_rgb(cell.bgcolor(), DEFAULT_BG);
            let mut fg = vt100_color_to_rgb(cell.fgcolor(), DEFAULT_FG);
            // vt100 reverse video swaps bg/fg.
            if cell.inverse() {
                std::mem::swap(&mut bg, &mut fg);
            }
            let x = (PADDING + c as u32 * cell_w) as i32;
            let y = (PADDING + r as u32 * cell_h) as i32;
            // Always paint the bg rectangle so non-default colors show.
            draw_filled_rect_mut(&mut img, Rect::at(x, y).of_size(cell_w, cell_h), bg);
            let s = cell.contents();
            if !s.is_empty() && s != " " {
                draw_text_mut(&mut img, fg, x, y, scale, font, &s);
            }
        }
    }

    Some(img)
}

/// Translate a `vt100::Color` into an `image::Rgb<u8>` using the
/// xterm 256-color palette for indexed colors.
pub fn vt100_color_to_rgb(c: vt100::Color, default: Rgb<u8>) -> Rgb<u8> {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => ANSI_256[i as usize],
        vt100::Color::Rgb(r, g, b) => Rgb([r, g, b]),
    }
}

/// Best-effort panic-payload stringifier for `tracing::warn!` output.
fn describe_panic(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

/// Loaded a font once for smoke / diagnostic surfaces. Returns `Ok`
/// only if `FontRef::try_from_slice` succeeds. The reported `path`
/// is `"$CCTEAM_SCREENSHOT_FONT_TTF"` when the env override is in
/// effect, else `"<vendored JetBrainsMono-Regular.ttf>"`.
pub fn probe_font() -> Result<String> {
    let (label, bytes): (String, Cow<[u8]>) = match std::env::var(FONT_ENV) {
        Ok(p) => {
            let body = std::fs::read(&p).with_context(|| format!("read {FONT_ENV}=`{p}`"))?;
            (format!("env {FONT_ENV}=`{p}`"), Cow::Owned(body))
        }
        Err(_) => (
            "<vendored JetBrainsMono-Regular.ttf>".to_string(),
            Cow::Borrowed(VENDORED_TTF),
        ),
    };
    FontRef::try_from_slice(&bytes).with_context(|| format!("parse ttf from {label}"))?;
    Ok(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vt100::Color;

    #[test]
    fn vendored_ttf_parses() {
        // The `include_bytes!` ttf must always produce a valid font —
        // canary for accidental file truncation on commit.
        FontRef::try_from_slice(VENDORED_TTF).expect("vendored ttf parses");
    }

    #[test]
    fn vt100_color_default_returns_caller_default() {
        let dflt = Rgb([1, 2, 3]);
        assert_eq!(vt100_color_to_rgb(Color::Default, dflt), dflt);
    }

    #[test]
    fn vt100_color_idx_uses_ansi256_table() {
        let dflt = Rgb([0, 0, 0]);
        // idx 1 = standard red 0x800000.
        assert_eq!(
            vt100_color_to_rgb(Color::Idx(1), dflt),
            Rgb([0x80, 0x00, 0x00])
        );
        // idx 196 = pure cube-red.
        assert_eq!(vt100_color_to_rgb(Color::Idx(196), dflt), Rgb([0xff, 0, 0]));
        // idx 232 = grayscale base.
        assert_eq!(vt100_color_to_rgb(Color::Idx(232), dflt), Rgb([8, 8, 8]));
    }

    #[test]
    fn vt100_color_rgb_passthrough() {
        assert_eq!(
            vt100_color_to_rgb(Color::Rgb(1, 2, 3), Rgb([9, 9, 9])),
            Rgb([1, 2, 3])
        );
    }

    #[test]
    fn probe_font_uses_vendored_when_env_unset() {
        // SAFETY of remove_var: the test reads back the var via
        // probe_font in the same thread; tests in this file don't
        // touch the env in parallel because they live in `lib::tests`
        // and share the process. To stay safe regardless, we save +
        // restore.
        let prev = std::env::var(FONT_ENV).ok();
        // SAFETY: see CLAUDE.md "易踩的坑" — env-mutating tests live in
        // integration test files. Here we only probe the *vendored*
        // path; we explicitly clear and restore.
        unsafe { std::env::remove_var(FONT_ENV) };
        let label = probe_font().expect("vendored probe ok");
        assert!(label.contains("vendored"), "unexpected label: {label}");
        if let Some(p) = prev {
            unsafe { std::env::set_var(FONT_ENV, p) };
        }
    }

    #[test]
    fn render_to_png_bytes_renders_simple_ansi() {
        // Hand-crafted ANSI: red 'X' on default bg + reset.
        // ESC[31m X ESC[0m
        let bytes = b"\x1b[31mHello\x1b[0m world";
        let font = FontRef::try_from_slice(VENDORED_TTF).unwrap();
        let img = render_to_png_bytes(&font, bytes, 4, 40).expect("renders");
        // Image is non-empty.
        assert!(img.width() > 0);
        assert!(img.height() > 0);
        // Sized like (40 cols * cell_w + 2*PADDING, 4 rows * cell_h + 2*PADDING)
        // Both dims must scale with grid (smoke check, not exact px).
        assert!(img.width() >= 40);
        assert!(img.height() >= 4);
    }

    #[test]
    fn render_to_png_bytes_returns_none_on_zero_dims() {
        let font = FontRef::try_from_slice(VENDORED_TTF).unwrap();
        assert!(render_to_png_bytes(&font, b"", 0, 80).is_none());
        assert!(render_to_png_bytes(&font, b"", 24, 0).is_none());
    }

    #[test]
    fn render_screenshot_returns_none_when_tmux_missing() {
        // Slug for a session that won't exist on the test host. The
        // entire main path must degrade silently to Ok(None).
        let tmp = tempfile::tempdir().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        let result = render_screenshot(
            &paths,
            "this-slug-definitely-doesnt-exist-xyz-123",
            None,
            10,
        )
        .expect("graceful degrade returns Ok");
        assert!(result.is_none());
    }
}
