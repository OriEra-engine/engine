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
    /// role=Tool:这帧内容是哪次调用的结果(多轮工具循环的粘合剂;
    /// 其他角色恒 None,构造子保证)。
    pub tool_call_id: Option<String>,
    /// role=Assistant:该轮请求过的工具调用历史,格式为引擎中立的
    /// JSON 数组 `[{"id","name","arguments_json"}]`(即 `Vec<ToolCall>`
    /// 的字段形状)。HTTP 方言在 build 时重排为厂商 wire 原文 ——
    /// OpenAI 要求 tool 消息前的 assistant 消息携带 tool_calls,
    /// Anthropic 要求 tool_use/tool_result 块,这里一并提供。
    pub tool_calls_json: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls_json: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls_json: None,
        }
    }
    /// Assistant 的纯文本轮。
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls_json: None,
        }
    }
    /// Assistant 的工具调用轮:`calls_json` 见字段文档。
    pub fn assistant_with_tools(calls_json: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: String::new(),
            tool_call_id: None,
            tool_calls_json: Some(calls_json.into()),
        }
    }
    /// 回传一次工具调用的执行结果。
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(call_id.into()),
            tool_calls_json: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    /// 工具结果帧(多轮工具循环);HTTP 方言映射为 OpenAI `tool` /
    /// Anthropic `tool_result`。
    Tool,
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
    /// The LOCAL concurrency cap ([`crate::InferJobs`]) is full — this
    /// frame chose not to enqueue another call. Not a provider fault at
    /// all; callers handle it structurally ("try again next beat"),
    /// never by retry loops.
    Busy,
}

impl fmt::Display for InferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InferError::Provider(m) => write!(f, "provider error: {m}"),
            InferError::Network(m) => write!(f, "network unreachable: {m}"),
            InferError::Timeout => write!(f, "timeout: far side alive but slow (budget signal)"),
            InferError::Status { code, detail } => write!(f, "http {code}: {detail}"),
            InferError::Parse(m) => write!(f, "malformed provider payload: {m}"),
            InferError::Busy => write!(
                f,
                "busy: local inference concurrency cap reached, skipped this beat"
            ),
        }
    }
}

impl std::error::Error for InferError {}

/// Vendor-agnostic error class (RFC-3 partner review, P0 item 1):
/// routing tables, host logs and partner dashboards fold every failure
/// into one of these public buckets instead of vendor-specific codes.
/// The mapping is derived from transport facts only — it can never
/// leak key material because `Status::detail` is scrubbed upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// HTTP 429 / provider busy — back off, then retry or fall through
    /// the slot's chain.
    RateLimited,
    /// The provider refused the content (policy / 合规). Not retryable
    /// with the same payload; degrade, never crash.
    ContentModeration,
    /// 401/403 — key invalid or lacks scope; a config problem.
    Auth,
    /// 402 / billing — quota or balance exhausted.
    Quota,
    /// 5xx — provider-side fault; safe to fall through the chain.
    Server,
    /// DNS/TCP/TLS never reached the far side.
    Network,
    /// Deadline hit; a compute-budget signal, not a fault.
    Timeout,
    /// Everything else (parse garbage, config errors, unclassified 4xx).
    Provider,
}

impl ErrorClass {
    /// Stable id for metrics labels (`oriera_infer_error{class=...}`).
    pub fn id(self) -> &'static str {
        match self {
            ErrorClass::RateLimited => "rate_limited",
            ErrorClass::ContentModeration => "content_moderation",
            ErrorClass::Auth => "auth",
            ErrorClass::Quota => "quota",
            ErrorClass::Server => "server",
            ErrorClass::Network => "network",
            ErrorClass::Timeout => "timeout",
            ErrorClass::Provider => "provider",
        }
    }

    /// Retry/fallback policy primitive (RFC 0002 P0①): transport-transient
    /// classes are worth backing off and retrying — or falling through the
    /// slot's chain; policy refusals and config/billing failures are not,
    /// because retrying the same payload cannot change the outcome.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            ErrorClass::RateLimited
                | ErrorClass::Server
                | ErrorClass::Network
                | ErrorClass::Timeout
        )
    }
}

