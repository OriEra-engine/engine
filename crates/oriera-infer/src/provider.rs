//! L7 model provider boundary (book T10).
//!
//! The engine — not the application — owns model access. Upper layers
//! (L9 agents, L11 apps) talk to [`ModelProvider`] through the inference
//! runtime; they never see HTTP, keys or model names. Browser/wasm builds
//! implement this trait over `fetch` (see examples/11-ai-cafe engine
//! kernel); native builds plug in an async client behind a feature flag
//! (ROADMAP v0.6). `RuleProvider` is the deterministic offline backend
//! that keeps tests hermetic and lets the engine run without a key.

use std::fmt;

/// One turn of a chat-format request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

/// Why a completion failed. Keys and transport details stay inside the
/// provider — they never leak into layer-9 prompt code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferError {
    /// The provider refused / errored / timed out.
    Provider(String),
}

impl fmt::Display for InferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InferError::Provider(m) => write!(f, "provider error: {m}"),
        }
    }
}

impl std::error::Error for InferError {}

/// The model boundary every consumer depends on (L7 contract,
/// `oriera.infer.provider.v1`).
pub trait ModelProvider: Send + Sync {
    /// Stable provider id for metrics (`oriera_infer_provider{...}`).
    fn id(&self) -> &str;

    /// Produce one assistant completion for the chat so far.
    fn complete(&self, messages: &[ChatMessage]) -> Result<String, InferError>;
}

/// Deterministic offline backend: picks a canned line by keyword.
/// Same input → same output, so engine tests are hermetic.
pub struct RuleProvider {
    pub id: &'static str,
}

impl Default for RuleProvider {
    fn default() -> Self {
        Self { id: "rule-v1" }
    }
}

impl RuleProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ModelProvider for RuleProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn complete(&self, messages: &[ChatMessage]) -> Result<String, InferError> {
        let last = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User || m.role == Role::System)
            .map(|m| m.content.as_str())
            .unwrap_or("");
        let say = if last.contains("咖啡") || last.contains("coffee") {
            "（擦着杯子）这豆子是埃塞的，果酸重，等它凉半分钟再喝。"
        } else if last.contains("画") || last.contains("绘本") {
            "（笔没停）你这句话能进我的绘本——'咖啡馆记得每个人的口味'。"
        } else if last.contains("引擎") || last.contains("代码") {
            "我那个世界引擎 demo 跑通了！两万实体六十帧，虽然只是小方块在飞。"
        } else {
            "（点头）这条街变了很多，好在咖啡馆没变。"
        };
        Ok(say.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_provider_is_deterministic_and_keyworded() {
        let p = RuleProvider::new();
        let a = p.complete(&[ChatMessage::user("聊聊咖啡豆")]).unwrap();
        let b = p.complete(&[ChatMessage::user("聊聊咖啡豆")]).unwrap();
        assert_eq!(a, b);
        assert!(a.contains("豆子"));
        assert!(p
            .complete(&[ChatMessage::user("x")])
            .unwrap()
            .contains("咖啡馆"));
    }
}
