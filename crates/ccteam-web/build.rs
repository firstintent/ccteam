//! V0.3.2 F53 — build script that drives `npm run build` for the
//! ccteam-web SPA bundle.
//!
//! Behavior matrix:
//!
//! | feature `web-bundle` | `CCTEAM_SKIP_WEB_BUILD` | action                          |
//! |----------------------|-------------------------|---------------------------------|
//! | off                  | any                     | write placeholder `dist/`       |
//! | on                   | `1`                     | write placeholder `dist/`       |
//! | on                   | unset / `0`             | `npm install` + `npm run build` |
//!
//! The placeholder branch always produces a valid `web/dist/index.html`
//! so `rust-embed::RustEmbed` (used by `routes/assets.rs`) has a
//! non-empty folder to embed regardless of whether the operator has
//! `npm` installed. Cargo `cargo:rerun-if-*` directives keep
//! incremental builds cheap: edits inside `web/src/` or to the build
//! config retrigger this script; nothing else does.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Re-run when the SPA source / config changes. We deliberately
    // scope this to the files that affect the build output — node_modules
    // churn or playwright runs do not.
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/vite.config.ts");
    println!("cargo:rerun-if-changed=web/index.html");
    println!("cargo:rerun-if-env-changed=CCTEAM_SKIP_WEB_BUILD");

    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set"));
    let web_dir = crate_dir.join("web");
    let dist_dir = web_dir.join("dist");

    let bundle_on = env::var_os("CARGO_FEATURE_WEB_BUNDLE").is_some();
    let skip = matches!(env::var("CCTEAM_SKIP_WEB_BUILD").as_deref(), Ok("1"));

    if !bundle_on || skip {
        let reason = if !bundle_on {
            "feature `web-bundle` disabled"
        } else {
            "CCTEAM_SKIP_WEB_BUILD=1"
        };
        println!(
            "cargo:warning=ccteam-web: SPA bundle skipped ({reason}); \
             emitting placeholder dist/"
        );
        write_placeholder_dist(&dist_dir);
        return;
    }

    // npm-driven build path.
    if !web_dir.exists() {
        println!(
            "cargo:warning=ccteam-web: {} missing — emitting placeholder dist/",
            web_dir.display()
        );
        write_placeholder_dist(&dist_dir);
        return;
    }

    let node_modules = web_dir.join("node_modules");
    if !node_modules.exists() {
        eprintln!(
            "ccteam-web: running `npm install` in {}",
            web_dir.display()
        );
        let status = Command::new("npm")
            .args(["install", "--no-audit", "--no-fund", "--silent"])
            .current_dir(&web_dir)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                println!(
                    "cargo:warning=ccteam-web: `npm install` failed with status {s}; \
                     set CCTEAM_SKIP_WEB_BUILD=1 to bypass or install Node.js"
                );
                std::process::exit(1);
            }
            Err(err) => {
                println!(
                    "cargo:warning=ccteam-web: `npm install` could not be spawned: {err}; \
                     set CCTEAM_SKIP_WEB_BUILD=1 to bypass or install Node.js"
                );
                std::process::exit(1);
            }
        }
    }

    eprintln!(
        "ccteam-web: running `npm run build` in {}",
        web_dir.display()
    );
    let status = Command::new("npm")
        .args(["run", "build", "--silent"])
        .current_dir(&web_dir)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            println!(
                "cargo:warning=ccteam-web: `npm run build` failed with status {s}; \
                 fix the SPA build or set CCTEAM_SKIP_WEB_BUILD=1"
            );
            std::process::exit(1);
        }
        Err(err) => {
            println!(
                "cargo:warning=ccteam-web: `npm run build` could not be spawned: {err}; \
                 set CCTEAM_SKIP_WEB_BUILD=1 to bypass"
            );
            std::process::exit(1);
        }
    }

    if !dist_dir.join("index.html").exists() {
        println!(
            "cargo:warning=ccteam-web: npm build finished but {}/index.html is missing",
            dist_dir.display()
        );
        std::process::exit(1);
    }
}

/// Write a minimal valid `dist/index.html` + `.gitkeep` so `rust-embed`
/// can always resolve the `web/dist/` folder, even when the SPA bundle
/// is intentionally skipped. The placeholder body documents how to
/// rebuild with the real bundle.
fn write_placeholder_dist(dist_dir: &Path) {
    fs::create_dir_all(dist_dir)
        .unwrap_or_else(|err| panic!("create {}: {err}", dist_dir.display()));
    let placeholder = "<!doctype html>\n<html><body>\
        ccteam web bundle disabled — rebuild with `--features web-bundle` \
        (and unset `CCTEAM_SKIP_WEB_BUILD`).\
        </body></html>\n";
    fs::write(dist_dir.join("index.html"), placeholder)
        .unwrap_or_else(|err| panic!("write placeholder index.html: {err}"));
    fs::write(dist_dir.join(".gitkeep"), "")
        .unwrap_or_else(|err| panic!("write .gitkeep: {err}"));
}
