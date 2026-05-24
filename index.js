#!/usr/bin/env node
// ccteam dual-host MCP bridge.
//
// Same repo serves as Claude Code plugin AND Codex plugin. This file is the
// `command` in `.mcp.json`; when either host spawns it as an MCP server, it:
//   1. Detects the host (CLAUDE_PLUGIN_ROOT vs CODEX_ACTIVE / PLUGIN_ROOT env).
//   2. Resolves the cached binary at ${PLUGIN_ROOT}/bin/ccteam.
//   3. If absent or version-mismatched against plugin.json (SoT), downloads
//      the per-platform tarball from the matching GitHub Release, extracts
//      the ccteam binary, chmods it, and symlinks it to ~/.local/bin/ccteam
//      so the CLI is also usable from a terminal without install.sh.
//   4. Soft-warns if tmux is missing (chat-mode features need it).
//   5. Execs `ccteam mcp-serve` with CCTEAM_HOST=claude|codex env injected;
//      pipes stdio between host <-> Rust process (stderr passes through to
//      the host for diagnostics).
//
// Design notes:
//   * Zero npm dependencies (require()s only Node.js stdlib).
//   * Tarball layout mirrors install.sh: ccteam-<TAG>-<suffix>/ccteam.
//   * Version SoT = .claude-plugin/plugin.json `version` field. The binary
//     is re-downloaded whenever `ccteam --version` does not contain that
//     string, so bumping plugin.json + tagging a release on GitHub is the
//     only release motion required.
//   * Tarball download follows HTTPS redirect chains (GitHub Releases issue
//     302 to the CDN); falls back gracefully with a clear stderr message
//     on any failure.

'use strict';

const { spawn, execSync, spawnSync } = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');
const https = require('https');
const { URL } = require('url');

const PLUGIN_JSON = path.join(__dirname, '.claude-plugin', 'plugin.json');
const VERSION = require(PLUGIN_JSON).version;
const TAG = `v${VERSION}`;

// --- host detection ----------------------------------------------------------
// Codex sets CODEX_ACTIVE (and possibly PLUGIN_ROOT without CLAUDE_PLUGIN_ROOT);
// Claude Code sets CLAUDE_PLUGIN_ROOT. We default to Claude when ambiguous.
const isCodex =
    !!process.env.CODEX_ACTIVE ||
    (!!process.env.PLUGIN_ROOT && !process.env.CLAUDE_PLUGIN_ROOT);
const HOST = isCodex ? 'codex' : 'claude';
const PLUGIN_ROOT =
    process.env.CLAUDE_PLUGIN_ROOT || process.env.PLUGIN_ROOT || __dirname;
const BIN_DIR = path.join(PLUGIN_ROOT, 'bin');
const BINARY = path.join(BIN_DIR, 'ccteam');

// --- platform target ---------------------------------------------------------
function targetSuffix() {
    const p = os.platform();
    const a = os.arch();
    if (p === 'darwin' && a === 'arm64') return 'macos-arm64';
    if (p === 'darwin' && a === 'x64') return 'macos-x64';
    if (p === 'linux' && a === 'x64') return 'linux-x64';
    throw new Error(
        `unsupported platform ${p}-${a}; supported: linux-x64, macos-arm64, macos-x64. ` +
        `Windows users run ccteam under WSL2 with the linux-x64 binary.`,
    );
}

// --- HTTPS download w/ redirect chain ---------------------------------------
function httpsGet(url, maxRedirects = 5) {
    return new Promise((resolve, reject) => {
        const visit = (u, hops) => {
            if (hops > maxRedirects) {
                return reject(new Error(`too many redirects fetching ${url}`));
            }
            const req = https.get(u, (res) => {
                const status = res.statusCode || 0;
                if (status >= 300 && status < 400 && res.headers.location) {
                    res.resume();
                    const next = new URL(res.headers.location, u).toString();
                    return visit(next, hops + 1);
                }
                if (status !== 200) {
                    res.resume();
                    return reject(
                        new Error(`HTTP ${status} fetching ${u}`),
                    );
                }
                resolve(res);
            });
            req.on('error', reject);
        };
        visit(url, 0);
    });
}

