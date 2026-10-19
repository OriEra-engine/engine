//! L7 model provider boundary (book T10).
//!
//! The engine — not the application — owns model access. Upper layers
//! (L9 agents, L11 apps) talk to [`ModelProvider`] through the inference
//! runtime; they never see HTTP, keys or model names. Browser/wasm builds
//! implement this trait over `fetch` (see examples/11-ai-cafe engine
//! kernel); native builds get the blocking HTTP adapter behind the
//! `native-http` feature (#68, design discussions/78). `RuleProvider`
//! is the deterministic offline backend that keeps tests hermetic and
//! lets the engine run without a key.

use oriera_common::TokenUsage;
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
///
/// `Network`/`Timeout`/`Status`/`Parse` are the native-HTTP adapter's
/// classification (#68, design discussions/78): the distinction is
/// actionable — Network means "never reached the far side", Timeout
/// means "the far side is alive but slow" (a budget signal, not a bug),
/// Status carries the provider's answer (4xx/5xx + scrubbed body
/// snippet), Parse means the provider answered garbage (a bug, never
/// retried). `Provider` stays as the catch-all for config-level and
/// backend-specific failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferError {
    /// The provider refused / errored / timed out.
    Provider(String),
    /// Connection never made it to the far side (DNS / TCP / TLS).
    /// Never carries key material.
    Network(String),
    /// The far side was alive but slow — the request hit its deadline.
    /// This is a compute-budget signal, not a fault to hide.
    Timeout,
    /// The provider answered with a non-2xx status. `detail` is a
    /// redacted body snippet (plus `retry-after:` when the provider
    /// sent one) — api keys are scrubbed before it is built.
    Status { code: u16, detail: String },
    /// 2xx but the payload did not parse — never retried; surface it.
    Parse(String),
}

impl fmt::Display for InferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InferError::Provider(m) => write!(f, "provider error: {m}"),
            InferError::Network(m) => write!(f, "network unreachable: {m}"),
            InferError::Timeout => write!(f, "timeout: far side alive but slow (budget signal)"),
            InferError::Status { code, detail } => write!(f, "http {code}: {detail}"),
            InferError::Parse(m) => write!(f, "malformed provider payload: {m}"),
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

    /// Token usage of the most recent [`ModelProvider::complete`] call.
    /// Provided (zero) so simple backends opt in only when they can
    /// measure; HTTP providers override with the API's usage block.
    fn last_usage(&self) -> TokenUsage {
        TokenUsage::ZERO
    }
}

/// Deterministic offline backend: picks a canned line by keyword.
/// Same input → same output, so engine tests are hermetic.
pub struct RuleProvider {
    pub id: &'static str,
    usage: std::sync::Mutex<TokenUsage>,
}

impl Default for RuleProvider {
    fn default() -> Self {
        Self {
            id: "rule-v1",
            usage: std::sync::Mutex::new(TokenUsage::ZERO),
        }
    }
}

impl RuleProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deterministic stand-in for tokenizer output: chars/4 per side.
    fn estimate(prompt_chars: usize, completion_chars: usize) -> TokenUsage {
        TokenUsage {
            prompt: (prompt_chars as u64) / 4,
            completion: (completion_chars as u64) / 4,
        }
    }
}

