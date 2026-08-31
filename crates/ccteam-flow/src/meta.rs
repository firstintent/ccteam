//! `export const meta = { ... }` extraction, plus the static half of the
//! determinism guard.
//!
//! Two jobs, both done *before* any JS runs:
//!
//! 1. [`extract_meta`] — a workflow must announce itself (name, description,
//!    optional phase outline) so a permission dialog, a run list or a progress
//!    tree can be drawn without executing anything. The official contract
//!    requires the object to be a PURE LITERAL; we enforce that by parsing a
//!    restricted literal grammar in Rust rather than evaluating the text.
//!    Evaluating it in an "empty" JS realm (what the reference runtimes do)
//!    still leaves `Object`, `Array`, `String` reachable, so
//!    `{name: Object.keys(x)[0]}` slips through there. A parser cannot be
//!    fooled that way and it can say *which* construct was illegal.
//!
//! 2. [`assert_deterministic`] — reject scripts that reference wall-clock or
//!    randomness. This is the early, readable half of the guard; the runtime
//!    traps in the JS prelude are the authoritative half (they also catch
//!    `Date["now"]()` and other indirection this scan cannot see).
//!
//! Both run against a *masked* copy of the source in which string, template
//! and comment bodies are blanked to spaces. That is a deliberate improvement
//! over the reference implementations, which scan raw source and therefore
//! reject a perfectly good workflow whose agent prompt happens to contain the
//! words `Math.random()` — very likely in exactly the code-review workflows
//! this runner exists for. Masking preserves byte length, so every offset
//! found in the mask indexes the original source unchanged.

use crate::error::FlowError;
use serde::{Deserialize, Serialize};

/// One entry of `meta.phases`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseMeta {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The declared identity of a workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowMeta {
    pub name: String,
    pub description: String,
    #[serde(rename = "whenToUse", default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseMeta>,
}

/// Message appended to every determinism rejection. Kept in one place so the
/// static scan, the runtime traps and the tests all say the same thing —
/// authors get one sentence to act on, not four phrasings of it.
pub const DETERMINISM_HINT: &str =
    "workflow scripts must be deterministic so a run can resume: pass timestamps and \
     randomness in through `args` instead";

// ───────────────────────────────────────────────────────────────────────────
// masking
// ───────────────────────────────────────────────────────────────────────────

