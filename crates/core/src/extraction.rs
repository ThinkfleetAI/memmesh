// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Heuristic memory extraction.
//!
//! Given a raw piece of text (a user message, an assistant turn, a chat log
//! line), return zero-or-more `ExtractedMemory` candidates that the caller
//! should persist via the Storage trait.
//!
//! v1 is regex + structural rules. It runs fast, has no external
//! dependencies, and is deterministic. It deliberately optimizes for
//! *precision* over recall: better to miss a memorable line than to flood
//! the memory store with conversational filler.
//!
//! Future variants (`extract_with_llm`) can plug in a Haiku call for
//! refinement — same return shape, same trait. The MCP server and the CLI
//! both consume this function; what changes is the backend that produces
//! the candidates.

use crate::{MemoryImpact, MemoryScope};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Role of the message being observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObserveRole {
    User,
    Assistant,
    System,
}

/// Lightweight context the caller supplies along with the text. Each field
/// flows into the `MemoryItem` if extraction succeeds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserveContext {
    pub platform_id: Option<String>,
    pub project_id: Option<String>,
    pub user_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub role: Option<ObserveRole>,
}

/// One extracted memory candidate. The caller maps these onto `MemoryItem`s
/// using its own ID strategy + the `ObserveContext`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedMemory {
    pub content: String,
    pub kind: &'static str,
    pub scope: MemoryScope,
    pub importance: f32,
    pub impact: MemoryImpact,
    /// Why the extractor thought this was worth saving (for audit /
    /// debugging — operators can see which rule fired).
    pub reason: &'static str,
}

/// Top-level extraction entrypoint. Returns 0..N candidates.
pub fn extract(text: &str, ctx: &ObserveContext) -> Vec<ExtractedMemory> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    if is_filler(text) {
        return Vec::new();
    }

    let mut out = Vec::new();
    for line in segment(text) {
        // SAFETY NET: never persist a line that looks like it carries a live
        // credential. Secrets belong in the encrypted vault (entered by the
        // user out-of-band), never in the plaintext memory store — and never
        // in the context we later inject back. Drop the whole line.
        if contains_secret(&line) {
            continue;
        }
        if let Some(m) = extract_line(&line, ctx) {
            out.push(m);
        }
    }

    // Fallback: nothing matched a structured rule, but the input is
    // substantive prose. Keep it verbatim as a raw `observation` rather than
    // silently dropping it — an agent memory that discards what the user tells
    // it is worse than one that over-captures. The structured rules are
    // first-person-anchored ("I prefer…", "we decided…"); most real input
    // (third-person, questions aside, arbitrary facts) matches nothing, so
    // without this the store stays empty. Hosted LLM extraction refines these
    // into typed facts; locally we at least never lose them.
    if out.is_empty() && !contains_secret(text) {
        if let Some(m) = fallback_observation(text) {
            out.push(m);
        }
    }

    out
}

/// Regexes for common live credentials. Precision-first: these match
/// high-signal token shapes and `key = value` secret assignments, not every
/// mention of the word "password". Used only as a safety net so a pasted
/// credential never lands in the plaintext memory store.
static SECRET_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    [
        r"AKIA[0-9A-Z]{16}",                                  // AWS access key id
        r"ASIA[0-9A-Z]{16}",                                  // AWS temp key id
        r"sk-ant-[A-Za-z0-9_\-]{20,}",                        // Anthropic
        r"sk-[A-Za-z0-9]{20,}",                               // OpenAI-style
        r"gh[opsu]_[A-Za-z0-9]{30,}",                         // GitHub tokens
        r"xox[baprs]-[A-Za-z0-9-]{10,}",                      // Slack
        r"AIza[0-9A-Za-z\-_]{35}",                            // Google API key
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----",               // PEM private key
        r"eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}", // JWT
        r"(?i)(password|passwd|pwd|api[_-]?key|secret|token|bearer)\s*[:=]\s*\S{6,}",
        r"(?i)(postgres(ql)?|mysql|mongodb(\+srv)?|redis|amqp)://[^:\s/]+:[^@\s]+@", // DSN with creds
    ]
    .iter()
    .map(|p| Regex::new(p).expect("invalid secret regex"))
    .collect()
});

/// True if the text appears to contain a live credential.
pub fn contains_secret(text: &str) -> bool {
    SECRET_PATTERNS.iter().any(|re| re.is_match(text))
}