impl InferError {
    /// Fold into the public [`ErrorClass`]. Only HTTP status plus
    /// scrubbed-detail keywords participate — vendor-specific codes are
    /// mapped to `Status` inside adapters, so this stays stable across
    /// providers.
    pub fn class(&self) -> ErrorClass {
        match self {
            InferError::Timeout => ErrorClass::Timeout,
            InferError::Network(_) => ErrorClass::Network,
            // Busy is our own cap, not the far side's — folded into the
            // local-catch-all bucket so partner metrics never blame a
            // vendor for our throttle. Handled structurally by callers.
            InferError::Busy | InferError::Parse(_) | InferError::Provider(_) => {
                ErrorClass::Provider
            }
            InferError::Status { code, detail } => match code {
                429 => ErrorClass::RateLimited,
                401 | 403 => ErrorClass::Auth,
                402 => ErrorClass::Quota,
                500..=599 => ErrorClass::Server,
                400..=499 if detail_says_moderation(detail) => ErrorClass::ContentModeration,
                _ => ErrorClass::Provider,
            },
        }
    }
}

/// Keyword probe on the already-redacted `Status::detail`; catches the
/// common policy-refusal phrasings (incl. the 1301-style vendor codes
/// surfaced in bodies) without coupling to any single vendor.
fn detail_says_moderation(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    ["content", "moderation", "safety", "敏感", "违规", "1301"]
        .iter()
        .any(|k| d.contains(k))
}

/// One capability the engine offers the model (RFC-3 tool calling).
/// `parameters_json` is a JSON Schema the provider forwards verbatim —
/// the engine never interprets vendor tool formats itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters_json: String,
}

/// A tool invocation the model requested. `id` rounds back to the
/// vendor as the tool-result message id (multi-turn tool loops).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

/// Result of a tool-enabled completion: prose, or the model asking the
/// engine to act (then the caller executes, appends a tool result and
/// may complete again).
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

/// The model boundary every consumer depends on (L7 contract,
/// `oriera.infer.provider.v1`).
pub trait ModelProvider: Send + Sync {
    /// Stable provider id for metrics (`oriera_infer_provider{...}`).
    fn id(&self) -> &str;

    /// Produce one assistant completion for the chat so far.
    fn complete(&self, messages: &[ChatMessage]) -> Result<String, InferError>;

    /// Does this backend REALLY honor the tools vocabulary? Default
    /// false — an honest "no" so callers can surface a degradation
    /// instead of silently losing agency.
    fn supports_tools(&self) -> bool {
        false
    }

    /// Tool-enabled completion (RFC-3). Default: tools are ignored and
    /// the call degrades to plain text — callers should check
    /// [`ModelProvider::supports_tools`] first to make that visible.
    fn complete_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<ToolOutcome, InferError> {
        let _ = tools;
        Ok(ToolOutcome::Text(self.complete(messages)?))
    }

    /// Streaming surface (RFC-3): the reply split into arrival-order
    /// chunks. The default returns the whole reply as ONE chunk — an
    /// honest "this backend does not stream" signal, not a fake stream;
    /// consumers render `len() == 1` as a plain message. Browser kernels
    /// stream via `fetch` SSE on their side; native SSE lands with the
    /// async HTTP adapter (ROADMAP). Chunks always concatenate to the
    /// `complete()` reply.
    fn complete_stream(&self, messages: &[ChatMessage]) -> Result<Vec<String>, InferError> {
        Ok(vec![self.complete(messages)?])
    }

    /// Token usage of the most recent [`ModelProvider::complete`] call.
    /// Provided (zero) so simple backends opt in only when they can
    /// measure; HTTP providers override with the API's usage block.
    fn last_usage(&self) -> TokenUsage {
        TokenUsage::ZERO
    }

    /// Why this provider is a stand-in rather than the configured
    /// backend, if it is one. **Degrade must be visible**: hosts surface
    /// this in status UI and metrics instead of letting a silent
    /// fallback pass for the real model (AI-NATIVE: absence degrades,
    /// but never silently).
    fn degradation_note(&self) -> Option<&str> {
        None
    }
}

/// Deterministic offline backend: picks a canned line by keyword.
/// Same input → same output, so engine tests are hermetic.
pub struct RuleProvider {
    pub id: &'static str,
    usage: std::sync::Mutex<TokenUsage>,
    /// Set when this instance stands in for a configured remote provider
    /// that this build cannot serve (native-http feature off) — surfaced
    /// via [`ModelProvider::degradation_note`].
    degrade_note: Option<String>,
}

