//! V0.6.0 Wave 2 F114 — scientist / mathematician / philosopher nickname
//! pool used by `ccteam-creator` when minting bot handles for new chat
//! workflows.
//!
//! Pool sourced from Codex's `references/codex/codex-rs/core/src/agent/
//! agent_names.txt` (101 names spanning antiquity → 20th century STEM +
//! philosophy). Mirrored verbatim here so ccteam doesn't depend on the
//! external `codex-rs` checkout at runtime — `references/` is gitignored
//! and only present on the maintainer machine.
//!
//! ## Selection policy
//!
//! [`pick_unused_bot_name`] walks the pool in declaration order and
//! returns the first name not present in `existing`. This is
//! deterministic — useful for tests + reproducible bot-handle
//! assignment across a single `ccteam-creator` invocation. If the pool
//! is exhausted the function falls back to `agent_N` (numbered).

/// Scientist / mathematician / philosopher nickname pool. Source:
/// `references/codex/codex-rs/core/src/agent/agent_names.txt` (Codex
/// upstream). Single-token first names only — no "von Neumann"-style
/// two-token names. 101 entries.
pub const SCIENTIST_NAMES: &[&str] = &[
    "Euclid",
    "Archimedes",
    "Ptolemy",
    "Hypatia",
    "Avicenna",
    "Averroes",
    "Aquinas",
    "Copernicus",
    "Kepler",
    "Galileo",
    "Bacon",
    "Descartes",
    "Pascal",
    "Fermat",
    "Huygens",
    "Leibniz",
    "Newton",
    "Halley",
    "Euler",
    "Lagrange",
    "Laplace",
    "Volta",
    "Gauss",
    "Ampere",
    "Faraday",
    "Darwin",
    "Lovelace",
    "Boole",
    "Pasteur",
    "Maxwell",
    "Mendel",
    "Curie",
    "Planck",
    "Tesla",
    "Poincare",
    "Noether",
    "Hilbert",
    "Einstein",
    "Raman",
    "Bohr",
    "Turing",
    "Hubble",
    "Feynman",
    "Franklin",
    "McClintock",
    "Meitner",
    "Herschel",
    "Linnaeus",
    "Wegener",
    "Chandrasekhar",
    "Sagan",
    "Goodall",
    "Carson",
    "Carver",
    "Socrates",
    "Plato",
    "Aristotle",
    "Epicurus",
    "Cicero",
    "Confucius",
    "Mencius",
    "Zeno",
    "Locke",
    "Hume",
    "Kant",
    "Hegel",
    "Kierkegaard",
    "Mill",
    "Nietzsche",
    "Peirce",
    "James",
    "Dewey",
    "Russell",
    "Popper",
    "Sartre",
    "Beauvoir",
    "Arendt",
    "Rawls",
    "Singer",
    "Anscombe",
    "Parfit",
    "Kuhn",
    "Boyle",
    "Hooke",
    "Harvey",
    "Dalton",
    "Ohm",
    "Helmholtz",
    "Gibbs",
    "Lorentz",
    "Schrodinger",
    "Heisenberg",
    "Pauli",
    "Dirac",
    "Bernoulli",
    "Godel",
    "Nash",
    "Banach",
    "Ramanujan",
    "Erdos",
    "Jason",
];

/// Pick the first nickname from [`SCIENTIST_NAMES`] not present in
/// `existing` (case-insensitive comparison so a leading `@helpful_`
/// prefix or differing case in stored handles doesn't cause a collision
/// miss). If the entire pool is occupied, returns `agent_<N>` where
/// `<N>` = `existing.len() + 1`.
///
/// Comparison is case-insensitive but the returned name preserves the
/// canonical PascalCase form from [`SCIENTIST_NAMES`].
pub fn pick_unused_bot_name(existing: &[String]) -> String {
    let taken: std::collections::HashSet<String> =
        existing.iter().map(|s| s.to_ascii_lowercase()).collect();
    for &name in SCIENTIST_NAMES {
        if !taken.contains(&name.to_ascii_lowercase()) {
            return name.to_string();
        }
    }
    format!("agent_{}", existing.len() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_is_nonempty_and_unique() {
        assert!(SCIENTIST_NAMES.len() >= 50);
        let mut seen = std::collections::HashSet::new();
        for &n in SCIENTIST_NAMES {
            assert!(!n.is_empty());
            assert!(seen.insert(n), "duplicate name in pool: {n}");
        }
    }

    #[test]
    fn pick_returns_first_unused() {
        let existing = vec!["Euclid".to_string(), "Archimedes".to_string()];
        // Pool order is Euclid, Archimedes, Ptolemy, ... — Ptolemy is next.
        assert_eq!(pick_unused_bot_name(&existing), "Ptolemy");
    }

    #[test]
    fn pick_is_case_insensitive() {
        let existing = vec!["euclid".to_string(), "ARCHIMEDES".to_string()];
        assert_eq!(pick_unused_bot_name(&existing), "Ptolemy");
    }

    #[test]
    fn pick_falls_back_when_pool_exhausted() {
        let existing: Vec<String> = SCIENTIST_NAMES.iter().map(|s| s.to_string()).collect();
        let n = existing.len();
        assert_eq!(pick_unused_bot_name(&existing), format!("agent_{}", n + 1));
    }

    #[test]
    fn pick_returns_first_name_on_empty() {
        assert_eq!(pick_unused_bot_name(&[]), "Euclid");
    }
}
