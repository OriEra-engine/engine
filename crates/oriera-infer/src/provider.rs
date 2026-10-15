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

/// Which model backend a creator plugged in. The engine never hardcodes
/// a vendor — `ProviderConfig` is data, chosen at boot by the app shell
/// (config file / env / UI dropdown) and resolved via [`create_provider`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub model: String,
    /// API key. Native builds read this from the environment at the
    /// platform shell; it never enters engine logs or world state.
    pub api_key: String,
    /// Base URL (defaults per kind; override for gateways/proxies).
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// 智谱开放平台 (open.bigmodel.cn) — glm-4-flash / glm-4.6.
    Zhipu,
    /// Any OpenAI-compatible /chat/completions endpoint: OpenAI,
    /// DeepSeek, Moonshot, Groq, OpenRouter, vLLM...
    OpenAiCompat,
    /// Anthropic messages API (needs its browser header on wasm).
    Anthropic,
    /// Deterministic offline backend — tests, CI, keyless mode.
    Rule,
}

impl ProviderKind {
    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::Zhipu => "https://open.bigmodel.cn/api/paas/v4",
            ProviderKind::OpenAiCompat => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com/v1",
            ProviderKind::Rule => "",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::Zhipu => "zhipu",
            ProviderKind::OpenAiCompat => "openai-compat",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Rule => "rule",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "zhipu" => Some(ProviderKind::Zhipu),
            "openai-compat" | "openai" | "deepseek" | "moonshot" | "groq" | "openrouter" => {
                Some(ProviderKind::OpenAiCompat)
            }
            "anthropic" => Some(ProviderKind::Anthropic),
            "rule" => Some(ProviderKind::Rule),
            _ => None,
        }
    }
}

/// Resolve a creator's choice into a provider. Native HTTP clients land
/// behind per-kind feature flags (ROADMAP v0.6); until then remote kinds
/// resolve to their wasm/browser implementation (kernel.js
/// `createProvider` mirrors this factory 1:1) and native builds get the
/// rule backend with the choice recorded for observability.
pub fn create_provider(cfg: &ProviderConfig) -> std::sync::Arc<dyn ModelProvider> {
    match cfg.kind {
        ProviderKind::Rule => std::sync::Arc::new(RuleProvider::new()),
        kind => {
            tracing::info!(
                provider = kind.id(),
                model = %cfg.model,
                "L7 remote provider selected; native HTTP adapter pending — browser kernel serves it"
            );
            std::sync::Arc::new(RuleProvider::new())
        }
    }
}

#[cfg(test)]
mod choice_tests {
    use super::*;

    #[test]
    fn creators_choose_any_vendor_through_data() {
        for id in [
            "zhipu",
            "openai",
            "deepseek",
            "moonshot",
            "groq",
            "openrouter",
            "anthropic",
            "rule",
        ] {
            let kind = ProviderKind::from_id(id).expect(id);
            let cfg = ProviderConfig {
                kind,
                model: "m".into(),
                api_key: String::new(),
                base_url: None,
            };
            let p = create_provider(&cfg);
            assert!(!p.id().is_empty());
        }
        assert!(ProviderKind::from_id("nope").is_none());
    }

    #[test]
    fn default_base_urls_are_sane() {
        assert!(ProviderKind::Zhipu.default_base_url().contains("bigmodel"));
        assert!(ProviderKind::Anthropic
            .default_base_url()
            .contains("anthropic"));
    }
}
