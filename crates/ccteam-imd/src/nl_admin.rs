//! `@ccteam <NL admin>` command parser.
//!
//! V0.6 keeps this lexical — no LLM dispatch. The full free-form NL
//! interpretation lands later (V0.7); for now we accept a small set
//! of verbs that the dashboard and humans can drive deterministically.

use serde::{Deserialize, Serialize};

/// Parsed admin command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdminCmd {
    /// `@ccteam status` — print daemon + bot status.
    Status,
    /// `@ccteam list` — list registered bots.
    List,
    /// `@ccteam pause <slug>/<role>` — write `signals/drain.signal`.
    Pause {
        /// Project slug.
        slug: String,
        /// Role name.
        role: String,
    },
    /// `@ccteam resume <slug>/<role>` — remove drain signal.
    Resume {
        /// Project slug.
        slug: String,
        /// Role name.
        role: String,
    },
    /// `@ccteam stop <slug>/<role>` — write `signals/shutdown.signal`.
    Stop {
        /// Project slug.
        slug: String,
        /// Role name.
        role: String,
    },
    /// `@ccteam help` — print help.
    Help,
    /// Unparsable.
    Unknown {
        /// Original input for echoback.
        raw: String,
    },
}

/// Parse `<verb> [<args...>]`.
pub fn parse(input: &str) -> AdminCmd {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return AdminCmd::Help;
    }
    let mut parts = trimmed.split_whitespace();
    let verb = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest: Vec<&str> = parts.collect();
    match verb.as_str() {
        "status" => AdminCmd::Status,
        "list" | "ls" => AdminCmd::List,
        "help" | "?" => AdminCmd::Help,
        "pause" | "resume" | "stop" => {
            let target = rest.first().copied().unwrap_or("");
            let (slug, role) = match target.split_once('/') {
                Some((s, r)) => (s.to_string(), r.to_string()),
                None => return AdminCmd::Unknown { raw: input.into() },
            };
            match verb.as_str() {
                "pause" => AdminCmd::Pause { slug, role },
                "resume" => AdminCmd::Resume { slug, role },
                "stop" => AdminCmd::Stop { slug, role },
                _ => unreachable!(),
            }
        }
        _ => AdminCmd::Unknown { raw: input.into() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status() {
        assert_eq!(parse("status"), AdminCmd::Status);
        assert_eq!(parse("  STATUS "), AdminCmd::Status);
    }

    #[test]
    fn parses_pause_slug_role() {
        assert_eq!(
            parse("pause dev-foo/lead"),
            AdminCmd::Pause {
                slug: "dev-foo".into(),
                role: "lead".into()
            }
        );
    }

    #[test]
    fn pause_without_slash_is_unknown() {
        assert!(matches!(parse("pause lead"), AdminCmd::Unknown { .. }));
    }

    #[test]
    fn empty_input_help() {
        assert_eq!(parse(""), AdminCmd::Help);
        assert_eq!(parse("help"), AdminCmd::Help);
        assert_eq!(parse("?"), AdminCmd::Help);
    }

    #[test]
    fn list_alias_ls() {
        assert_eq!(parse("list"), AdminCmd::List);
        assert_eq!(parse("ls"), AdminCmd::List);
    }
}