/// Keep substantive input that matched no structured rule as a raw
/// `observation`. Filters out questions, code/commands, and short fragments so
/// the store fills with statements rather than chatter or one-word acks.
fn fallback_observation(text: &str) -> Option<ExtractedMemory> {
    let trimmed = text.trim();
    if is_code_or_command(trimmed) {
        return None;
    }
    // Questions ask, they don't assert — skip them.
    if trimmed.ends_with('?') {
        return None;
    }
    // Require some substance so acks that slipped past the filler filter
    // ("sounds good to me") don't become memories.
    let word_count = trimmed.split_whitespace().count();
    if trimmed.chars().count() < 24 || word_count < 4 {
        return None;
    }
    Some(ExtractedMemory {
        content: normalize_subject(trimmed),
        kind: "observation",
        scope: MemoryScope::Project,
        importance: 3.0,
        impact: MemoryImpact::Low,
        reason: "raw-observation",
    })
}

/// Split a message into sentence-ish lines for per-clause extraction. Real
/// NLP-grade sentence splitting is overkill; we just want to keep distinct
/// statements distinct.
fn segment(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let trimmed = paragraph.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip fenced code blocks entirely.
        if trimmed.starts_with("```") {
            continue;
        }
        // Sentence-split on ". " / "! " / "? "; preserve original case.
        let mut start = 0;
        let chars: Vec<char> = trimmed.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if (*c == '.' || *c == '!' || *c == '?')
                && chars.get(i + 1).is_some_and(|n| n.is_whitespace())
            {
                let s: String = chars[start..=i].iter().collect();
                let s = s.trim().to_string();
                if !s.is_empty() {
                    out.push(s);
                }
                start = i + 1;
            }
        }
        let tail: String = chars[start..].iter().collect();
        let tail = tail.trim().to_string();
        if !tail.is_empty() {
            out.push(tail);
        }
    }
    out
}

/// Try each rule in priority order. First match wins.
fn extract_line(raw: &str, _ctx: &ObserveContext) -> Option<ExtractedMemory> {
    if is_code_or_command(raw) || is_filler(raw) {
        return None;
    }

    // Strip leading conjunctions / interjections that don't change meaning.
    let line = raw
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string();
    let lower = line.to_lowercase();

    // Each rule: (regex, kind, scope, importance, impact, reason)
    for (re, kind, scope, importance, impact, reason) in RULES.iter() {
        if re.is_match(&lower) {
            // Normalize the content to third person — swap leading "I " /
            // "my " / "we " for clearer storage. Heuristic; safe to refine.
            let normalized = normalize_subject(&line);
            return Some(ExtractedMemory {
                content: normalized,
                kind,
                scope: *scope,
                importance: *importance,
                impact: *impact,
                reason,
            });
        }
    }
    None
}

/// Map first-person to third-person at the start of the line so the stored
/// memory reads cleanly across sessions and tools. Crude, intentionally —
/// false transformations on edge cases are recoverable; missed
/// transformations cost nothing.
fn normalize_subject(line: &str) -> String {
    let lower = line.to_lowercase();
    // Match prefix on lowercase, slice the *original* string at the same
    // byte index so case + non-ASCII characters survive intact.
    let prefixes = [
        ("i prefer ", "User prefers "),
        ("i'm ", "User is "),
        ("i am ", "User is "),
        ("i use ", "User uses "),
        ("my ", "User's "),
        ("we ", "Team "),
    ];
    for (lower_pref, replacement) in prefixes {
        if lower.starts_with(lower_pref) {
            return format!("{}{}", replacement, &line[lower_pref.len()..]);
        }
    }
    line.to_string()
}

/// Rules registry. Each entry is a precompiled regex + how the extractor
/// should tag the resulting memory if it fires. Patterns are
/// case-insensitive (we lowercase the line first).
type Rule = (
    Regex,
    &'static str,
    MemoryScope,
    f32,
    MemoryImpact,
    &'static str,
);

