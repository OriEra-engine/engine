//! L9 Agent — natural-language intent layer.
//!
//! The agent turns what a user *says* into structured engine commands,
//! then hands them to the L7 inference runtime for execution. v0.1 is a
//! rule-based parser (no LLM); the interface is shaped so an LLM backend
//! can drop in later without touching callers (book T12, ROADMAP v0.5–0.7).
//!
//! Example round trip:
//!
//! ```no_run
//! use oriera_agent::parse;
//! use oriera_infer::InferSystem;
//!
//! let mut infer = InferSystem::new();
//! infer.submit(parse("spawn 100 particles"));
//! infer.submit(parse("把重力设为 9.8"));
//! # assert_eq!(infer.processed, 0); // executed on the next scheduler tick
//! ```

use oriera_infer::Command;

/// Parse a natural-language utterance into an engine [`Command`].
///
/// Returns `Command::Unknown(input)` when no intent matches — the L7
/// runtime counts it under `rejected` and executes nothing. Recognized
/// intents: spawn/放/生成 + N, clear/清空/清屏, gravity/重力 + N.
pub fn parse(input: &str) -> Command {
    let lower = input.to_lowercase();
    if lower.contains("spawn") || lower.contains("放") || lower.contains("生成") {
        if let Some(n) = extract_number(&lower) {
            return Command::Spawn(n.round() as usize);
        }
    }
    if lower.contains("clear") || lower.contains("清空") || lower.contains("清屏") {
        return Command::Clear;
    }
    if lower.contains("gravity") || lower.contains("重力") {
        if let Some(g) = extract_number(&lower) {
            return Command::SetGravity(g as f32);
        }
    }
    Command::Unknown(input.to_string())
}

/// Extract the first number in `s` ("gravity 9.8" → 9.8, not 98).
fn extract_number(s: &str) -> Option<f64> {
    let mut buf = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || (c == '.' && !buf.contains('.')) {
            buf.push(c);
        } else if !buf.is_empty() {
            break; // first number complete
        }
    }
    if buf == "." {
        return None;
    }
    buf.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spawn() {
        assert_eq!(parse("spawn 100 particles"), Command::Spawn(100));
        assert_eq!(parse("生成 50 个粒子"), Command::Spawn(50));
    }

    #[test]
    fn parses_clear() {
        assert_eq!(parse("clear the world"), Command::Clear);
        assert_eq!(parse("清空"), Command::Clear);
    }

    #[test]
    fn parses_gravity() {
        assert_eq!(parse("set gravity to 10"), Command::SetGravity(10.0));
        assert_eq!(parse("gravity 9.8"), Command::SetGravity(9.8));
        assert_eq!(parse("重力 9"), Command::SetGravity(9.0));
    }

    #[test]
    fn unknown_falls_through() {
        match parse("hello there") {
            Command::Unknown(s) => assert_eq!(s, "hello there"),
            other => panic!("expected Unknown, got {:?}", other),
        }
    }
}