/// Return a copy of `src` where comment bodies, string literals, template
/// text and regex literals are replaced by spaces, preserving byte length.
///
/// Template *substitutions* (`${ ... }`) stay visible: they are executable
/// code and must be scanned like any other expression.
pub(crate) fn mask_non_code(src: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Code,
        Line,
        Block,
        Str(char),
        Template,
        Regex,
    }

    let mut out = String::with_capacity(src.len());
    let bytes: Vec<(usize, char)> = src.char_indices().collect();
    let mut mode = Mode::Code;
    // Brace depth inside a `${}` substitution, per open template. Empty when
    // we are not inside a template substitution.
    let mut template_stack: Vec<usize> = Vec::new();
    // Last non-whitespace code character emitted; decides whether `/` starts a
    // regex literal or is a division operator (the standard heuristic).
    let mut prev_sig: Option<char> = None;
    let mut i = 0usize;

    // Push `ch` verbatim (it is code).
    macro_rules! keep {
        ($ch:expr) => {{
            let c: char = $ch;
            out.push(c);
            if !c.is_whitespace() {
                prev_sig = Some(c);
            }
        }};
    }
    // Blank `ch` but keep its byte width so offsets stay aligned.
    macro_rules! blank {
        ($ch:expr) => {{
            let c: char = $ch;
            for _ in 0..c.len_utf8() {
                out.push(' ');
            }
        }};
    }

    while i < bytes.len() {
        let (_, ch) = bytes[i];
        let next = bytes.get(i + 1).map(|(_, c)| *c);
        match mode {
            Mode::Code => {
                if ch == '/' && next == Some('/') {
                    mode = Mode::Line;
                    blank!(ch);
                } else if ch == '/' && next == Some('*') {
                    mode = Mode::Block;
                    blank!(ch);
                } else if ch == '"' || ch == '\'' {
                    mode = Mode::Str(ch);
                    keep!(ch);
                } else if ch == '`' {
                    mode = Mode::Template;
                    keep!(ch);
                } else if ch == '/' && regex_can_start(prev_sig) {
                    mode = Mode::Regex;
                    keep!(ch);
                } else {
                    // A `}` that closes a template substitution returns us to
                    // template text rather than to ordinary code.
                    if ch == '}' {
                        if let Some(depth) = template_stack.last_mut() {
                            if *depth == 0 {
                                template_stack.pop();
                                mode = Mode::Template;
                                keep!(ch);
                                i += 1;
                                continue;
                            }
                            *depth -= 1;
                        }
                    } else if ch == '{' {
                        if let Some(depth) = template_stack.last_mut() {
                            *depth += 1;
                        }
                    }
                    keep!(ch);
                }
            }
            Mode::Line => {
                if ch == '\n' {
                    mode = Mode::Code;
                    out.push('\n');
                } else {
                    blank!(ch);
                }
            }
            Mode::Block => {
                if ch == '*' && next == Some('/') {
                    mode = Mode::Code;
                    out.push_str("  ");
                    i += 2;
                    continue;
                }
                // Newlines survive so line numbers in diagnostics stay honest.
                if ch == '\n' {
                    out.push('\n');
                } else {
                    blank!(ch);
                }
            }
            Mode::Str(q) => {
                if ch == '\\' {
                    blank!(ch);
                    if let Some((_, esc)) = bytes.get(i + 1) {
                        blank!(*esc);
                        i += 2;
                        continue;
                    }
                } else if ch == q {
                    mode = Mode::Code;
                    keep!(ch);
                } else if ch == '\n' {
                    // Unterminated string: recover at the newline rather than
                    // swallowing the rest of the file.
                    mode = Mode::Code;
                    out.push('\n');
                } else {
                    blank!(ch);
                }
            }
            Mode::Template => {
                if ch == '\\' {
                    blank!(ch);
                    if let Some((_, esc)) = bytes.get(i + 1) {
                        blank!(*esc);
                        i += 2;
                        continue;
                    }
                } else if ch == '$' && next == Some('{') {
                    template_stack.push(0);
                    mode = Mode::Code;
                    out.push_str("${");
                    prev_sig = Some('{');
                    i += 2;
                    continue;
                } else if ch == '`' {
                    mode = Mode::Code;
                    keep!(ch);
                } else if ch == '\n' {
                    out.push('\n');
                } else {
                    blank!(ch);
                }
            }
            Mode::Regex => {
                if ch == '\\' {
                    blank!(ch);
                    if let Some((_, esc)) = bytes.get(i + 1) {
                        blank!(*esc);
                        i += 2;
                        continue;
                    }
                } else if ch == '/' {
                    mode = Mode::Code;
                    keep!(ch);
                } else if ch == '\n' {
                    // Regex literals cannot span lines; recover.
                    mode = Mode::Code;
                    out.push('\n');
                } else {
                    blank!(ch);
                }
            }
        }
        i += 1;
    }

    debug_assert_eq!(out.len(), src.len(), "mask must preserve byte offsets");
    out
}