static RULES: Lazy<Vec<Rule>> = Lazy::new(|| {
    use MemoryImpact::*;
    use MemoryScope::*;

    let rules: Vec<(
        &str,
        &'static str,
        MemoryScope,
        f32,
        MemoryImpact,
        &'static str,
    )> = vec![
        // Identity / personal facts
        (
            r"^i('m| am) (a|an) ",
            "fact",
            User,
            7.0,
            High,
            "identity-statement",
        ),
        (
            r"^my name (is|'s) ",
            "fact",
            User,
            8.0,
            High,
            "name-statement",
        ),
        (
            r"^i work (at|for|on) ",
            "fact",
            User,
            7.0,
            High,
            "employer-statement",
        ),
        (
            r"^i live (in|at|near) ",
            "fact",
            User,
            7.0,
            High,
            "location-statement",
        ),
        // Preferences — most common autosave target
        (
            r"^i prefer ",
            "preference",
            User,
            6.0,
            Low,
            "prefer-statement",
        ),
        (
            r"^i (always|usually) ",
            "preference",
            User,
            6.0,
            Low,
            "habit-statement",
        ),
        (
            r"^i (never|don't|do not) (like|use|want) ",
            "preference",
            User,
            6.0,
            Low,
            "negative-preference",
        ),
        (
            r"^i hate ",
            "preference",
            User,
            6.0,
            Low,
            "negative-preference",
        ),
        (
            r"^i love ",
            "preference",
            User,
            6.0,
            Low,
            "positive-preference",
        ),
        // Decisions (team or personal)
        (
            r"^we (decided|chose|picked|went with) ",
            "decision",
            Project,
            7.0,
            High,
            "team-decision",
        ),
        (
            r"^let'?s (use|go with|switch to) ",
            "decision",
            Project,
            6.0,
            Low,
            "decision-proposal",
        ),
        (
            r"^(remember|note) that ",
            "fact",
            Project,
            7.0,
            Low,
            "explicit-remember",
        ),
        // Rules
        (
            r"^(this|the) (codebase|repo|project) (uses|requires|needs) ",
            "rule",
            Project,
            7.0,
            High,
            "project-rule",
        ),
        (
            r"^we (use|require) ",
            "rule",
            Project,
            6.0,
            Low,
            "team-rule",
        ),
        (
            r"^(never|don't|always) use ",
            "rule",
            Project,
            6.0,
            Low,
            "imperative-rule",
        ),
        (
            r"^(always|never) ",
            "rule",
            Project,
            5.0,
            Low,
            "absolute-rule",
        ),
        // Constraints
        (
            r"(by (monday|tuesday|wednesday|thursday|friday|saturday|sunday|tomorrow|next week|eod|cob))",
            "constraint",
            Project,
            7.0,
            Low,
            "deadline",
        ),
        (
            r"(deadline|due|cutoff)\b",
            "constraint",
            Project,
            6.0,
            Low,
            "deadline-mention",
        ),
        // Project / org facts
        (
            r"^my (company|team|org|client) (is|'s) ",
            "fact",
            User,
            7.0,
            Low,
            "org-statement",
        ),
        (
            r"^the (company|project|product) (is|'s) called ",
            "fact",
            Project,
            7.0,
            Low,
            "naming-statement",
        ),
        // Strong "remember this" signals
        (
            r"^remember ",
            "fact",
            Project,
            8.0,
            Low,
            "explicit-remember",
        ),
        (
            r"^don'?t forget ",
            "fact",
            Project,
            8.0,
            Low,
            "explicit-remember",
        ),
        (
            r"^make a note ",
            "fact",
            Project,
            8.0,
            Low,
            "explicit-remember",
        ),
        // Ideas / aspirations — softer, forward-looking language the user
        // muses about ("we should…", "what if we…", "might want to…"). Kept
        // LOWER priority than facts/decisions/rules so those still win; this
        // block upgrades what would otherwise be a low-value raw observation
        // into a first-class, listable `idea`. Non-anchored `\b…` variants
        // catch the cue mid-sentence (e.g. "…app we might want to do X").
        (
            r"^idea[:\-] ",
            "idea",
            Project,
            6.0,
            Low,
            "explicit-idea",
        ),
        (
            r"\b(one|another|quick|random|cool) idea\b",
            "idea",
            Project,
            6.0,
            Low,
            "explicit-idea",
        ),
        (
            r"^what if we ",
            "idea",
            Project,
            6.0,
            Low,
            "idea-what-if",
        ),
        (
            r"\bwe (might|may) want to ",
            "idea",
            Project,
            6.0,
            Low,
            "idea-aspiration",
        ),
        (
            r"\b(i'?m|we'?re) thinking (we|that|about|maybe) ",
            "idea",
            Project,
            6.0,
            Low,
            "idea-thinking",
        ),
        (
            r"\bwe (should|could|ought to) ",
            "idea",
            Project,
            5.0,
            Low,
            "idea-suggestion",
        ),
        (
            r"\bit would be (nice|good|great|cool|helpful|useful) (to|if) ",
            "idea",
            Project,
            5.0,
            Low,
            "idea-wish",
        ),
        (
            r"\b(eventually|someday|down the road|at some point) we ",
            "idea",
            Project,
            5.0,
            Low,
            "idea-future",
        ),
    ];

    rules
        .into_iter()
        .map(|(pat, kind, scope, imp, impact, reason)| {
            let p = format!("(?i){pat}");
            (
                Regex::new(&p).expect("invalid extraction regex"),
                kind,
                scope,
                imp,
                impact,
                reason,
            )
        })
        .collect()
});