impl Default for RuleProvider {
    fn default() -> Self {
        Self {
            id: "rule-v1",
            usage: std::sync::Mutex::new(TokenUsage::ZERO),
            degrade_note: None,
        }
    }
}

impl RuleProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// A rule backend standing in for the configured `from` provider;
    /// `degradation_note()` tells hosts they are not talking to the
    /// configured model.
    pub fn degraded(from: &str) -> Self {
        Self {
            degrade_note: Some(format!(
                "configured provider '{from}' unavailable in this build \
                 (native-http feature off); serving deterministic rule backend"
            )),
            ..Self::default()
        }
    }

    /// Deterministic stand-in for tokenizer output: chars/4 per side.
    fn estimate(prompt_chars: usize, completion_chars: usize) -> TokenUsage {
        TokenUsage {
            prompt: (prompt_chars as u64) / 4,
            completion: (completion_chars as u64) / 4,
            cached: 0, // offline backend has no provider cache to hit
        }
    }
}

impl ModelProvider for RuleProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn degradation_note(&self) -> Option<&str> {
        self.degrade_note.as_deref()
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

    /// Simulated stream: the canned reply split at sentence enders, so
    /// typewriter UI paths are exercised offline deterministically.
    /// Concatenation is byte-identical to `complete()`.
    fn complete_stream(&self, messages: &[ChatMessage]) -> Result<Vec<String>, InferError> {
        let reply = self.complete(messages)?;
        let mut chunks = Vec::new();
        let mut buf = String::new();
        for ch in reply.chars() {
            buf.push(ch);
            if matches!(ch, '。' | '！' | '？' | '；' | '\n') {
                chunks.push(std::mem::take(&mut buf));
            }
        }
        if !buf.is_empty() {
            chunks.push(buf);
        }
        Ok(if chunks.is_empty() {
            vec![reply]
        } else {
            chunks
        })
    }

    /// The rule backend simulates tool calling deterministically: if
    /// the last message contains `工具名 {json}` (e.g. `spawn {"count":3}`),
    /// that tool is "requested" with those arguments. Same input →
    /// same tool calls, so engine-side tool loops stay hermetic offline.
    fn supports_tools(&self) -> bool {
        true
    }

    fn complete_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<ToolOutcome, InferError> {
        // 多轮循环:Tool 结果帧不参与判定,轮末上下文是 assistant
        // 的调用摘要 —— 第二轮没有标记即自然收敛为纯文本。
        let last = messages
            .iter()
            .rev()
            .find(|m| m.role != Role::Tool)
            .map(|m| m.content.as_str())
            .unwrap_or("");
        for tool in tools {
            let Some(open) = last.find(&format!("{} {{", tool.name)) else {
                continue;
            };
            // marker = "name {":跳过名字与那个空格,落在 '{' 上
            let args_start = open + tool.name.len() + 1;
            let Some(args) = take_json_object(&last[args_start..]) else {
                continue;
            };
            // Meter the simulated round trip, mirroring complete().
            let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
            let completion_chars = tool.name.chars().count() + args.chars().count();
            if let Ok(mut u) = self.usage.lock() {
                *u = Self::estimate(prompt_chars, completion_chars);
            }
            return Ok(ToolOutcome::ToolCalls(vec![ToolCall {
                id: format!("call-{}", tool.name),
                name: tool.name.clone(),
                arguments_json: args,
            }]));
        }
        Ok(ToolOutcome::Text(self.complete(messages)?))
    }
}