/// After these characters a `/` is a division operator, not a regex opener.
fn regex_can_start(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(c) => {
            !(c.is_alphanumeric() || c == '_' || c == '$' || c == ')' || c == ']' || c == '`')
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// determinism scan
// ───────────────────────────────────────────────────────────────────────────

/// Reject a script that reaches for wall-clock or randomness.
///
/// Best-effort by construction: it sees `Date.now()` but not
/// `globalThis["Da" + "te"].now()`. The prelude's runtime traps are what
/// actually guarantee the property; this exists so the common mistake is
/// reported instantly, with the file never having run.
pub fn assert_deterministic(source: &str) -> Result<(), FlowError> {
    let masked = mask_non_code(source);
    let banned = [
        (member_use(&masked, "Date", "now"), "Date.now()"),
        (member_use(&masked, "Math", "random"), "Math.random()"),
        (argless_new_date(&masked), "argless `new Date()`"),
        (
            member_use(&masked, "Intl", "DateTimeFormat"),
            "Intl.DateTimeFormat",
        ),
    ];
    for (hit, what) in banned {
        if hit {
            return Err(FlowError::Determinism(format!(
                "{what} is unavailable in workflow scripts — {DETERMINISM_HINT}"
            )));
        }
    }
    Ok(())
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Byte offsets where `ident` appears as a whole identifier.
fn ident_positions(hay: &str, ident: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(ident) {
        let at = from + rel;
        let before_ok = hay[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident_char(c));
        let after_ok = hay[at + ident.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident_char(c));
        if before_ok && after_ok {
            out.push(at);
        }
        from = at + ident.len();
    }
    out
}

fn skip_ws(hay: &str, mut at: usize) -> usize {
    while let Some(c) = hay[at..].chars().next() {
        if c.is_whitespace() {
            at += c.len_utf8();
        } else {
            break;
        }
    }
    at
}

/// `obj . member` anywhere (whitespace tolerated, as in the masked source).
fn member_use(masked: &str, obj: &str, member: &str) -> bool {
    ident_positions(masked, obj).into_iter().any(|at| {
        let after = skip_ws(masked, at + obj.len());
        if !masked[after..].starts_with('.') {
            return false;
        }
        let m = skip_ws(masked, after + 1);
        masked[m..].starts_with(member)
            && masked[m + member.len()..]
                .chars()
                .next()
                .is_none_or(|c| !is_ident_char(c))
    })
}

/// `new Date()` with no arguments — the "give me now" constructor.
/// `new Date(2020, 0, 1)` stays legal: it is a pure function of its inputs.
fn argless_new_date(masked: &str) -> bool {
    ident_positions(masked, "new").into_iter().any(|at| {
        let d = skip_ws(masked, at + 3);
        if !masked[d..].starts_with("Date") {
            return false;
        }
        if masked[d + 4..].chars().next().is_some_and(is_ident_char) {
            return false;
        }
        let p = skip_ws(masked, d + 4);
        if !masked[p..].starts_with('(') {
            // `new Date` with no parens is also the now-constructor.
            return true;
        }
        let close = skip_ws(masked, p + 1);
        masked[close..].starts_with(')')
    })
}

// ───────────────────────────────────────────────────────────────────────────
// meta extraction
// ───────────────────────────────────────────────────────────────────────────

/// Parse the leading `export const meta = { ... }` block.
pub fn extract_meta(source: &str) -> Result<WorkflowMeta, FlowError> {
    let masked = mask_non_code(source);
    let eq = find_meta_decl(&masked).ok_or_else(|| {
        FlowError::Meta(
            "workflow script must begin with `export const meta = { name, description }`"
                .to_string(),
        )
    })?;
    let (start, end) = object_literal_span(&masked, eq)?;
    let value = parse_literal(source, start, end)?;

    let obj = match value {
        serde_json::Value::Object(map) => map,
        _ => {
            return Err(FlowError::Meta(
                "`meta` must be an object literal".to_string(),
            ))
        }
    };

    let name = required_str(&obj, "name")?;
    let description = required_str(&obj, "description")?;
    let when_to_use = match obj.get("whenToUse") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(FlowError::Meta(
                "`meta.whenToUse` must be a string".to_string(),
            ))
        }
    };

    let mut phases = Vec::new();
    match obj.get("phases") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Array(items)) => {
            for (idx, item) in items.iter().enumerate() {
                let entry = item.as_object().ok_or_else(|| {
                    FlowError::Meta(format!(
                        "`meta.phases[{idx}]` must be an object like {{ title, detail }}"
                    ))
                })?;
                let title = entry
                    .get("title")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        FlowError::Meta(format!("`meta.phases[{idx}].title` is required"))
                    })?
                    .to_string();
                let detail = entry
                    .get("detail")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                phases.push(PhaseMeta { title, detail });
            }
        }
        Some(_) => {
            return Err(FlowError::Meta(
                "`meta.phases` must be an array".to_string(),
            ))
        }
    }

    Ok(WorkflowMeta {
        name,
        description,
        when_to_use,
        phases,
    })
}

