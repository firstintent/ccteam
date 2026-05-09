//! V0.2.2 F38 — xterm 256-color ANSI palette.
//!
//! Standard mapping documented at
//! <https://en.wikipedia.org/wiki/ANSI_escape_code#8-bit>:
//!
//! - 0..=15: 16 base colors (xterm system palette).
//! - 16..=231: 6×6×6 RGB cube. For each component, the level table is
//!   `[0, 95, 135, 175, 215, 255]`.
//! - 232..=255: 24-step grayscale ramp `8, 18, 28, ..., 238`.
//!
//! Built once as a `const` so callers pay zero per-frame cost.

use image::Rgb;

/// Color component levels for the 6×6×6 cube (xterm convention).
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// 16 base ANSI colors as xterm renders them.
///
/// Indices 0..=7 are the standard normal-intensity colors;
/// indices 8..=15 are the bright variants.
const ANSI_BASE_16: [Rgb<u8>; 16] = [
    Rgb([0x00, 0x00, 0x00]), // 0  black
    Rgb([0x80, 0x00, 0x00]), // 1  red
    Rgb([0x00, 0x80, 0x00]), // 2  green
    Rgb([0x80, 0x80, 0x00]), // 3  yellow
    Rgb([0x00, 0x00, 0x80]), // 4  blue
    Rgb([0x80, 0x00, 0x80]), // 5  magenta
    Rgb([0x00, 0x80, 0x80]), // 6  cyan
    Rgb([0xc0, 0xc0, 0xc0]), // 7  white (light gray)
    Rgb([0x80, 0x80, 0x80]), // 8  bright black (dark gray)
    Rgb([0xff, 0x00, 0x00]), // 9  bright red
    Rgb([0x00, 0xff, 0x00]), // 10 bright green
    Rgb([0xff, 0xff, 0x00]), // 11 bright yellow
    Rgb([0x00, 0x00, 0xff]), // 12 bright blue
    Rgb([0xff, 0x00, 0xff]), // 13 bright magenta
    Rgb([0x00, 0xff, 0xff]), // 14 bright cyan
    Rgb([0xff, 0xff, 0xff]), // 15 bright white
];

/// Build the 256-entry xterm palette at compile time.
const fn build_ansi_256() -> [Rgb<u8>; 256] {
    let mut out = [Rgb([0u8, 0, 0]); 256];

    // 0..=15 — base 16.
    let mut i = 0;
    while i < 16 {
        out[i] = ANSI_BASE_16[i];
        i += 1;
    }

    // 16..=231 — 6×6×6 cube. Index formula: 16 + 36*r + 6*g + b.
    let mut r = 0;
    while r < 6 {
        let mut g = 0;
        while g < 6 {
            let mut b = 0;
            while b < 6 {
                let idx = 16 + 36 * r + 6 * g + b;
                out[idx] = Rgb([CUBE_LEVELS[r], CUBE_LEVELS[g], CUBE_LEVELS[b]]);
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }

    // 232..=255 — 24-step grayscale, step = 10, base = 8.
    let mut k = 0;
    while k < 24 {
        let v = 8 + 10 * (k as u8);
        out[232 + k] = Rgb([v, v, v]);
        k += 1;
    }

    out
}

/// Full 256-color xterm palette. Callers index by the `u8` from
/// `vt100::Color::Idx`.
pub const ANSI_256: [Rgb<u8>; 256] = build_ansi_256();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base16_spot_check() {
        // Black, red, white, bright_white — anchors of the base 16 block.
        assert_eq!(ANSI_256[0], Rgb([0x00, 0x00, 0x00]));
        assert_eq!(ANSI_256[1], Rgb([0x80, 0x00, 0x00]));
        assert_eq!(ANSI_256[7], Rgb([0xc0, 0xc0, 0xc0]));
        assert_eq!(ANSI_256[15], Rgb([0xff, 0xff, 0xff]));
    }

    #[test]
    fn rgb_cube_spot_check() {
        // 16 = (0,0,0) of the cube.
        assert_eq!(ANSI_256[16], Rgb([0, 0, 0]));
        // 231 = (5,5,5) of the cube — pure cube-white at 0xff.
        assert_eq!(ANSI_256[231], Rgb([0xff, 0xff, 0xff]));
        // 196 = 16 + 36*5 + 6*0 + 0 = pure cube-red.
        assert_eq!(ANSI_256[196], Rgb([0xff, 0, 0]));
        // 21 = 16 + 36*0 + 6*0 + 5 = pure cube-blue.
        assert_eq!(ANSI_256[21], Rgb([0, 0, 0xff]));
    }

    #[test]
    fn grayscale_ramp_spot_check() {
        // Index 232 = base 8. Index 255 = 8 + 10*23 = 238.
        assert_eq!(ANSI_256[232], Rgb([8, 8, 8]));
        assert_eq!(ANSI_256[233], Rgb([18, 18, 18]));
        assert_eq!(ANSI_256[255], Rgb([238, 238, 238]));
    }

    #[test]
    fn full_table_is_256_entries() {
        // Compile-time guard, but assert anyway so a refactor that
        // accidentally drops an index trips loud.
        assert_eq!(ANSI_256.len(), 256);
    }
}
