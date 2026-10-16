//! L9 Agent — cognition, memory and world-progression as engine services.
//!
//! Three capabilities live here (book T12), all driven through the L2
//! scheduler as [`oriera_core::System`]s — applications never call a
//! model directly:
//!
//! * [`cognition::CognitionSystem`] — NPCs speak each N engine ticks,
//!   prompted from their retrieved long-term memories and the current
//!   world beat, via the L7 [`oriera_infer::ModelProvider`] boundary.
//! * [`cognition::DirectorSystem`] — the AI director advances the world
//!   story (beats/days) from the running dialogue.
//! * [`memory::MemoryStore`] — episodic long-term memory with
//!   keyword/recency/importance retrieval, mirrored 1:1 by the wasm
//!   kernel in `examples/11-ai-cafe/engine/kernel.js`.
//!
//! Intent parsing (`parse`) stays for command-style applications.
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

pub mod cognition;
pub mod genai;
pub mod memory;

pub use cognition::{
    CognitionSystem, DialogueLog, DirectorSystem, Mood, Persona, Turn, WorldStory,
};
pub use memory::{Episode, MemoryStore};

use oriera_infer::Command;

/// Parse a natural-language utterance into an engine [`Command`].
///
/// Returns `Command::Unknown(input)` when no intent matches — the L7
/// runtime counts it under `rejected` and executes nothing. Recognized
/// intents: spawn/放/生成 + N, clear/清空/清屏, gravity/重力 + N,
/// teleport/传送 + X Y, how many/多少个, speed/速度/倍速 + N.
///
/// A negation word ("不要", "别", "勿", "don't"…) appearing **before**
/// the intent keyword suppresses execution ("别清空" is not Clear);
/// a negation after the keyword does not ("清空，但别关灯" still Clear).
pub fn parse(input: &str) -> Command {
    let lower = input.to_lowercase();
    if negation_precedes_intent(&lower) {
        return Command::Unknown(input.to_string());
    }
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
    if lower.contains("teleport") || lower.contains("传送") {
        let nums = extract_numbers(&lower);
        if nums.len() >= 2 {
            return Command::Teleport(nums[0] as f32, nums[1] as f32);
        }
    }
    if lower.contains("how many") || lower.contains("多少") {
        return Command::Count;
    }
    if lower.contains("speed") || lower.contains("速度") || lower.contains("倍速") {
        if let Some(k) = extract_number(&lower) {
            return Command::SetSpeed(k as f32);
        }
    }
    Command::Unknown(input.to_string())
}

/// Negation words that suppress an intent they precede.
const NEGATIONS: &[&str] = &["不要", "别", "勿", "don't", "dont", "do not", "never"];

/// Intent keywords, mirrored from [`parse`] — used only to locate the
/// first intent mention and compare it against negation positions.
const INTENT_KEYWORDS: &[&str] = &[
    "spawn", "放", "生成", "clear", "清空", "清屏", "gravity", "重力", "teleport", "传送",
    "how many", "多少", "speed", "速度", "倍速",
];

fn negation_precedes_intent(lower: &str) -> bool {
    let Some(first_neg) = NEGATIONS.iter().filter_map(|n| lower.find(n)).min() else {
        return false;
    };
    match INTENT_KEYWORDS.iter().filter_map(|k| lower.find(k)).min() {
        Some(first_kw) => first_neg < first_kw,
        None => false,
    }
}

/// Extract the first number in `s` ("gravity 9.8" → 9.8, not 98).
fn extract_number(s: &str) -> Option<f64> {
    extract_numbers(s).into_iter().next()
}

/// Extract every number in `s`, in order ("teleport to 100 -25" → [100, -25]).
/// A leading '-' counts as part of the number; lone '-'/'.' are dropped.
fn extract_numbers(s: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for c in s.chars() {
        if c.is_ascii_digit()
            || (c == '.' && !buf.contains('.') && !buf.ends_with('-'))
            || (c == '-' && buf.is_empty())
        {
            buf.push(c);
        } else if !buf.is_empty() {
            if let Ok(n) = buf.parse::<f64>() {
                out.push(n);
            }
            buf.clear();
        }
    }
    if let Ok(n) = buf.parse::<f64>() {
        out.push(n);
    }
    out
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
    fn parses_teleport() {
        assert_eq!(
            parse("teleport to 100 200"),
            Command::Teleport(100.0, 200.0)
        );
        assert_eq!(parse("传送到 50,-25"), Command::Teleport(50.0, -25.0));
    }

    #[test]
    fn parses_count() {
        assert_eq!(parse("how many entities"), Command::Count);
        assert_eq!(parse("现在有多少个"), Command::Count);
    }

    #[test]
    fn parses_speed() {
        assert_eq!(parse("speed 2x"), Command::SetSpeed(2.0));
        assert_eq!(parse("速度 1.5 倍"), Command::SetSpeed(1.5));
    }

    #[test]
    fn teleport_needs_two_numbers() {
        assert_eq!(
            parse("teleport to the moon"),
            Command::Unknown("teleport to the moon".to_string())
        );
    }

    #[test]
    fn unknown_falls_through() {
        match parse("hello there") {
            Command::Unknown(s) => assert_eq!(s, "hello there"),
            other => panic!("expected Unknown, got {:?}", other),
        }
    }

    #[test]
    fn negation_before_intent_suppresses_it() {
        assert_eq!(parse("别清空"), Command::Unknown("别清空".to_string()));
        assert_eq!(
            parse("不要 spawn 5"),
            Command::Unknown("不要 spawn 5".to_string())
        );
        assert_eq!(
            parse("don't clear"),
            Command::Unknown("don't clear".to_string())
        );
    }

    #[test]
    fn negation_after_intent_does_not_suppress() {
        assert_eq!(parse("清空，但别关灯"), Command::Clear);
        assert_eq!(parse("spawn 5, never mind the rest"), Command::Spawn(5));
    }
}