/// Extract the first balanced `{...}` JSON object from `s` (starts at
/// the `{`), tolerating nested braces and escaped quotes.
fn take_json_object(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, &c) in b.iter().enumerate() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            b'\\' if in_str => esc = true,
            b'"' => in_str = !in_str,
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_classes_fold_transport_facts() {
        let status = |code: u16, detail: &str| InferError::Status {
            code,
            detail: detail.into(),
        };
        assert_eq!(status(429, "busy").class(), ErrorClass::RateLimited);
        assert_eq!(status(401, "bad key").class(), ErrorClass::Auth);
        assert_eq!(status(403, "forbidden").class(), ErrorClass::Auth);
        assert_eq!(status(402, "balance").class(), ErrorClass::Quota);
        assert_eq!(status(500, "boom").class(), ErrorClass::Server);
        assert_eq!(status(503, "down").class(), ErrorClass::Server);
        assert_eq!(
            status(400, "prompt violates content policy").class(),
            ErrorClass::ContentModeration
        );
        assert_eq!(
            status(400, "1301: 敏感内容").class(),
            ErrorClass::ContentModeration
        );
        assert_eq!(status(404, "no such model").class(), ErrorClass::Provider);
        assert_eq!(InferError::Timeout.class(), ErrorClass::Timeout);
        assert_eq!(
            InferError::Network("dns".into()).class(),
            ErrorClass::Network
        );
        assert_eq!(
            InferError::Parse("garbage".into()).class(),
            ErrorClass::Provider
        );
        assert_eq!(
            InferError::Provider("cfg".into()).class(),
            ErrorClass::Provider
        );
    }

    #[test]
    fn rule_provider_simulates_tool_calls_deterministically() {
        let spawn = ToolDef {
            name: "spawn".into(),
            description: "生成实体".into(),
            parameters_json: r#"{"type":"object"}"#.into(),
        };
        let p = RuleProvider::new();
        // 消息里带 `工具名 {json}` → 请求该工具
        let out = p
            .complete_with_tools(
                &[ChatMessage::user("导演说:spawn {\"count\":3},再来点气氛")],
                std::slice::from_ref(&spawn),
            )
            .unwrap();
        match out {
            ToolOutcome::ToolCalls(c) => {
                assert_eq!(c.len(), 1);
                assert_eq!(c[0].name, "spawn");
                assert_eq!(c[0].arguments_json, "{\"count\":3}");
            }
            other => panic!("expected tool call, got {other:?}"),
        }
        // 没有标记 → 纯文本(降级回 complete 的罐头回复)
        let plain = p
            .complete_with_tools(&[ChatMessage::user("聊聊咖啡豆")], &[spawn])
            .unwrap();
        assert!(matches!(plain, ToolOutcome::Text(_)));
        assert!(p.supports_tools());
    }

    #[test]
    fn default_provider_reports_no_tool_support_honestly() {
        struct Plain;
        impl ModelProvider for Plain {
            fn id(&self) -> &str {
                "plain"
            }
            fn complete(&self, _m: &[ChatMessage]) -> Result<String, InferError> {
                Ok("文本".into())
            }
        }
        assert!(!Plain.supports_tools());
        // 默认实现:忽略工具词表,不伪造工具能力
        let out = Plain
            .complete_with_tools(&[ChatMessage::user("x")], &[])
            .unwrap();
        assert_eq!(out, ToolOutcome::Text("文本".into()));
    }

    #[test]
    fn error_class_ids_are_stable_and_snake_case() {
        for (class, id) in [
            (ErrorClass::RateLimited, "rate_limited"),
            (ErrorClass::ContentModeration, "content_moderation"),
            (ErrorClass::Auth, "auth"),
            (ErrorClass::Quota, "quota"),
            (ErrorClass::Server, "server"),
            (ErrorClass::Network, "network"),
            (ErrorClass::Timeout, "timeout"),
            (ErrorClass::Provider, "provider"),
        ] {
            assert_eq!(class.id(), id);
        }
    }

    #[test]
    fn retry_policy_splits_transient_from_permanent() {
        for transient in [
            ErrorClass::RateLimited,
            ErrorClass::Server,
            ErrorClass::Network,
            ErrorClass::Timeout,
        ] {
            assert!(transient.retryable(), "{transient:?} should be retryable");
        }
        for permanent in [
            ErrorClass::ContentModeration,
            ErrorClass::Auth,
            ErrorClass::Quota,
            ErrorClass::Provider,
        ] {
            assert!(!permanent.retryable(), "{permanent:?} must not retry");
        }
    }

    #[test]
    fn degrade_is_visible_not_silent() {
        // 规则后端本身没有降级说明
        assert_eq!(RuleProvider::new().degradation_note(), None);
        // 顶替远端厂商时必须可查询到降级事实(含被顶替者 id)
        let stand_in = RuleProvider::degraded("zhipu");
        let note = stand_in
            .degradation_note()
            .expect("degraded must be visible");
        assert!(note.contains("zhipu"));
        assert!(note.contains("rule backend"));
        // 普通后端走默认实现:None
        struct Bare;
        impl ModelProvider for Bare {
            fn id(&self) -> &str {
                "bare"
            }
            fn complete(&self, _m: &[ChatMessage]) -> Result<String, InferError> {
                Ok(String::new())
            }
        }
        assert_eq!(Bare.degradation_note(), None);
    }

    #[cfg(not(feature = "native-http"))]
    #[test]
    fn factory_degraded_provider_carries_the_note() {
        let cfg = ProviderConfig {
            kind: ProviderKind::Zhipu,
            model: "glm-4-flash".into(),
            api_key: String::new(),
            base_url: None,
        };
        let p = create_provider(&cfg);
        assert!(p.degradation_note().unwrap_or_default().contains("zhipu"));
    }

    #[test]
    fn rule_provider_streams_sentence_chunks_that_concatenate() {
        let p = RuleProvider::new();
        // 「引擎」关键词的罐头回复含 ！ 与 。 两个句末 → 两块。
        let chunks = p
            .complete_stream(&[ChatMessage::user("你们引擎做得怎么样")])
            .unwrap();
        assert!(
            chunks.len() >= 2,
            "expected 2 sentence chunks, got {chunks:?}"
        );
        assert_eq!(
            chunks.concat(),
            p.complete(&[ChatMessage::user("你们引擎做得怎么样")])
                .unwrap()
        );
    }

    #[test]
    fn default_stream_is_one_honest_chunk() {
        struct Single;
        impl ModelProvider for Single {
            fn id(&self) -> &str {
                "single"
            }
            fn complete(&self, _m: &[ChatMessage]) -> Result<String, InferError> {
                Ok("一句话".into())
            }
        }
        let chunks = Single.complete_stream(&[]).unwrap();
        assert_eq!(chunks.len(), 1, "default surface must not fake a stream");
        assert_eq!(chunks[0], "一句话");
    }

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
                completion: (out.chars().count() / 4) as u64,
                cached: 0,
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
            | "qwen" | "stepfun" | "ollama" | "vllm" | "llamacpp" | "lmstudio" => {
                Some(ProviderKind::OpenAiCompat)
            }
            "anthropic" => Some(ProviderKind::Anthropic),
            "rule" => Some(ProviderKind::Rule),
            _ => None,
        }
    }
}