fn required_str(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, FlowError> {
    match obj.get(key) {
        Some(serde_json::Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(serde_json::Value::String(_)) => {
            Err(FlowError::Meta(format!("`meta.{key}` must not be empty")))
        }
        Some(_) => Err(FlowError::Meta(format!(
            "`meta.{key}` must be a string literal"
        ))),
        None => Err(FlowError::Meta(format!("`meta.{key}` is required"))),
    }
}

/// Byte offset of the `=` that starts the meta literal, searched in masked
/// source so a `const meta =` inside a prompt string cannot match.
fn find_meta_decl(masked: &str) -> Option<usize> {
    for at in ident_positions(masked, "meta") {
        // Walk back over whitespace to the declaring keyword.
        let head = &masked[..at];
        let kw_end = head.trim_end().len();
        let head = &masked[..kw_end];
        let starts_decl = head.ends_with("const") || head.ends_with("let") || head.ends_with("var");
        if !starts_decl {
            continue;
        }
        let after = skip_ws(masked, at + 4);
        if masked[after..].starts_with('=') && !masked[after..].starts_with("==") {
            return Some(after);
        }
    }
    None
}

/// Balanced-brace span of the object literal that follows `eq`. Runs on the
/// mask, so braces inside strings and comments cannot unbalance it.
fn object_literal_span(masked: &str, eq: usize) -> Result<(usize, usize), FlowError> {
    let start = skip_ws(masked, eq + 1);
    if !masked[start..].starts_with('{') {
        return Err(FlowError::Meta(
            "`meta` must be assigned an object literal `{ ... }`".to_string(),
        ));
    }
    let mut depth = 0usize;
    for (off, ch) in masked[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((start, start + off + 1));
                }
            }
            _ => {}
        }
    }
    Err(FlowError::Meta(
        "unterminated `meta` object literal: no matching `}`".to_string(),
    ))
}

// ───────────────────────────────────────────────────────────────────────────
// restricted literal parser
// ───────────────────────────────────────────────────────────────────────────

struct LiteralParser<'a> {
    src: &'a str,
    at: usize,
    end: usize,
}

fn literal_err(what: &str, at: usize) -> FlowError {
    FlowError::Meta(format!(
        "`meta` must be a pure object literal (no variables, calls, spreads or template \
         interpolation): {what} at offset {at}"
    ))
}

fn parse_literal(src: &str, start: usize, end: usize) -> Result<serde_json::Value, FlowError> {
    let mut p = LiteralParser {
        src,
        at: start,
        end,
    };
    let v = p.value()?;
    p.skip_trivia();
    if p.at != end {
        return Err(literal_err("trailing content", p.at));
    }
    Ok(v)
}

