# Third-party licenses

ccteam itself is MIT-licensed (see `Cargo.toml::workspace.package.license`).
This file lists the third-party assets that ship inside the ccteam binary
or repository, with their respective licenses.

## Rust source code (adapted)

### OpenAI Codex — daemon lifecycle

- **Adapted into**: `crates/ccteam-core/src/daemon.rs` (pid-record ownership,
  operation lock, SIGTERM stop ladder, readiness probe) and
  `crates/ccteam-cli/src/{daemon_cli,legacy_takeover}.rs` (the `ccteam daemon`
  lifecycle command surface), introduced in v0.9.7.
- **Derived from**: `codex-rs/app-server-daemon` — chiefly
  `src/backend/pid.rs` (`PidRecord`, setsid detach, `process_start_time`
  PID-reuse guard, stop poll + grace ladder), `src/lib.rs` (lifecycle
  command/status enums, `daemon.lock` operation lock) and `src/client.rs`
  (control-socket `initialize` readiness probe). The code was rewritten for
  ccteam's MCP Unix socket and paths, not copied verbatim.
- **Upstream**: <https://github.com/openai/codex>
- **License**: [Apache License, Version 2.0](https://github.com/openai/codex/blob/main/LICENSE)
- **Copyright**: OpenAI Codex, Copyright 2025 OpenAI (see the upstream `NOTICE`).

The Apache-2.0 license permits use, modification, and redistribution under
the terms of the license, including in MIT-licensed projects, provided the
attribution and license notice above are retained.

## Fonts

### JetBrains Mono Regular

- **Vendored at**: `crates/ccteam-core/assets/fonts/JetBrainsMono-Regular.ttf`
- **Used by**: V0.2.2 F38 — terminal screenshot pipeline. Baked into the
  `ccteam` binary via `include_bytes!` so screenshot rendering has zero
  system-font dependencies. Users can override at runtime by setting
  the `CCTEAM_SCREENSHOT_FONT_TTF` environment variable.
- **Upstream**: <https://github.com/JetBrains/JetBrainsMono>
- **Version vendored**: v2.304 (released 2023-01-14)
- **License**: [SIL Open Font License, Version 1.1](https://scripts.sil.org/OFL)
- **Copyright**: Copyright 2020 The JetBrains Mono Project Authors
  (<https://github.com/JetBrains/JetBrainsMono>)

The OFL permits embedding and redistribution of the font as part of
software, including binary forms; it forbids sale of the font as a
standalone product. Derivative works of the font (which ccteam does
not produce) must use a Reserved Font Name distinct from the original.

## JavaScript

### htmx

- **Vendored at**: `crates/ccteam-web/assets/htmx.min.js`
- **Used by**: V0.3 M5.1 — read-only web dashboard. Baked into the
  `ccteam` binary via `include_bytes!`; served by `GET /assets/htmx.min.js`
  so the web UI is self-contained (no npm / Vite / external CDN).
- **Upstream**: <https://htmx.org>
- **Version vendored**: 2.0.4
- **License**: [BSD 2-Clause "Simplified" License](https://github.com/bigskysoftware/htmx/blob/master/LICENSE)
- **Copyright**: Copyright (c) 2020, Big Sky Software

### htmx-ext-sse

- **Vendored at**: `crates/ccteam-web/assets/htmx-ext-sse.js`
- **Used by**: V0.3 M5.2 — Server-Sent Events extension for htmx.
  Loaded by `GET /assets/htmx-ext-sse.js` after the htmx core lib so
  the SSE extension can register its handlers. Powers the live event
  feed on the dashboard / project detail pages.
- **Upstream**: <https://github.com/bigskysoftware/htmx-extensions/tree/main/src/sse>
- **Version vendored**: 2.2.2
- **License**: [BSD 2-Clause "Simplified" License](https://github.com/bigskysoftware/htmx-extensions/blob/main/LICENSE)
- **Copyright**: Copyright (c) 2020, Big Sky Software

### @xterm/xterm

- **Vendored at**: `crates/ccteam-web/assets/xterm.js` and
  `crates/ccteam-web/assets/xterm.css`
- **Used by**: V0.3 pane snapshot rendering. The web UI fetches raw
  ANSI bytes from `GET /api/<slug>/pane-snapshot.ansi` and renders them
  client-side with xterm.js, while keeping the legacy PNG screenshot
  endpoint as a fallback.
- **Upstream**: <https://xtermjs.org/>
- **Version vendored**: 6.0.0
- **License**: [MIT License](https://github.com/xtermjs/xterm.js/blob/master/LICENSE)
- **Copyright**: Copyright (c) 2017-2019, The xterm.js authors;
  Copyright (c) 2014-2016, SourceLair Private Company;
  Copyright (c) 2012-2013, Christopher Jeffrey
