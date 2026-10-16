//! L9 → L8 bridge: natural-language generation intent (rule-based v1).
//!
//! Mirrors [`crate::parse`] for the asset pipeline: an utterance that
//! asks for *content* (a 3D model, a video clip) becomes a
//! [`GenerationRequest`] for [`oriera_genai::GenAiSystem`]; everything
//! else still goes to `parse()` → L7 [`oriera_infer::Command`]. Apps
//! try generation first, commands second:
//!
//! ```no_run
//! use oriera_agent::genai::parse_generation;
//! use oriera_genai::GenAiSystem;
//!
//! let mut genai: GenAiSystem = GenAiSystem::new(std::sync::Arc::new(
//!     oriera_genai::MockProvider::new(),
//! ));
//! if let Some(req) = parse_generation("生成一个 3D 模型：雨夜的咖啡馆门帘") {
//!     genai.submit(req); // landed via L4 hot swap a few ticks later
//! }
//! ```
//!
//! v1 is rule-based (zh + en, negation-aware), same contract `parse()`
//! carries: unmatched input returns `None`, never a guess. The LLM
//! version lands with v0.5–v0.7 model routing.

use oriera_genai::{GenerationKind, GenerationRequest};

/// Generation intent keywords, zh + en.
const GEN_3D_KEYWORDS: &[&str] = &["3d", "模型", "model", "mesh", "网格"];
const GEN_VIDEO_KEYWORDS: &[&str] = &["视频", "video", "clip"];
/// Negation words that suppress a generation intent they precede
/// (same list and semantics as `parse()`).
const NEGATIONS: &[&str] = &["不要", "别", "勿", "don't", "dont", "do not", "never"];
/// Explicit-naming markers: "命名为 X" / "名为 X" / "叫 X" / "name X".
/// ASCII markers carry their trailing space to avoid matching inside
/// words ("as" in "based").
const NAME_MARKERS: &[&str] = &["命名为", "名为", "叫", "name ", " as "];

/// Parse an utterance into a generation request. Returns `None` when
/// the utterance is not about generated content (or is negated).
pub fn parse_generation(input: &str) -> Option<GenerationRequest> {
    let lower = input.to_lowercase();
    if negation_precedes(&lower) {
        return None;
    }
    let kind = if GEN_VIDEO_KEYWORDS.iter().any(|k| lower.contains(k)) {
        GenerationKind::TextToVideo
    } else if GEN_3D_KEYWORDS.iter().any(|k| lower.contains(k)) {
        GenerationKind::TextTo3d
    } else {
        return None;
    };
    let name = extract_name(input).unwrap_or_else(|| fallback_name(input));
    let req = match kind {
        GenerationKind::TextToVideo => GenerationRequest::text_to_video(name, input),
        _ => GenerationRequest::text_to_3d(name, input),
    };
    Some(req)
}

/// First negation before the first generation keyword suppresses the
/// intent ("别生成模型" → None); after it, it does not.
fn negation_precedes(lower: &str) -> bool {
    let first_neg = NEGATIONS.iter().filter_map(|n| lower.find(n)).min();
    let first_kw = GEN_3D_KEYWORDS
        .iter()
        .chain(GEN_VIDEO_KEYWORDS)
        .filter_map(|k| lower.find(k))
        .min();
    match (first_neg, first_kw) {
        (Some(neg), Some(kw)) => neg < kw,
        _ => false,
    }
}

/// Extract an explicit landing name: after the first naming marker,
/// up to the next delimiter or whitespace, capped at 32 chars.
fn extract_name(input: &str) -> Option<String> {
    let lower = input.to_lowercase();
    let (idx, marker_len) = NAME_MARKERS
        .iter()
        .filter_map(|m| lower.find(m).map(|i| (i, m.len())))
        .min()?;
    let rest = &input[(idx + marker_len).min(input.len())..];
    let name: String = rest
        .trim_start()
        .chars()
        .take_while(|c| !c.is_whitespace() && !"，。,.;；：:、!！?？\"'「」“”[]()（）".contains(*c))
        .take(32)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Deterministic fallback name from the utterance: `gen_` + FNV-1a.
/// Two identical prompts share a name (regeneration semantics); two
/// different prompts practically never collide.
fn fallback_name(input: &str) -> String {
    let mut hash: u32 = 0x811C9DC5;
    for b in input.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    format!("gen_{hash:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_3d_and_video_intents() {
        let r = parse_generation("生成一个 3D 模型：雨夜的咖啡馆门帘").unwrap();
        assert_eq!(r.kind, GenerationKind::TextTo3d);
        assert_eq!(r.name, fallback_name("生成一个 3D 模型：雨夜的咖啡馆门帘"));

        let r = parse_generation("make a video of the rainy cafe").unwrap();
        assert_eq!(r.kind, GenerationKind::TextToVideo);
    }

    #[test]
    fn explicit_names_are_extracted() {
        let r = parse_generation("生成模型 命名为 hero_prop，做旧木吉他").unwrap();
        assert_eq!(r.name, "hero_prop");
        let r = parse_generation("generate a 3d model name npc_hat").unwrap();
        assert_eq!(r.name, "npc_hat");
    }

    #[test]
    fn fallback_names_are_deterministic_and_distinct() {
        assert_eq!(fallback_name("门帘"), fallback_name("门帘"));
        assert_ne!(fallback_name("门帘"), fallback_name("吉他"));
        let r = parse_generation("生成一个模型：门帘").unwrap();
        assert!(r.name.starts_with("gen_"));
    }

    #[test]
    fn negation_before_intent_suppresses_generation() {
        assert!(parse_generation("别生成模型").is_none());
        assert!(parse_generation("don't make a video").is_none());
    }

    #[test]
    fn non_generation_utteances_return_none() {
        // These belong to parse() → L7 commands.
        assert!(parse_generation("spawn 50 particles").is_none());
        assert!(parse_generation("清空").is_none());
        assert!(parse_generation("hello there").is_none());
    }
}