async function downloadAndExtract(url, destDir) {
    fs.mkdirSync(destDir, { recursive: true });
    const res = await httpsGet(url);
    // Pipe straight into `tar -xz -C destDir --strip-components=1` so the
    // archive's ccteam-<TAG>-<suffix>/ wrapper dir is unwrapped in place.
    return new Promise((resolve, reject) => {
        const tar = spawn(
            'tar',
            ['-xz', '-C', destDir, '--strip-components=1'],
            { stdio: ['pipe', 'inherit', 'inherit'] },
        );
        res.pipe(tar.stdin);
        tar.on('error', reject);
        tar.on('exit', (code) => {
            if (code === 0) resolve();
            else reject(new Error(`tar exited ${code}`));
        });
        res.on('error', reject);
    });
}

// --- version alignment -------------------------------------------------------
function needsDownload() {
    if (!fs.existsSync(BINARY)) return true;
    try {
        const out = execSync(`${JSON.stringify(BINARY)} --version`, {
            encoding: 'utf8',
            timeout: 5000,
        }).trim();
        // `ccteam --version` prints e.g. "ccteam 0.6.6"; refresh if mismatched.
        return !out.includes(VERSION);
    } catch (_) {
        return true;
    }
}

// --- CLI symlink so `ccteam start` works from a terminal --------------------
function symlinkCli() {
    const localBin = path.join(os.homedir(), '.local', 'bin');
    try {
        fs.mkdirSync(localBin, { recursive: true });
    } catch (e) {
        console.error(
            `[ccteam] CLI symlink skipped (mkdir ${localBin} failed: ${e.message})`,
        );
        return;
    }
    const link = path.join(localBin, 'ccteam');
    try {
        const st = fs.lstatSync(link);
        if (st.isSymbolicLink() || st.isFile()) fs.unlinkSync(link);
    } catch (_) {
        /* link absent — fine */
    }
    try {
        fs.symlinkSync(BINARY, link);
        console.error(`[ccteam] CLI symlinked: ${link} -> ${BINARY}`);
    } catch (e) {
        console.error(
            `[ccteam] CLI symlink skipped (${e.message}); add ${BIN_DIR} to $PATH manually if you want \`ccteam\` on PATH`,
        );
    }
}

// --- tmux soft-check ---------------------------------------------------------
function checkTmux() {
    const r = spawnSync('sh', ['-c', 'command -v tmux'], { stdio: 'ignore' });
    if (r.status !== 0) {
        console.error(
            '[ccteam] warning: tmux not found on PATH — chat-mode features (long-running IM bots, /ccteam-creator) will be unavailable. Install: `brew install tmux` (macOS) or `apt install tmux` (Debian/Ubuntu).',
        );
    }
}

// --- main --------------------------------------------------------------------
async function main() {
    console.error(`[ccteam] launching inside host=${HOST}, version=${TAG}`);
    if (needsDownload()) {
        const suffix = targetSuffix();
        const asset = `ccteam-${TAG}-${suffix}.tar.gz`;
        const url = `https://github.com/firstintent/ccteam/releases/download/${TAG}/${asset}`;
        console.error(`[ccteam] downloading ${asset} ...`);
        try {
            await downloadAndExtract(url, BIN_DIR);
        } catch (e) {
            console.error(`[ccteam] download failed: ${e.message}`);
            console.error(
                `[ccteam] fallback: install the CLI binary manually with \`curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh\` and re-run.`,
            );
            process.exit(1);
        }
        if (!fs.existsSync(BINARY)) {
            console.error(
                `[ccteam] extraction left no binary at ${BINARY} — archive layout drift?`,
            );
            process.exit(1);
        }
        fs.chmodSync(BINARY, 0o755);
        symlinkCli();
        console.error(`[ccteam] installed binary at ${BINARY}`);
    }
    checkTmux();
    const child = spawn(BINARY, ['mcp-serve'], {
        env: { ...process.env, CCTEAM_HOST: HOST },
        stdio: ['pipe', 'pipe', 'inherit'],
    });
    process.stdin.pipe(child.stdin);
    child.stdout.pipe(process.stdout);
    child.on('exit', (code, signal) => {
        if (signal) {
            console.error(`[ccteam] mcp-serve killed by ${signal}`);
            process.exit(128);
        }
        process.exit(code ?? 0);
    });
    child.on('error', (err) => {
        console.error(`[ccteam] failed to spawn ${BINARY}: ${err.message}`);
        process.exit(1);
    });
}

main().catch((err) => {
    console.error(`[ccteam] fatal: ${err.message}`);
    process.exit(1);
});
