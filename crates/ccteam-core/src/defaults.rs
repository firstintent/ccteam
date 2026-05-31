//! Cross-crate primitive defaults.
//!
//! Keep values here when they are consumed outside the engine modules
//! that originally introduced them.

/// V0.6.8 F195 — per-turn watchdog default (seconds).
///
/// 90s leaves enough headroom for normal multi-tool turns to finish
/// without triggering the "still working" notice, while keeping the
/// silent-stall feedback loop tight enough that a stuck Stop hook /
/// tail loop / claude hang doesn't go unsurfaced for minutes.
pub const DEFAULT_TURN_TIMEOUT_SECS: u32 = 90;