/// What a preset row's endpoint can do, declared per row (v0.7,
/// PLAN-deepseek-upgrade A1). Data, not behavior: B6 (structured
/// output), B4 (memory budgeting by context window) and B8 (the model
/// bench) read this instead of hardcoding vendor facts. Conservative
/// by default — `false`/`None` means "unknown, don't assume", so a
/// wrong row degrades a feature visibly instead of misfiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelCaps {
    /// Context window in tokens, when the row's default model has a
    /// published one. `None` = not registered (yet).
    pub context_window: Option<u64>,
    /// Accepts `response_format: json_schema` (B6 structured output).
    pub structured_output: bool,
    /// Accepts tool/function definitions (the B1 reserved contract).
    pub tools: bool,
    /// Accepts image input (L10's future entry point).
    pub image_input: bool,
    /// Reports prompt-cache hits and bills them cheaper — the B5
    /// stable-prefix convention pays off only on these rows.
    pub prompt_cache: bool,
}

impl ModelCaps {
    /// The unregistered row: nothing assumed. Const twin of
    /// `default()` for use inside the `PROVIDER_PRESETS` table.
    pub const UNREGISTERED: ModelCaps = ModelCaps {
        context_window: None,
        structured_output: false,
        tools: false,
        image_input: false,
        prompt_cache: false,
    };
}

/// Registered price for a preset row (v0.7, B5 cost accounting):
/// micro-currency units (= 1e-6 of `currency`) **per 1M tokens**.
/// Integers, so `ProviderPreset` stays `Eq`. Registered entries carry
/// a date in the table comment; prices drift, treat rows as dated
/// snapshots and re-verify at B8 (model bench) time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPrice {
    /// ISO currency code of all three fields ("USD", "CNY", …).
    pub currency: &'static str,
    /// Cache-miss prompt price, µ-currency per 1M tokens.
    pub prompt_micro_per_m: u64,
    /// Completion price, µ-currency per 1M tokens.
    pub completion_micro_per_m: u64,
    /// Cache-hit prompt price, µ-currency per 1M tokens.
    pub cached_micro_per_m: u64,
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
    /// Declared endpoint capabilities (v0.7 A1) — see [`ModelCaps`].
    pub caps: ModelCaps,
    /// Registered price (v0.7 B5); `None` = 牌价待登记 (cost meters
    /// count the call, price it as unpriced).
    pub price: Option<ModelPrice>,
}