impl LiteralParser<'_> {
    fn peek(&self) -> Option<char> {
        if self.at >= self.end {
            return None;
        }
        self.src[self.at..self.end].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.at += c.len_utf8();
            true
        } else {
            false
        }
    }

    /// Whitespace and comments — both legal inside a literal.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.at += c.len_utf8();
                }
                Some('/') if self.src[self.at..self.end].starts_with("//") => {
                    while let Some(c) = self.peek() {
                        self.at += c.len_utf8();
                        if c == '\n' {
                            break;
                        }
                    }
                }
                Some('/') if self.src[self.at..self.end].starts_with("/*") => {
                    self.at += 2;
                    while self.at < self.end && !self.src[self.at..self.end].starts_with("*/") {
                        let c = self.peek().unwrap_or('*');
                        self.at += c.len_utf8();
                    }
                    self.at = (self.at + 2).min(self.end);
                }
                _ => return,
            }
        }
    }

    fn value(&mut self) -> Result<serde_json::Value, FlowError> {
        self.skip_trivia();
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') | Some('\'') | Some('`') => Ok(serde_json::Value::String(self.string()?)),
            Some(c) if c == '-' || c == '+' || c.is_ascii_digit() => self.number(),
            Some(c) if is_ident_char(c) => {
                let word = self.ident();
                match word.as_str() {
                    "true" => Ok(serde_json::Value::Bool(true)),
                    "false" => Ok(serde_json::Value::Bool(false)),
                    "null" => Ok(serde_json::Value::Null),
                    // `undefined` is not JSON; treat it as absent so
                    // `{detail: undefined}` behaves like omitting the key.
                    "undefined" => Ok(serde_json::Value::Null),
                    other => Err(literal_err(
                        &format!("identifier `{other}`"),
                        self.at - other.len(),
                    )),
                }
            }
            Some('.') => Err(literal_err("spread (`...`)", self.at)),
            Some(c) => Err(literal_err(&format!("unexpected `{c}`"), self.at)),
            None => Err(literal_err("unexpected end of literal", self.at)),
        }
    }

    fn object(&mut self) -> Result<serde_json::Value, FlowError> {
        debug_assert_eq!(self.peek(), Some('{'));
        self.at += 1;
        let mut map = serde_json::Map::new();
        loop {
            self.skip_trivia();
            if self.eat('}') {
                return Ok(serde_json::Value::Object(map));
            }
            if self.peek() == Some('.') {
                return Err(literal_err("spread (`...`)", self.at));
            }
            let key = match self.peek() {
                Some('"') | Some('\'') | Some('`') => self.string()?,
                Some(c) if is_ident_char(c) => self.ident(),
                Some('[') => return Err(literal_err("computed key (`[expr]`)", self.at)),
                Some(c) => return Err(literal_err(&format!("unexpected `{c}` as key"), self.at)),
                None => return Err(literal_err("unexpected end of object", self.at)),
            };
            self.skip_trivia();
            if !self.eat(':') {
                // `{ name }` shorthand or `{ name() {} }` method — both refer
                // to something outside the literal.
                return Err(literal_err(
                    &format!("`{key}` is not a `key: literal` pair"),
                    self.at,
                ));
            }
            let value = self.value()?;
            map.insert(key, value);
            self.skip_trivia();
            if self.eat(',') {
                continue;
            }
            self.skip_trivia();
            if self.eat('}') {
                return Ok(serde_json::Value::Object(map));
            }
            return Err(literal_err("expected `,` or `}`", self.at));
        }
    }

    fn array(&mut self) -> Result<serde_json::Value, FlowError> {
        debug_assert_eq!(self.peek(), Some('['));
        self.at += 1;
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            if self.eat(']') {
                return Ok(serde_json::Value::Array(items));
            }
            items.push(self.value()?);
            self.skip_trivia();
            if self.eat(',') {
                continue;
            }
            self.skip_trivia();
            if self.eat(']') {
                return Ok(serde_json::Value::Array(items));
            }
            return Err(literal_err("expected `,` or `]`", self.at));
        }
    }

    fn ident(&mut self) -> String {
        let start = self.at;
        while let Some(c) = self.peek() {
            if is_ident_char(c) {
                self.at += c.len_utf8();
            } else {
                break;
            }
        }
        self.src[start..self.at].to_string()
    }

    fn number(&mut self) -> Result<serde_json::Value, FlowError> {
        let start = self.at;
        if self.peek() == Some('-') || self.peek() == Some('+') {
            self.at += 1;
        }
        while let Some(c) = self.peek() {
            // `_` is JS numeric separator syntax (1_000); strip it below.
            if c.is_ascii_digit() || c == '.' || c == '_' || c == 'e' || c == 'E' {
                self.at += c.len_utf8();
            } else if (c == '+' || c == '-')
                && matches!(
                    self.src[..self.at].chars().next_back(),
                    Some('e') | Some('E')
                )
            {
                self.at += 1;
            } else {
                break;
            }
        }
        let raw: String = self.src[start..self.at].replace('_', "");
        raw.parse::<serde_json::Number>()
            .map(serde_json::Value::Number)
            .map_err(|_| literal_err(&format!("malformed number `{raw}`"), start))
    }

    fn string(&mut self) -> Result<String, FlowError> {
        let quote = self.bump().expect("caller checked a quote is present");
        let mut out = String::new();
        loop {
            let start = self.at;
            let Some(c) = self.bump() else {
                return Err(literal_err("unterminated string", start));
            };
            if c == quote {
                return Ok(out);
            }
            if quote == '`' && c == '$' && self.peek() == Some('{') {
                return Err(literal_err("template interpolation (`${...}`)", start));
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let Some(esc) = self.bump() else {
                return Err(literal_err("dangling escape", start));
            };
            match esc {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                '0' => out.push('\0'),
                'u' => out.push(self.unicode_escape(start)?),
                // `\'`, `\"`, `\\`, `\<newline>` and anything else: the
                // escaped character itself, matching JS.
                other => out.push(other),
            }
        }
    }

    /// Body of a `\uXXXX` / `\u{XXXXXX}` escape, cursor just past the `u`.
    fn unicode_escape(&mut self, start: usize) -> Result<char, FlowError> {
        let hex = if self.eat('{') {
            let s = self.at;
            while self.peek().is_some_and(|c| c != '}') {
                self.at += 1;
            }
            let h = self.src[s..self.at].to_string();
            if !self.eat('}') {
                return Err(literal_err("unterminated `\\u{...}` escape", start));
            }
            h
        } else {
            let s = self.at;
            for _ in 0..4 {
                if self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    self.at += 1;
                }
            }
            self.src[s..self.at].to_string()
        };
        u32::from_str_radix(&hex, 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| literal_err("invalid unicode escape", start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_name_description_and_phases() {
        let meta = extract_meta(
            r#"
            export const meta = {
              name: 'review-changes',
              description: 'Review a diff across dimensions',
              whenToUse: 'before merging',
              phases: [
                { title: 'Review', detail: 'one agent per dimension' },
                { title: 'Verify' },
              ],
            }
            phase('Review')
            "#,
        )
        .expect("meta parses");
        assert_eq!(meta.name, "review-changes");
        assert_eq!(meta.description, "Review a diff across dimensions");
        assert_eq!(meta.when_to_use.as_deref(), Some("before merging"));
        assert_eq!(meta.phases.len(), 2);
        assert_eq!(meta.phases[0].title, "Review");
        assert_eq!(
            meta.phases[0].detail.as_deref(),
            Some("one agent per dimension")
        );
        assert_eq!(meta.phases[1].detail, None);
    }

    #[test]
    fn missing_meta_block_is_rejected() {
        let err = extract_meta("agent('do a thing')").expect_err("no meta");
        assert!(
            err.to_string().contains("export const meta"),
            "error should name the missing declaration: {err}"
        );
    }

    #[test]
    fn missing_description_is_rejected() {
        let err = extract_meta("export const meta = { name: 'x' }").expect_err("no description");
        assert!(
            err.to_string().contains("meta.description"),
            "error should name the missing field: {err}"
        );
    }

    #[test]
    fn empty_name_is_rejected() {
        let err =
            extract_meta("export const meta = { name: '', description: 'd' }").expect_err("empty");
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    #[test]
    fn computed_meta_values_are_rejected() {
        for source in [
            "const NAME = 'x'\nexport const meta = { name: NAME, description: 'd' }",
            "export const meta = { name: makeName(), description: 'd' }",
            "export const meta = { ...base, name: 'x', description: 'd' }",
            "export const meta = { name: `hello ${who}`, description: 'd' }",
            "export const meta = { [key]: 'x', description: 'd' }",
        ] {
            let err = extract_meta(source).expect_err("computed meta must be rejected");
            assert!(
                err.to_string().contains("pure object literal"),
                "wrong error for {source:?}: {err}"
            );
        }
    }

    #[test]
    fn shorthand_and_methods_are_rejected() {
        let err = extract_meta("const name = 'x'\nexport const meta = { name, description: 'd' }")
            .expect_err("shorthand");
        assert!(err.to_string().contains("pure object literal"), "{err}");
    }

    #[test]
    fn plain_template_literal_is_a_string() {
        let meta = extract_meta("export const meta = { name: `plain`, description: 'd' }")
            .expect("no interpolation");
        assert_eq!(meta.name, "plain");
    }

    #[test]
    fn literal_supports_comments_trailing_commas_and_escapes() {
        let meta = extract_meta(
            r#"export const meta = {
                 // the name
                 name: 'it\'s fine',
                 /* block */ description: "a A b",
               }"#,
        )
        .expect("meta parses");
        assert_eq!(meta.name, "it's fine");
        assert_eq!(meta.description, "a A b");
    }

    #[test]
    fn meta_declaration_inside_a_string_does_not_match() {
        let err = extract_meta("agent(\"export const meta = {oops: 1}\")").expect_err("no meta");
        assert!(err.to_string().contains("export const meta"), "{err}");
    }

    #[test]
    fn banned_apis_are_rejected_with_an_actionable_message() {
        for (source, needle) in [
            ("const t = Date.now()", "Date.now()"),
            ("const r = Math.random()", "Math.random()"),
            ("const d = new Date()", "new Date()"),
            ("const d = new Date", "new Date()"),
            (
                "const f = new Intl.DateTimeFormat('en')",
                "Intl.DateTimeFormat",
            ),
        ] {
            let err = assert_deterministic(source).expect_err("must be rejected");
            let text = err.to_string();
            assert!(text.contains(needle), "expected {needle} in: {text}");
            assert!(
                text.contains("args"),
                "message must tell the author what to do instead: {text}"
            );
        }
    }

    #[test]
    fn deterministic_apis_with_arguments_are_allowed() {
        assert_deterministic("const d = new Date(2026, 0, 1); const s = Date.parse(x)")
            .expect("explicit-argument Date is deterministic");
    }

    #[test]
    fn banned_apis_inside_prompts_and_comments_are_allowed() {
        // The whole point of this runner is orchestrating code review; a
        // prompt that says "look for Math.random()" must not be rejected.
        assert_deterministic("agent('find every Math.random() and Date.now() call')")
            .expect("string contents are not code");
        assert_deterministic("// one day: Date.now() would be nice\nagent('x')")
            .expect("comments are not code");
        assert_deterministic("agent(`audit ${target} for Date.now()`)")
            .expect("template text is not code");
    }

    #[test]
    fn banned_api_inside_a_template_substitution_is_still_code() {
        assert_deterministic("agent(`seed ${Math.random()}`)")
            .expect_err("a substitution is executable code");
    }

    #[test]
    fn mask_preserves_byte_offsets_with_multibyte_text() {
        let src = "const s = '评审代码'; Date.now()";
        let masked = mask_non_code(src);
        assert_eq!(masked.len(), src.len());
        assert!(masked.contains("Date.now"));
        assert!(!masked.contains('评'));
    }

    #[test]
    fn regex_literals_do_not_desynchronise_the_mask() {
        let src = r#"const re = /["']/; const t = Date.now()"#;
        assert!(
            assert_deterministic(src).is_err(),
            "a quote inside a regex must not swallow the rest of the file"
        );
    }
}