impl ModelProvider for RuleProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn complete(&self, messages: &[ChatMessage]) -> Result<String, InferError> {
        let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
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
        }
        .to_string();
        let completion_chars = say.chars().count();
        if let Ok(mut u) = self.usage.lock() {
            *u = Self::estimate(prompt_chars, completion_chars);
        }
        Ok(say)
    }

    fn last_usage(&self) -> TokenUsage {
        self.usage.lock().map(|u| *u).unwrap_or(TokenUsage::ZERO)
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

    #[test]
    fn rule_provider_reports_usage_after_each_call() {
        let p = RuleProvider::new();
        assert_eq!(p.last_usage(), TokenUsage::ZERO);
        let prompt = "聊聊咖啡豆豆"; // 6 chars → estimate 6/4 = 1 token
        let out = p.complete(&[ChatMessage::user(prompt)]).unwrap();
        assert_eq!(
            p.last_usage(),
            TokenUsage {
                prompt: 1,
                completion: (out.chars().count() / 4) as u64
            }
        );
        assert!(p.last_usage().total() > 0);
    }

    #[test]
    fn default_last_usage_is_zero_for_minimal_backends() {
        struct Bare;
        impl ModelProvider for Bare {
            fn id(&self) -> &str {
                "bare"
            }
            fn complete(&self, _m: &[ChatMessage]) -> Result<String, InferError> {
                Ok(String::new())
            }
        }
        assert_eq!(Bare.last_usage(), TokenUsage::ZERO);
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
            "openai-compat" | "openai" | "deepseek" | "moonshot" | "groq" | "openrouter"
            | "qwen" | "ollama" | "vllm" | "llamacpp" | "lmstudio" => {
                Some(ProviderKind::OpenAiCompat)
            }
            "anthropic" => Some(ProviderKind::Anthropic),
            "rule" => Some(ProviderKind::Rule),
            _ => None,
        }
    }
}

/// One row of the provider preset table. Mirrors the browser kernel's
/// `PROVIDER_PRESETS` (examples/11-ai-cafe/engine/kernel.js) and extends
/// it with the open-source local serving stack — the "geek rows": the
/// same table drives the Studio model center, the Rust-side smoke
/// harness and the free-tier matrix (docs/OPEN-MODEL-MATRIX.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderPreset {
    /// Stable id used in configs, env vars and UI rows.
    pub id: &'static str,
    /// Human label for the model center.
    pub label: &'static str,
    pub kind: ProviderKind,
    /// Suggested first model (free tier where one exists).
    pub model: &'static str,
    /// Default base URL; empty for the offline rule row.
    pub base_url: &'static str,
    /// `true` for local open-source serving (no key, no egress): the
    /// rows the companion/edu tracks pin for privacy and compliance.
    pub local: bool,
}

/// The preset table. Order is the model-center display order: offline
/// row first (never fails), then cloud vendors, then local open-source
/// rows. Cloud rows are **reserved interfaces** — the big-vendor API
/// endpoints are pinned here so plugging a vendor key is a one-field
/// edit; local rows point at the standard OpenAI-compatible endpoints
/// of ollama / vLLM / llama.cpp / LM Studio.
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    // ── offline ──
    ProviderPreset {
        id: "rule",
        label: "离线规则引擎",
        kind: ProviderKind::Rule,
        model: "rule-v1",
        base_url: "",
        local: true,
    },
    // ── cloud vendors (reserved API interfaces; key in the shell) ──
    ProviderPreset {
        id: "zhipu",
        label: "智谱 GLM",
        kind: ProviderKind::Zhipu,
        model: "glm-4-flash",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        local: false,
    },
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        kind: ProviderKind::OpenAiCompat,
        model: "gpt-4o-mini",
        base_url: "https://api.openai.com/v1",
        local: false,
    },
    ProviderPreset {
        id: "qwen",
        label: "Qwen 通义",
        kind: ProviderKind::OpenAiCompat,
        model: "qwen-plus",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        local: false,
    },
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek",
        kind: ProviderKind::OpenAiCompat,
        model: "deepseek-chat",
        base_url: "https://api.deepseek.com/v1",
        local: false,
    },
    ProviderPreset {
        id: "moonshot",
        label: "Moonshot Kimi",
        kind: ProviderKind::OpenAiCompat,
        model: "moonshot-v1-8k",
        base_url: "https://api.moonshot.cn/v1",
        local: false,
    },
    ProviderPreset {
        id: "groq",
        label: "Groq",
        kind: ProviderKind::OpenAiCompat,
        model: "llama-3.1-8b-instant",
        base_url: "https://api.groq.com/openai/v1",
        local: false,
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        kind: ProviderKind::OpenAiCompat,
        model: "openrouter/auto",
        base_url: "https://openrouter.ai/api/v1",
        local: false,
    },
    ProviderPreset {
        id: "anthropic",
        label: "Anthropic",
        kind: ProviderKind::Anthropic,
        model: "claude-3-5-haiku-latest",
        base_url: "https://api.anthropic.com/v1",
        local: false,
    },
    // ── local open-source serving (no key, no egress) ──
    ProviderPreset {
        id: "ollama",
        label: "Ollama 本地",
        kind: ProviderKind::OpenAiCompat,
        model: "qwen3:8b",
        base_url: "http://127.0.0.1:11434/v1",
        local: true,
    },
    ProviderPreset {
        id: "vllm",
        label: "vLLM 本地",
        kind: ProviderKind::OpenAiCompat,
        model: "Qwen/Qwen3-8B",
        base_url: "http://127.0.0.1:8000/v1",
        local: true,
    },
    ProviderPreset {
        id: "llamacpp",
        label: "llama.cpp 本地",
        kind: ProviderKind::OpenAiCompat,
        model: "glm-4-9b-chat",
        base_url: "http://127.0.0.1:8080/v1",
        local: true,
    },
    ProviderPreset {
        id: "lmstudio",
        label: "LM Studio 本地",
        kind: ProviderKind::OpenAiCompat,
        model: "minicpm-v",
        base_url: "http://127.0.0.1:1234/v1",
        local: true,
    },
];