/// The preset table. Order is the model-center display order: offline
/// row first (never fails), then cloud vendors, then local open-source
/// rows. Cloud rows are **reserved interfaces** — the big-vendor API
/// endpoints are pinned here so plugging a vendor key is a one-field
/// edit; local rows point at the standard OpenAI-compatible endpoints
/// of ollama / vLLM / llama.cpp / LM Studio.
///
/// Caps/price registration policy (v0.7 A1): rows get `caps` only for
/// facts this project verified (deepseek: the V4.1-Flash /v1/models
/// market check + official pricing page 2026-10) or that are safe
/// defaults; unregistered rows read `ModelCaps::default()` (all
/// conservative) and `price: None` (成本计量照走，计为 unpriced).
/// Local rows' structured-output flag follows each server's own docs
/// and drifts with the endpoint version — trust the endpoint's answer
/// over the table.
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    // ── offline ──
    ProviderPreset {
        id: "rule",
        label: "离线规则引擎",
        kind: ProviderKind::Rule,
        model: "rule-v1",
        base_url: "",
        local: true,
        caps: ModelCaps::UNREGISTERED,
        price: None,
    },
    // ── cloud vendors (reserved API interfaces; key in the shell) ──
    ProviderPreset {
        id: "zhipu",
        label: "智谱 GLM",
        kind: ProviderKind::Zhipu,
        model: "glm-4-flash",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        local: false,
        caps: ModelCaps {
            tools: true,
            ..ModelCaps::UNREGISTERED
        },
        price: None, // 牌价待登记（B8 核价）
    },
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        kind: ProviderKind::OpenAiCompat,
        model: "gpt-4o-mini",
        base_url: "https://api.openai.com/v1",
        local: false,
        caps: ModelCaps {
            context_window: Some(128_000),
            structured_output: true,
            tools: true,
            image_input: true,
            prompt_cache: true,
        },
        // 登记牌价（2026-10 检索 OpenAI 公开 pricing 页，gpt-4o-mini 一口价
        // 无峰谷）：miss $0.15 / hit $0.075 / out $0.60 每百万 tokens，B8 复核。
        price: Some(ModelPrice {
            currency: "USD",
            prompt_micro_per_m: 150_000,     // $0.15 / 1M miss
            completion_micro_per_m: 600_000, // $0.60 / 1M
            cached_micro_per_m: 75_000,      // $0.075 / 1M hit
        }),
    },
    ProviderPreset {
        id: "qwen",
        label: "Qwen 通义",
        kind: ProviderKind::OpenAiCompat,
        model: "qwen-plus",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        local: false,
        caps: ModelCaps::UNREGISTERED, // 待登记
        price: None,
    },
    ProviderPreset {
        id: "stepfun",
        label: "阶跃 Step",
        kind: ProviderKind::OpenAiCompat,
        model: "step-3.7-flash",
        base_url: "https://api.stepfun.com/v1",
        local: false,
        caps: ModelCaps::UNREGISTERED, // 待登记
        price: None,
    },
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek",
        kind: ProviderKind::OpenAiCompat,
        model: "deepseek-chat",
        base_url: "https://api.deepseek.com/v1",
        local: false,
        caps: ModelCaps {
            // deepseek-chat = V4.1-Flash 别名：1M 上下文 + 图像输入
            // （2026-10-19 /v1/models 行情实测，aigame 会话）。
            context_window: Some(1_000_000),
            structured_output: true,
            tools: true,
            image_input: true,
            prompt_cache: true,
        },
        // 登记牌价（2026-10 检索官方 pricing 页，V4.1-Flash 谷时口径；
        // 峰时约 ×2，以官方页为准，B8 复核）：miss $0.15 / hit $0.003 /
        // out $0.60 每百万 tokens。
        price: Some(ModelPrice {
            currency: "USD",
            prompt_micro_per_m: 150_000,
            completion_micro_per_m: 600_000,
            cached_micro_per_m: 3_000,
        }),
    },
    ProviderPreset {
        id: "moonshot",
        label: "Moonshot Kimi",
        kind: ProviderKind::OpenAiCompat,
        model: "moonshot-v1-8k",
        base_url: "https://api.moonshot.cn/v1",
        local: false,
        caps: ModelCaps::UNREGISTERED, // 待登记
        price: None,
    },
    ProviderPreset {
        id: "groq",
        label: "Groq",
        kind: ProviderKind::OpenAiCompat,
        model: "llama-3.1-8b-instant",
        base_url: "https://api.groq.com/openai/v1",
        local: false,
        caps: ModelCaps::UNREGISTERED, // 待登记
        price: None,
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        kind: ProviderKind::OpenAiCompat,
        model: "openrouter/auto",
        base_url: "https://openrouter.ai/api/v1",
        local: false,
        caps: ModelCaps::UNREGISTERED, // 透传网关：能力随路由模型浮动，不预设
        price: None,
    },
    ProviderPreset {
        id: "anthropic",
        label: "Anthropic",
        kind: ProviderKind::Anthropic,
        model: "claude-3-5-haiku-latest",
        base_url: "https://api.anthropic.com/v1",
        local: false,
        caps: ModelCaps {
            context_window: Some(200_000),
            structured_output: false, // messages 协议无 response_format
            tools: true,
            image_input: true,
            prompt_cache: true, // cache_read_input_tokens 已解析
        },
        price: None, // 牌价待登记
    },
    // ── local open-source serving (no key, no egress) ──
    ProviderPreset {
        id: "ollama",
        label: "Ollama 本地",
        kind: ProviderKind::OpenAiCompat,
        model: "qwen3:8b",
        base_url: "http://127.0.0.1:11434/v1",
        local: true,
        caps: ModelCaps {
            structured_output: true,
            tools: true,
            ..ModelCaps::UNREGISTERED
        },
        price: None, // 本地推理：电费口径，不记账
    },
    ProviderPreset {
        id: "vllm",
        label: "vLLM 本地",
        kind: ProviderKind::OpenAiCompat,
        model: "Qwen/Qwen3-8B",
        base_url: "http://127.0.0.1:8000/v1",
        local: true,
        caps: ModelCaps {
            structured_output: true,
            tools: true,
            ..ModelCaps::UNREGISTERED
        },
        price: None,
    },
    ProviderPreset {
        id: "llamacpp",
        label: "llama.cpp 本地",
        kind: ProviderKind::OpenAiCompat,
        model: "glm-4-9b-chat",
        base_url: "http://127.0.0.1:8080/v1",
        local: true,
        caps: ModelCaps {
            structured_output: true,
            ..ModelCaps::UNREGISTERED
        },
        price: None,
    },
    ProviderPreset {
        id: "lmstudio",
        label: "LM Studio 本地",
        kind: ProviderKind::OpenAiCompat,
        model: "minicpm-v",
        base_url: "http://127.0.0.1:1234/v1",
        local: true,
        caps: ModelCaps {
            structured_output: true,
            ..ModelCaps::UNREGISTERED
        },
        price: None,
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
/// Protocol-independent reasoning effort, mirroring
/// `deepseek-recipe-core::conversation::ReasoningEffort` (the official
/// `reasoning_effort` wire field, snake_case). Our own copy keeps this
/// crate dependency-free below the `native-http` feature; the adapter
/// converts to the recipe type for the wire (A1, PLAN-deepseek-upgrade).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    Low,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningEffort::Low => "low",
            ReasoningEffort::High => "high",
            ReasoningEffort::Xhigh => "xhigh",
            ReasoningEffort::Max => "max",
        }
    }
}