/// Cheap negative filters — skip anything that's clearly not a memory:
/// pleasantries, single tokens, very short messages.
fn is_filler(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.len() < 8 {
        return true;
    }
    let lower = trimmed.to_lowercase();
    const FILLER_PREFIXES: &[&str] = &[
        "hi",
        "hello",
        "hey",
        "thanks",
        "thank you",
        "ok",
        "okay",
        "got it",
        "yes",
        "no",
        "sure",
        "cool",
        "nice",
        "great",
        "awesome",
        "lol",
        "perfect",
        "indeed",
        "right",
        "exactly",
    ];
    for f in FILLER_PREFIXES {
        if lower == *f || lower.starts_with(&format!("{f} ")) || lower.starts_with(&format!("{f},"))
        {
            // Only filter if the rest is also short / non-substantive.
            if trimmed.len() < 24 {
                return true;
            }
        }
    }
    false
}

/// Skip code blocks, shell commands, and tool-output-shaped lines. The
/// heuristic is rough: backticks, sigils that don't appear in prose, or
/// JSON-looking content.
fn is_code_or_command(text: &str) -> bool {
    let t = text.trim();
    if t.starts_with('`') || t.starts_with("```") {
        return true;
    }
    if t.starts_with('$') || t.starts_with('>') || t.starts_with('#') {
        return true;
    }
    if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')) {
        return true;
    }
    // Lines that look like JSON string values / fragments: a quoted string
    // followed by a structural close. Catches "...text"}, "...text"], "...text",
    // which appear when a JSON blob is pasted into the prompt and a sentence
    // ends inside a value.
    if t.ends_with("\"}") || t.ends_with("\"]") || t.ends_with("\",") {
        return true;
    }
    // Lines that look like assignment / function calls in source.
    if t.contains(" => ") || t.contains(" -> ") || t.contains("::") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ObserveContext {
        ObserveContext {
            user_id: Some("u_test".into()),
            ..Default::default()
        }
    }

    #[test]
    fn filler_is_dropped() {
        assert!(extract("hi", &ctx()).is_empty());
        assert!(extract("thanks!", &ctx()).is_empty());
        assert!(extract("ok cool", &ctx()).is_empty());
    }

    #[test]
    fn preferences_extracted() {
        let out = extract("I prefer Vitest over Jest for testing.", &ctx());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "preference");
        assert!(out[0].content.to_lowercase().contains("vitest"));
    }

    #[test]
    fn identity_facts() {
        let out = extract("I'm a senior backend engineer.", &ctx());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "fact");
    }

    #[test]
    fn team_decisions() {
        let out = extract("We decided to use Rust for the memory engine.", &ctx());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "decision");
        assert_eq!(out[0].scope, MemoryScope::Project);
    }

    #[test]
    fn rules() {
        let out = extract("This codebase uses pnpm, never npm.", &ctx());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "rule");
    }

    #[test]
    fn code_blocks_dropped() {
        let out = extract("```js\nconst foo = 1;\n```", &ctx());
        assert!(out.is_empty());
    }

    #[test]
    fn multiple_facts_in_one_message() {
        let text = "My name is Ryan. I prefer Vitest. We decided to use Rust.";
        let out = extract(text, &ctx());
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn explicit_remember_signal() {
        let out = extract("Remember that our standup is Tuesday at 10am.", &ctx());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reason, "explicit-remember");
    }

    #[test]
    fn ignores_questions() {
        let out = extract("What do you think we should do here?", &ctx());
        assert!(out.is_empty());
    }

    #[test]
    fn third_person_statement_kept_as_raw_observation() {
        // Matches no first-person rule, but must not be dropped.
        let out = extract("Ryan prefers pnpm over npm for all projects.", &ctx());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "observation");
        assert_eq!(out[0].reason, "raw-observation");
        assert!(out[0].content.to_lowercase().contains("pnpm"));
    }

    #[test]
    fn arbitrary_fact_captured_via_fallback() {
        let out = extract("Ryan's email is ryan@thinkfleet.ai for work.", &ctx());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "observation");
    }

    #[test]
    fn structured_rule_still_wins_over_fallback() {
        // A first-person preference should classify as `preference`, not the
        // generic `observation` fallback.
        let out = extract("I prefer Vitest over Jest for testing.", &ctx());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "preference");
    }

    #[test]
    fn fallback_skips_questions_and_short_fragments() {
        assert!(extract("What should we do about the migration here?", &ctx()).is_empty());
        assert!(extract("Sounds good to me", &ctx()).is_empty());
    }
}