impl ProviderConfig {
    /// Build a config from a preset row (`id` matches
    /// [`ProviderPreset::id`]). The API key is left empty — the shell
    /// is the only place a key ever lives; local rows never need one.
    pub fn preset(id: &str) -> Option<ProviderConfig> {
        PROVIDER_PRESETS
            .iter()
            .find(|p| p.id == id)
            .map(|p| ProviderConfig {
                kind: p.kind,
                model: p.model.to_string(),
                api_key: String::new(),
                base_url: if p.base_url.is_empty() {
                    None
                } else {
                    Some(p.base_url.to_string())
                },
            })
    }
}

/// Resolve a creator's choice into a provider.
///
/// With the `native-http` feature (#68, design discussions/78) remote
/// kinds get the blocking HTTP adapter (ureq + rustls, 20s/5s timeouts,
/// key only from the shell). Without the feature — the default — remote
/// kinds resolve to the rule backend with the choice recorded for
/// observability, and the browser kernel (kernel.js `createProvider`)
/// remains the mirror implementation of this factory.
/// [`create_provider`] with wire options (v2.1 kernel-v2 parity):
/// seed / thinking / json_mode for the native HTTP adapter. Without
/// the `native-http` feature this behaves exactly like
/// [`create_provider`] (options are recorded but there is no wire).
/// Construction-time wire options for the native HTTP adapter
/// (v2.1, kernel-v2 parity): reproducibility seed, thinking-mode
/// control and JSON mode — the browser kernel passes these per call
/// (`opts.seed/opts.thinking/opts.json`); the Rust trait's
/// `complete(&self, messages)` has no per-call channel, so they live
/// at construction here. OpenAI-compatible dialect only; Anthropic
/// requests are untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderOptions {
    /// Provider-side sampling seed (reproducibility; best-effort —
    /// vendors may still drift, see the twin demo's honesty note).
    pub seed: Option<u64>,
    /// Send `thinking: {"type":"disabled"}` — reasoning models
    /// (DeepSeek-V4 default-high) otherwise burn max_tokens into
    /// hidden reasoning and return empty/short bodies.
    pub thinking_disabled: bool,
    /// Send `response_format: {"type":"json_object"}`.
    pub json_mode: bool,
}

