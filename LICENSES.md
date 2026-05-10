# Third-party licenses

ccteam itself is MIT-licensed (see `Cargo.toml::workspace.package.license`).
This file lists the third-party assets that ship inside the ccteam binary
or repository, with their respective licenses.

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