/// Construction-time wire options for the native HTTP adapter
/// (v2.1, kernel-v2 parity): reproducibility seed, thinking-mode
/// control and JSON mode — the browser kernel passes these per call
/// (`opts.seed/opts.thinking/opts.json`); the Rust trait's
/// `complete(&self, messages)` has no per-call channel, so they live
/// at construction here. OpenAI-compatible dialect only; Anthropic
/// requests are untouched.
///
/// v0.7 additions (PLAN-deepseek-upgrade B6, pre-B1 transition): the
/// schema rides construction-time options until B1's `InferRequest`
/// gives the trait a per-call channel. `response_schema` carries the
/// raw JSON Schema text; the adapter validates it eagerly (fail-fast
/// before any network) and sends `response_format: json_schema`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderOptions {
    /// Provider-side sampling seed (reproducibility; best-effort —
    /// vendors may still drift, see the twin demo's honesty note).
    pub seed: Option<u64>,
    /// Send `thinking: {"type":"disabled"}` — reasoning models
    /// (DeepSeek-V4 default-high) otherwise burn max_tokens into
    /// hidden reasoning and return empty/short bodies. Proven on the
    /// wire; kept alongside `reasoning_effort` (the recipe-shaped
    /// effort dial) — they answer different questions: on/off vs how hard.
    pub thinking_disabled: bool,
    /// Send `response_format: {"type":"json_object"}`.
    pub json_mode: bool,
    /// Reasoning effort dial (`reasoning_effort` wire field, recipe-core
    /// semantics). Semantically exclusive with `thinking_disabled`; if
    /// both are set the explicit disable wins and a warning is traced.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Raw JSON Schema the reply must satisfy — sent as
    /// `response_format: {"type":"json_schema", ...}`. Supersedes
    /// `json_mode` when both are set (json_object ⊆ json_schema).
    /// Invalid JSON fails the call before any network is touched.
    pub response_schema: Option<String>,
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
            tracing::warn!(
                provider = kind.id(),
                model = %cfg.model,
                "L7 DEGRADED: remote provider requested but native-http is off; \
                 serving rule backend — surface degradation_note(), never pass silently"
            );
            std::sync::Arc::new(RuleProvider::degraded(kind.id()))
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
            tracing::warn!(
                provider = kind.id(),
                model = %cfg.model,
                "L7 DEGRADED: remote provider requested but native-http is off; \
                 serving rule backend — surface degradation_note(), never pass silently"
            );
            std::sync::Arc::new(RuleProvider::degraded(kind.id()))
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
        // The nine offline/cloud rows the browser kernel serves
        // (kernel.js PROVIDER_PRESETS) all resolve, plus the qwen and
        // stepfun reserved rows and the four local open-source rows.
        for id in [
            "rule",
            "zhipu",
            "openai",
            "deepseek",
            "moonshot",
            "groq",
            "openrouter",
            "stepfun",
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
        assert_eq!(PROVIDER_PRESETS.len(), 14);
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

    #[test]
    fn caps_and_prices_are_registered_where_claimed() {
        // deepseek is the measured row: V4.1-Flash market facts + dated price.
        let ds = PROVIDER_PRESETS
            .iter()
            .find(|p| p.id == "deepseek")
            .unwrap();
        assert_eq!(ds.caps.context_window, Some(1_000_000));
        assert!(ds.caps.structured_output && ds.caps.prompt_cache);
        let price = ds.price.expect("deepseek carries the dated price");
        assert_eq!(price.currency, "USD");
        assert!(
            price.cached_micro_per_m < price.prompt_micro_per_m,
            "a cache hit must price below a miss — that's the whole B5 point"
        );
        let oai = PROVIDER_PRESETS.iter().find(|p| p.id == "openai").unwrap();
        assert!(oai.caps.structured_output && oai.price.is_some());
        // zhipu is tools-only (schema support unverified — stay false).
        let zhipu = PROVIDER_PRESETS.iter().find(|p| p.id == "zhipu").unwrap();
        assert!(zhipu.caps.tools && !zhipu.caps.structured_output);
        // anthropic: no response_format on the wire, cache reads parsed.
        let ant = PROVIDER_PRESETS
            .iter()
            .find(|p| p.id == "anthropic")
            .unwrap();
        assert!(!ant.caps.structured_output && ant.caps.prompt_cache);
        assert_eq!(ant.caps.context_window, Some(200_000));
        assert!(ant.price.is_none());

        // Unregistered rows stay conservative — no invented vendor
        // facts. (zhipu IS registered, tools only — GLM function calling
        // is documented; schema support is unverified so structured_output
        // stays false.)
        for id in ["rule", "qwen", "stepfun", "moonshot", "groq", "openrouter"] {
            let row = PROVIDER_PRESETS.iter().find(|p| p.id == id).unwrap();
            assert_eq!(
                row.caps,
                ModelCaps::default(),
                "{id} must stay unregistered"
            );
            assert!(row.price.is_none(), "{id} must carry no price");
        }
        // Local serving is electricity, not billing — never priced.
        for row in PROVIDER_PRESETS.iter().filter(|p| p.local) {
            assert!(row.price.is_none(), "{} local must be unpriced", row.id);
        }
    }
}