pub fn create_provider_with_options(
    cfg: &ProviderConfig,
    opts: &ProviderOptions,
) -> std::sync::Arc<dyn ModelProvider> {
    match cfg.kind {
        ProviderKind::Rule => std::sync::Arc::new(RuleProvider::new()),
        #[cfg(feature = "native-http")]
        kind => {
            tracing::info!(
                provider = kind.id(),
                model = %cfg.model,
                "L7 native HTTP provider armed with wire options"
            );
            std::sync::Arc::new(crate::http::HttpProvider::with_options(
                cfg.clone(),
                opts.clone(),
            ))
        }
        #[cfg(not(feature = "native-http"))]
        kind => {
            let _ = opts; // options take effect once native-http lands
            tracing::info!(
                provider = kind.id(),
                model = %cfg.model,
                "L7 remote provider selected; options recorded, native HTTP adapter pending"
            );
            std::sync::Arc::new(RuleProvider::new())
        }
    }
}

pub fn create_provider(cfg: &ProviderConfig) -> std::sync::Arc<dyn ModelProvider> {
    match cfg.kind {
        ProviderKind::Rule => std::sync::Arc::new(RuleProvider::new()),
        #[cfg(feature = "native-http")]
        kind => {
            tracing::info!(
                provider = kind.id(),
                model = %cfg.model,
                "L7 native HTTP provider armed (feature native-http)"
            );
            std::sync::Arc::new(crate::http::HttpProvider::new(cfg.clone()))
        }
        #[cfg(not(feature = "native-http"))]
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

#[cfg(test)]
mod preset_tests {
    use super::*;

    #[test]
    fn preset_table_is_well_formed() {
        let mut ids = PROVIDER_PRESETS.iter().map(|p| p.id).collect::<Vec<_>>();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "preset ids must be unique");
        // The offline row leads, and every cloud row pins a base URL.
        assert_eq!(PROVIDER_PRESETS[0].id, "rule");
        for p in PROVIDER_PRESETS {
            if !p.local {
                assert!(!p.base_url.is_empty(), "{} needs a base URL", p.id);
            }
        }
    }

    #[test]
    fn kernel_js_parity_plus_geek_rows() {
        // The eight rows the browser kernel serves (kernel.js
        // PROVIDER_PRESETS) all resolve, plus the qwen reserved row and
        // the four local open-source serving rows.
        for id in [
            "rule",
            "zhipu",
            "openai",
            "deepseek",
            "moonshot",
            "groq",
            "openrouter",
            "anthropic",
        ] {
            assert!(
                ProviderConfig::preset(id).is_some(),
                "kernel row {id} missing"
            );
        }
        for id in ["qwen", "ollama", "vllm", "llamacpp", "lmstudio"] {
            assert!(
                ProviderConfig::preset(id).is_some(),
                "geek row {id} missing"
            );
        }
        assert_eq!(PROVIDER_PRESETS.len(), 13);
    }

    #[test]
    fn preset_builds_a_config_with_no_key_inside() {
        let cfg = ProviderConfig::preset("ollama").unwrap();
        assert_eq!(cfg.kind, ProviderKind::OpenAiCompat);
        assert_eq!(cfg.model, "qwen3:8b");
        assert_eq!(cfg.base_url.as_deref(), Some("http://127.0.0.1:11434/v1"));
        assert!(cfg.api_key.is_empty(), "presets never carry a key");

        let qwen = ProviderConfig::preset("qwen").unwrap();
        assert_eq!(
            qwen.base_url.as_deref(),
            Some("https://dashscope.aliyuncs.com/compatible-mode/v1")
        );

        let rule = ProviderConfig::preset("rule").unwrap();
        assert_eq!(rule.kind, ProviderKind::Rule);
        assert!(rule.base_url.is_none());
        assert!(ProviderConfig::preset("nope").is_none());
    }

    #[test]
    fn local_rows_are_flagged_for_privacy_pinning() {
        let local: Vec<_> = PROVIDER_PRESETS
            .iter()
            .filter(|p| p.local)
            .map(|p| p.id)
            .collect();
        assert_eq!(local, ["rule", "ollama", "vllm", "llamacpp", "lmstudio"]);
    }
}
