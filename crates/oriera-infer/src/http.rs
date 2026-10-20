//! Native HTTP `ModelProvider` adapter (#68, design discussions/78).
//!
//! One blocking client (ureq + rustls) fits `System::run`'s synchronous
//! shape — no async runtime enters L7. Two wire dialects cover the whole
//! preset table: OpenAI-compatible `/chat/completions` (zhipu, openai,
//! qwen, deepseek, moonshot, groq, openrouter + the four local serving
//! rows) and Anthropic `/messages` with its own header set.
//!
//! Failure policy (the part the design page pins down):
//!
//! - **classification** — [`InferError::Network`] (never reached the
//!   far side) / [`InferError::Timeout`] (alive but slow: a budget
//!   signal) / [`InferError::Status`] (provider answered, non-2xx,
//!   scrubbed snippet) / [`InferError::Parse`] (2xx garbage, never
//!   retried);
//! - **retry** — zero by default; the single exception is a non-timeout
//!   transport failure, retried once immediately (no sleep). The design
//!   page scoped that to "connection phase"; ureq does not expose the
//!   pre/post-send boundary, so the implementation widens it to
//!   non-timeout transports — a chat completion is semantically
//!   read-only for the world, the worst case is a double-billed token
//!   on a rare partial send;
//! - **timeouts** — 20s overall (kernel.js parity), 5s of it budgeted
//!   for the connect phase;
//! - **key hygiene** — the key lives in `ProviderConfig` and nowhere
//!   else: never in logs, never in error details (scrubbed, including
//!   the request URL ureq embeds in transport messages), never in
//!   metrics ids beyond `kind:model`.
//!
//! v0.7 (PLAN-deepseek-upgrade): B5 parses the providers' prompt-cache
//! hit counts into `TokenUsage::cached` (all three wire paths: blocking,
//! tools, SSE); B6 adds `response_format: json_schema` gated in the wire
//! builder (a malformed schema fails before any network, on every path);
//! A1 shapes `reasoning_effort` through `deepseek-recipe-core`'s
//! canonical enum.

// Native-only feature: the wasm channel is the kernel.js mirror, and
// ureq does not compile for wasm targets — fail fast with a message
// that says which channel to use instead of a wall of foreign errors.
#[cfg(target_family = "wasm")]
compile_error!(
    "oriera-infer `native-http` is native-only; wasm builds use the \
     kernel.js HttpChatProvider mirror (examples/11-ai-cafe/engine/kernel.js)"
);

/// Our dial maps 1:1 onto the recipe's canonical wire enum (A1) — the
/// copy exists only so the crate stays dependency-free below the
/// `native-http` feature.
impl From<ReasoningEffort> for RecipeEffort {
    fn from(e: ReasoningEffort) -> Self {
        match e {
            ReasoningEffort::Low => RecipeEffort::Low,
            ReasoningEffort::High => RecipeEffort::High,
            ReasoningEffort::Xhigh => RecipeEffort::Xhigh,
            ReasoningEffort::Max => RecipeEffort::Max,
        }
    }
}

/// B5: cache-hit prompt tokens, three vendor styles — DeepSeek
/// `prompt_cache_hit_tokens` first (unambiguous), then OpenAI's nested
/// details, then Anthropic's cache read. `v` is the usage object (or the
/// whole payload — pointer paths are root-relative either way for the
/// first hop `/usage/...`; callers pass the right root). Clamped into
/// the prompt: a lying vendor is truncated, never propagated.
fn usage_cached(v: &Value, prompt: u64) -> u64 {
    v.pointer("/prompt_cache_hit_tokens")
        .or_else(|| v.pointer("/prompt_tokens_details/cached_tokens"))
        .or_else(|| v.pointer("/cache_read_input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(prompt)
}

use crate::provider::{
    ChatMessage, InferError, ModelProvider, ProviderConfig, ProviderKind, ProviderOptions,
    ReasoningEffort, Role, ToolCall, ToolDef, ToolOutcome,
};
use deepseek_recipe_core::conversation::ReasoningEffort as RecipeEffort;
use oriera_common::TokenUsage;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read};
use std::sync::Mutex;
use std::time::Duration;

/// Wire protocol shape. Zhipu is the OpenAI dialect with its own base
/// URL — vendors are data, not code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `/chat/completions`, `Authorization: Bearer`.
    OpenAi,
    /// `/messages`, `x-api-key` + `anthropic-version`.
    Anthropic,
}

impl Dialect {
    fn of(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::Anthropic => Dialect::Anthropic,
            ProviderKind::Zhipu | ProviderKind::OpenAiCompat | ProviderKind::Rule => {
                Dialect::OpenAi
            }
        }
    }
}

/// Blocking HTTP backend for one `ProviderConfig`. Cheap to clone
/// behind `Arc` (the agent pools connections internally).
pub struct HttpProvider {
    agent: ureq::Agent,
    cfg: ProviderConfig,
    dialect: Dialect,
    url: String,
    label: String,
    usage: Mutex<TokenUsage>,
    options: ProviderOptions,
}

impl HttpProvider {
    /// 20s overall / 5s connect — kernel.js parity (design §5).
    pub fn new(cfg: ProviderConfig) -> Self {
        Self::with_timeouts(cfg, Duration::from_secs(20), Duration::from_secs(5))
    }

    /// Wire options (v2.1 kernel-v2 parity): seed / thinking / json.
    pub fn with_options(cfg: ProviderConfig, options: ProviderOptions) -> Self {
        Self::with_timeouts_and_options(
            cfg,
            options,
            Duration::from_secs(20),
            Duration::from_secs(5),
        )
    }

    /// Options + timeout override for constrained shells and tests.
    pub fn with_timeouts_and_options(
        cfg: ProviderConfig,
        options: ProviderOptions,
        overall: Duration,
        connect: Duration,
    ) -> Self {
        Self {
            options,
            ..Self::with_timeouts(cfg, overall, connect)
        }
    }

    /// Timeout override for constrained shells and tests.
    pub fn with_timeouts(cfg: ProviderConfig, overall: Duration, connect: Duration) -> Self {
        let dialect = Dialect::of(cfg.kind);
        let base = cfg
            .base_url
            .clone()
            .unwrap_or_else(|| cfg.kind.default_base_url().to_string());
        let base = base.trim_end_matches('/').to_string();
        let path = match dialect {
            Dialect::OpenAi => "/chat/completions",
            Dialect::Anthropic => "/messages",
        };
        let label = format!("{}:{}", cfg.kind.id(), cfg.model);
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(overall)
                .timeout_connect(connect)
                .build(),
            url: format!("{base}{path}"),
            cfg,
            dialect,
            label,
            usage: Mutex::new(TokenUsage::ZERO),
            options: ProviderOptions::default(),
        }
    }

    /// Local endpoints (127.0.0.1 / localhost) need no key; every
    /// remote endpoint is config-incomplete without one.
    fn is_local_endpoint(&self) -> bool {
        self.url.starts_with("http://127.0.0.1") || self.url.starts_with("http://localhost")
    }

    fn build_request(&self, messages: &[ChatMessage]) -> Result<String, InferError> {
        self.build_request_ex(messages, None, false)
    }

    /// Extended wire builder: optional tool vocabulary + SSE streaming.
    fn build_request_ex(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[crate::provider::ToolDef]>,
        stream: bool,
    ) -> Result<String, InferError> {
        let mut body: Value = match self.dialect {
            Dialect::OpenAi => json!({
                "model": self.cfg.model,
                "messages": messages
                    .iter()
                    .map(message_openai)
                    .collect::<Vec<_>>(),
                "temperature": 0.85,
                "max_tokens": 260,
            }),
            Dialect::Anthropic => {
                let system = messages
                    .iter()
                    .filter(|m| m.role == Role::System)
                    .map(|m| m.content.as_str())
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    );
                json!({
                    "model": self.cfg.model,
                    "system": system,
                    "max_tokens": 260,
                    "messages": messages
                        .iter()
                        .filter(|m| m.role != Role::System)
                        .map(message_anthropic)
                        .collect::<Vec<_>>(),
                })
            }
        };
        // B6: a malformed schema is a CONFIG error — it fails here,
        // before any network, before any key is spent (blocking,
        // tools and SSE paths all route through this builder, so one
        // gate covers every road).
        let schema: Option<Value> = match self.options.response_schema.as_deref() {
            None => None,
            Some(raw) => match serde_json::from_str(raw) {
                Ok(v) => Some(v),
                Err(e) => {
                    return Err(InferError::Provider(format!(
                        "response_schema is not valid JSON: {e}"
                    )))
                }
            },
        };
        match self.dialect {
            Dialect::OpenAi => {
                if let Some(seed) = self.options.seed {
                    body["seed"] = json!(seed);
                }
                if self.options.thinking_disabled {
                    // The explicit off-switch wins over the effort dial
                    // (semantic exclusivity, documented on ProviderOptions).
                    body["thinking"] = json!({ "type": "disabled" });
                } else if let Some(effort) = self.options.reasoning_effort {
                    // A1: the recipe's canonical effort dial on the wire.
                    body["reasoning_effort"] =
                        serde_json::to_value(RecipeEffort::from(effort)).unwrap_or(Value::Null);
                }
                // B6: a schema supersedes plain json mode (json_object ⊆
                // json_schema). Anthropic has no response_format — a
                // configured schema is validated above but recorded, not
                // sent (caps mark the row structured_output: false).
                match schema.as_ref() {
                    Some(s) => {
                        body["response_format"] = json!({
                            "type": "json_schema",
                            "json_schema": { "name": "reply", "strict": true, "schema": s },
                        });
                    }
                    None if self.options.json_mode => {
                        body["response_format"] = json!({ "type": "json_object" });
                    }
                    None => {}
                }
                if let Some(tools) = tools.filter(|t| !t.is_empty()) {
                    body["tools"] = Value::Array(
                        tools
                            .iter()
                            .map(|t| {
                                json!({
                                    "type": "function",
                                    "function": {
                                        "name": t.name,
                                        "description": t.description,
                                        "parameters": serde_json::from_str::<Value>(&t.parameters_json)
                                            .unwrap_or_else(|_| json!({ "type": "object" })),
                                    }
                                })
                            })
                            .collect(),
                    );
                    body["tool_choice"] = json!("auto");
                }
                if stream {
                    body["stream"] = json!(true);
                    body["stream_options"] = json!({ "include_usage": true });
                }
            }
            Dialect::Anthropic => {
                if let Some(tools) = tools.filter(|t| !t.is_empty()) {
                    body["tools"] = Value::Array(
                        tools
                            .iter()
                            .map(|t| {
                                json!({
                                    "name": t.name,
                                    "description": t.description,
                                    "input_schema": serde_json::from_str::<Value>(&t.parameters_json)
                                        .unwrap_or_else(|_| json!({ "type": "object" })),
                                })
                            })
                            .collect(),
                    );
                }
                if stream {
                    body["stream"] = json!(true);
                }
            }
        }
        Ok(body.to_string())
    }

    fn apply_headers(&self, req: ureq::Request) -> ureq::Request {
        let req = req.set("Content-Type", "application/json");
        match self.dialect {
            Dialect::OpenAi => req.set(
                "Authorization",
                &format!("Bearer {}", self.cfg.api_key), // never logged
            ),
            Dialect::Anthropic => req
                .set("x-api-key", &self.cfg.api_key)
                .set("anthropic-version", "2023-06-01"),
        }
    }

    fn read_body(&self, reader: impl Read) -> Result<String, InferError> {
        // Cap at 64KB: completion payloads are small, and error bodies
        // must never drag a vendor's debug page into memory.
        let mut buf = Vec::new();
        reader
            .take(64 * 1024)
            .read_to_end(&mut buf)
            .map_err(|e| InferError::Network(e.to_string()))?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Send one request; 2xx returns the response (caller owns the
    /// body reader), non-2xx and transport failures map to InferError.
    fn send(&self, body: &str) -> Result<ureq::Response, InferError> {
        let req = self.apply_headers(self.agent.post(&self.url));
        match req.send_string(body) {
            Ok(resp) => Ok(resp),
            Err(e) => Err(self.map_send_err(e)),
        }
    }

    /// ureq error → InferError (shared by body and SSE paths).
    fn map_send_err(&self, e: ureq::Error) -> InferError {
        match e {
            ureq::Error::Status(code, resp) => {
                let retry_after = resp.header("retry-after").map(str::to_string);
                let detail = match self.read_body(resp.into_reader()) {
                    Ok(d) => d,
                    Err(e) => return e,
                };
                let mut detail = detail;
                if let Some(ra) = retry_after {
                    detail.push_str(&format!(" [retry-after: {ra}]"));
                }
                InferError::Status {
                    code,
                    detail: self.scrub(detail),
                }
            }
            ureq::Error::Transport(t) => {
                let msg = t.to_string();
                let deadline = {
                    let lower = msg.to_lowercase();
                    lower.contains("timed out")
                        || lower.contains("timeout")
                        || msg.contains("(os error 10060)")
                        || msg.contains("(os error 110)")
                };
                if t.kind() == ureq::ErrorKind::Io && deadline {
                    InferError::Timeout
                } else {
                    InferError::Network(self.scrub(msg))
                }
            }
        }
    }

    fn attempt(&self, body: &str) -> Result<String, InferError> {
        match self.send(body) {
            Ok(resp) => self.read_body(resp.into_reader()),
            Err(e) => Err(e),
        }
    }

    /// Strip secrets from anything that is about to become an error
    /// detail. Two surfaces: the api key itself (and Bearer forms), and
    /// the request URL — ureq transport messages embed it verbatim, and
    /// a gateway base_url may carry a query-string credential that must
    /// not round-trip into logs. Keys and URLs never leave the provider
    /// in raw form.
    fn scrub(&self, s: String) -> String {
        let mut out = s;
        if let Some(cut) = self.url.find('?') {
            let sanitized = &self.url[..cut];
            out = out.replace(self.url.as_str(), sanitized);
        }
        let key = self.cfg.api_key.trim();
        if !key.is_empty() {
            out = out
                .replace(key, "***")
                .replace(&format!("Bearer {key}"), "Bearer ***");
        }
        out
    }

    fn parse_reply(&self, raw: String) -> Result<(String, TokenUsage), InferError> {
        let v: Value =
            serde_json::from_str(&raw).map_err(|e| InferError::Parse(format!("not json: {e}")))?;
        let text = match self.dialect {
            Dialect::OpenAi => v
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .ok_or_else(|| InferError::Parse("no choices[0].message.content".into()))?,
            Dialect::Anthropic => v
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .ok_or_else(|| InferError::Parse("no content[0].text".into()))?,
        }
        .to_string();
        let prompt = v
            .pointer("/usage/prompt_tokens")
            .or_else(|| v.pointer("/usage/input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let usage = TokenUsage {
            prompt,
            completion: v
                .pointer("/usage/completion_tokens")
                .or_else(|| v.pointer("/usage/output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cached: v.pointer("/usage").map_or(0, |u| usage_cached(u, prompt)),
        };
        Ok((text, usage))
    }
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// 引擎中立的工具调用历史 → 单条 OpenAI wire tool_call。
fn tool_call_openai(v: &Value) -> Value {
    json!({
        "id": v["id"],
        "type": "function",
        "function": {
            "name": v["name"],
            "arguments": v["arguments_json"].as_str().unwrap_or("{}"),
        }
    })
}

/// 引擎中立的工具调用历史 → 单条 Anthropic tool_use 块。
fn tool_call_anthropic(v: &Value) -> Value {
    json!({
        "type": "tool_use",
        "id": v["id"],
        "name": v["name"],
        "input": serde_json::from_str::<Value>(v["arguments_json"].as_str().unwrap_or("{}"))
            .unwrap_or_else(|_| json!({})),
    })
}

/// 按 OpenAI 方言渲染一条消息(工具轮的粘合历史)。
fn message_openai(m: &ChatMessage) -> Value {
    match m.role {
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
            "content": m.content,
        }),
        Role::Assistant if m.tool_calls_json.is_some() => {
            let calls: Vec<Value> =
                serde_json::from_str::<Value>(m.tool_calls_json.as_deref().unwrap_or("[]"))
                    .ok()
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .map(tool_call_openai)
                    .collect();
            json!({
                "role": "assistant",
                "content": m.content,
                "tool_calls": calls,
            })
        }
        _ => json!({ "role": role_str(m.role), "content": m.content }),
    }
}

/// 按 Anthropic 方言渲染一条消息(工具结果= user 帧内的 tool_result 块)。
fn message_anthropic(m: &ChatMessage) -> Value {
    match m.role {
        Role::Tool => json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.content,
            }],
        }),
        Role::Assistant if m.tool_calls_json.is_some() => {
            let mut blocks = vec![json!({ "type": "text", "text": m.content })];
            if let Some(arr) =
                serde_json::from_str::<Value>(m.tool_calls_json.as_deref().unwrap_or("[]"))
                    .ok()
                    .and_then(|v| v.as_array().cloned())
            {
                for c in &arr {
                    blocks.push(tool_call_anthropic(c));
                }
            }
            json!({ "role": "assistant", "content": blocks })
        }
        _ => json!({ "role": role_str(m.role), "content": m.content }),
    }
}

impl ModelProvider for HttpProvider {
    fn id(&self) -> &str {
        &self.label
    }

    fn complete(&self, messages: &[ChatMessage]) -> Result<String, InferError> {
        if self.cfg.api_key.trim().is_empty() && !self.is_local_endpoint() {
            return Err(InferError::Provider(
                "no api key configured — the key only ever enters via the shell (env/UI), never the engine".into(),
            ));
        }
        let body = self.build_request(messages)?;
        // Zero retries by design; one immediate exception for a
        // non-timeout transport blip (design §5, widened — see module
        // docs). Status/Parse/Timeout never retry. If the retry itself
        // hits the deadline, that is the more severe signal — report
        // Timeout, not the first attempt's Network.
        let raw = match self.attempt(&body) {
            Ok(raw) => raw,
            Err(InferError::Network(msg)) => match self.attempt(&body) {
                Ok(raw) => raw,
                Err(InferError::Timeout) => return Err(InferError::Timeout),
                Err(_) => return Err(InferError::Network(msg)),
            },
            Err(e) => return Err(e),
        };
        let (text, usage) = self.parse_reply(raw)?;
        if let Ok(mut u) = self.usage.lock() {
            *u = usage;
        }
        Ok(text)
    }

    fn supports_tools(&self) -> bool {
        true
    }

    /// Tool-enabled completion over the vendor wire (RFC-3): OpenAI
    /// `tools`/`tool_choice=auto`, Anthropic `tools`/`input_schema`.
    fn complete_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<ToolOutcome, InferError> {
        if self.cfg.api_key.trim().is_empty() && !self.is_local_endpoint() {
            return Err(InferError::Provider(
                "no api key configured — the key only ever enters via the shell (env/UI), never the engine".into(),
            ));
        }
        let body = self.build_request_ex(messages, Some(tools), false)?;
        let raw = self.post_json(&body)?;
        self.parse_tool_reply(raw)
    }

    /// Native SSE streaming (RFC-3): one chunk per text delta, in
    /// arrival order; usage is taken from the terminal frames.
    fn complete_stream(&self, messages: &[ChatMessage]) -> Result<Vec<String>, InferError> {
        if self.cfg.api_key.trim().is_empty() && !self.is_local_endpoint() {
            return Err(InferError::Provider(
                "no api key configured — the key only ever enters via the shell (env/UI), never the engine".into(),
            ));
        }
        let body = self.build_request_ex(messages, None, true)?;
        let resp = self.send(&body)?;
        let mut reader = BufReader::new(resp.into_reader());
        let mut chunks: Vec<String> = Vec::new();
        let mut usage = TokenUsage::ZERO;
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .map_err(|e| InferError::Network(e.to_string()))?;
            if n == 0 {
                break; // EOF before [DONE]/message_stop — take what landed
            }
            let trimmed = line.trim_end();
            let Some(payload) = trimmed.strip_prefix("data:") else {
                continue; // "event:" lines and blank separators carry no data
            };
            let payload = payload.trim();
            if payload == "[DONE]" {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            match self.dialect {
                Dialect::OpenAi => {
                    if let Some(t) = v
                        .pointer("/choices/0/delta/content")
                        .and_then(Value::as_str)
                    {
                        if !t.is_empty() {
                            chunks.push(t.to_string());
                        }
                    }
                    if let Some(u) = v.pointer("/usage").filter(|u| u.is_object()) {
                        let prompt = u
                            .pointer("/prompt_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        usage = TokenUsage {
                            prompt,
                            completion: u
                                .pointer("/completion_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or(0),
                            cached: usage_cached(u, prompt),
                        };
                    }
                }
                Dialect::Anthropic => match v.pointer("/type").and_then(Value::as_str) {
                    Some("content_block_delta") => {
                        if let Some(t) = v.pointer("/delta/text").and_then(Value::as_str) {
                            chunks.push(t.to_string());
                        }
                    }
                    Some("message_start") => {
                        usage.prompt = v
                            .pointer("/message/usage/input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                    }
                    Some("message_delta") => {
                        usage.completion = v
                            .pointer("/usage/output_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                    }
                    Some("message_stop") => break,
                    _ => {}
                },
            }
        }
        if chunks.is_empty() {
            return Err(InferError::Parse(
                "stream carried no text deltas (200 but empty SSE body)".into(),
            ));
        }
        if let Ok(mut u) = self.usage.lock() {
            *u = usage;
        }
        Ok(chunks)
    }

    fn last_usage(&self) -> TokenUsage {
        self.usage.lock().map(|u| *u).unwrap_or(TokenUsage::ZERO)
    }
}

impl HttpProvider {
    /// POST with complete()'s minimal retry policy.
    fn post_json(&self, body: &str) -> Result<String, InferError> {
        match self.attempt(body) {
            Ok(raw) => Ok(raw),
            Err(InferError::Network(msg)) => match self.attempt(body) {
                Ok(raw) => Ok(raw),
                Err(InferError::Timeout) => Err(InferError::Timeout),
                Err(_) => Err(InferError::Network(msg)),
            },
            Err(e) => Err(e),
        }
    }

    /// Split a tool-enabled reply into prose or tool calls, both
    /// dialects; usage is metered on either road.
    fn parse_tool_reply(&self, raw: String) -> Result<ToolOutcome, InferError> {
        let v: Value =
            serde_json::from_str(&raw).map_err(|e| InferError::Parse(format!("not json: {e}")))?;
        let prompt = v
            .pointer("/usage/prompt_tokens")
            .or_else(|| v.pointer("/usage/input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let usage = TokenUsage {
            prompt,
            completion: v
                .pointer("/usage/completion_tokens")
                .or_else(|| v.pointer("/usage/output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cached: v.pointer("/usage").map_or(0, |u| usage_cached(u, prompt)),
        };
        let calls: Vec<ToolCall> = match self.dialect {
            Dialect::OpenAi => v
                .pointer("/choices/0/message/tool_calls")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| {
                            Some(ToolCall {
                                id: c.pointer("/id").and_then(Value::as_str)?.to_string(),
                                name: c
                                    .pointer("/function/name")
                                    .and_then(Value::as_str)?
                                    .to_string(),
                                arguments_json: c
                                    .pointer("/function/arguments")
                                    .and_then(Value::as_str)
                                    .unwrap_or("{}")
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Dialect::Anthropic => v
                .pointer("/content")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter(|c| c.pointer("/type").and_then(Value::as_str) == Some("tool_use"))
                        .filter_map(|c| {
                            Some(ToolCall {
                                id: c.pointer("/id").and_then(Value::as_str)?.to_string(),
                                name: c.pointer("/name").and_then(Value::as_str)?.to_string(),
                                arguments_json: c
                                    .pointer("/input")
                                    .map(|input| input.to_string())
                                    .unwrap_or_else(|| "{}".into()),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        if let Ok(mut u) = self.usage.lock() {
            *u = usage;
        }
        if calls.is_empty() {
            let (text, usage) = self.parse_reply(raw)?;
            if let Ok(mut u) = self.usage.lock() {
                *u = usage;
            }
            return Ok(ToolOutcome::Text(text));
        }
        Ok(ToolOutcome::ToolCalls(calls))
    }
}

// ---------------------------------------------------------------------------
// Tests: a hand-rolled mock HTTP server on std::net — no new dev-deps.
// Each test scripts the exact bytes the "vendor" answers with.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::Arc;

    /// What the mock does with one connection.
    enum Step {
        /// Answer with this status + body.
        Reply(u16, String),
        /// Read the request, then slam the connection shut (transport blip).
        CloseAbrupt,
        /// Wait before answering (drives the deadline-expiry path).
        SleepThenReply(u64, u16, String),
    }

    struct Mock {
        addr: SocketAddr,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl Mock {
        /// Serve `steps.len()` connections, one per step, on a
        /// background thread. Returns the base URL to point providers at.
        fn spawn(steps: Vec<Step>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let rx = Arc::clone(&requests);
            std::thread::spawn(move || {
                for step in steps {
                    let (mut stream, _) = listener.accept().unwrap();
                    let req = read_request(&mut stream);
                    rx.lock().unwrap().push(req);
                    match step {
                        Step::Reply(code, body) => {
                            write_reply(&mut stream, code, &body);
                        }
                        Step::CloseAbrupt => {
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                        }
                        Step::SleepThenReply(ms, code, body) => {
                            std::thread::sleep(Duration::from_millis(ms));
                            write_reply(&mut stream, code, &body);
                        }
                    }
                }
            });
            Self { addr, requests }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    /// Read headers + Content-Length body from one connection.
    fn read_request(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        // headers until CRLFCRLF
        loop {
            if stream.read_exact(&mut byte).is_err() {
                break;
            }
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf).into_owned();
        let len: usize = head
            .lines()
            .find(|l| l.to_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; len];
        if len > 0 {
            let _ = stream.read_exact(&mut body);
        }
        format!("{head}{}", String::from_utf8_lossy(&body))
    }

    fn write_reply(stream: &mut TcpStream, code: u16, body: &str) {
        let resp = format!(
            "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    }

    fn provider(cfg: ProviderConfig, overall: Duration) -> HttpProvider {
        HttpProvider::with_timeouts(cfg, overall, Duration::from_millis(500))
    }

    impl HttpProvider {
        fn set_timeouts_for_test(&self, _overall: Duration) {
            // timeouts are baked into the agent at construction; this shim
            // exists only to keep the options test explicit about intent.
        }
    }

    #[test]
    fn openai_compat_round_trip_with_usage() {
        let mock = Mock::spawn(vec![Step::Reply(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"在线。"}}],"usage":{"prompt_tokens":12,"completion_tokens":3}}"#.into(),
        )]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key-openai".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let out = p
            .complete(&[
                ChatMessage::system("你是自检员。"),
                ChatMessage::user("证明你在线。"),
            ])
            .unwrap();
        assert_eq!(out, "在线。");
        assert_eq!(
            p.last_usage(),
            TokenUsage {
                prompt: 12,
                completion: 3,
                cached: 0
            }
        );

        let req = mock.requests()[0].clone();
        assert!(req.contains("POST /chat/completions"));
        assert!(req.contains("Authorization: Bearer test-key-openai"));
        assert!(req.contains("Content-Type: application/json"));
        assert!(req.contains(r#""model":"m""#));
        assert!(req.contains(r#""role":"system""#));
    }

    #[test]
    fn zhipu_row_is_the_openai_dialect_at_its_own_base() {
        let mock = Mock::spawn(vec![Step::Reply(
            200,
            r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#.into(),
        )]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::Zhipu,
                model: "glm-4-flash".into(),
                api_key: "test-key-zhipu".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        assert!(p.id().starts_with("zhipu:glm-4-flash"));
        assert_eq!(p.complete(&[ChatMessage::user("hi")]).unwrap(), "ok");
        assert!(mock.requests()[0].contains("POST /chat/completions"));
    }

    #[test]
    fn anthropic_dialect_headers_and_system_extraction() {
        let mock = Mock::spawn(vec![Step::Reply(
            200,
            r#"{"content":[{"type":"text","text":"在场。"}],"usage":{"input_tokens":7,"output_tokens":2}}"#.into(),
        )]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::Anthropic,
                model: "claude".into(),
                api_key: "test-key-anthropic".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let out = p
            .complete(&[ChatMessage::system("系统提示词"), ChatMessage::user("报数")])
            .unwrap();
        assert_eq!(out, "在场。");
        assert_eq!(
            p.last_usage(),
            TokenUsage {
                prompt: 7,
                completion: 2,
                cached: 0
            }
        );

        let req = mock.requests()[0].clone();
        assert!(req.contains("POST /messages"));
        assert!(req.contains("x-api-key: test-key-anthropic"));
        assert!(req.contains("anthropic-version: 2023-06-01"));
        // system prompt hoisted out of the message array
        assert!(req.contains(r#""system":"系统提示词""#));
        assert!(!req.contains(r#""messages":[{"role":"system""#));
    }

    #[test]
    fn status_error_carries_a_scrubbed_snippet() {
        let key = "test-key-secret";
        let mock = Mock::spawn(vec![Step::Reply(
            401,
            format!(r#"{{"error":{{"message":"bad key {key}"}}}}"#),
        )]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: key.into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        match p.complete(&[ChatMessage::user("x")]) {
            Err(InferError::Status { code: 401, detail }) => {
                assert!(!detail.contains(key), "key must be scrubbed: {detail}");
                assert!(detail.contains("***"));
            }
            other => panic!("expected Status(401), got {other:?}"),
        }
    }

    #[test]
    fn transport_blip_retries_once_then_succeeds() {
        // First connection dies mid-conversation, second answers.
        let mock = Mock::spawn(vec![
            Step::CloseAbrupt,
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"second try"}}]}"#.into(),
            ),
        ]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        assert_eq!(p.complete(&[ChatMessage::user("x")]).unwrap(), "second try");
        assert_eq!(mock.requests().len(), 2, "exactly one retry");
    }

    #[test]
    fn refused_connection_is_network_not_timeout() {
        // A port that WAS open and is now closed: the kernel answers
        // RST immediately on loopback — a genuine connection-refused,
        // unlike well-known ports that desktop firewalls like to filter
        // into silence (which would read as a timeout).
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(format!("http://127.0.0.1:{port}")),
            },
            Duration::from_secs(2),
        );
        match p.complete(&[ChatMessage::user("x")]) {
            Err(InferError::Network(_)) => {}
            other => panic!("expected Network, got {other:?}"),
        }
    }

    #[test]
    fn slow_far_side_is_classified_as_timeout() {
        let mock = Mock::spawn(vec![Step::SleepThenReply(
            800,
            200,
            r#"{"choices":[{"message":{"content":"late"}}"#.into(),
        )]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_millis(150),
        );
        match p.complete(&[ChatMessage::user("x")]) {
            Err(InferError::Timeout) => {}
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn garbage_payload_is_parse_and_never_retried() {
        let mock = Mock::spawn(vec![Step::Reply(200, "not json at all".into())]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        match p.complete(&[ChatMessage::user("x")]) {
            Err(InferError::Parse(m)) => assert!(m.contains("not json")),
            other => panic!("expected Parse, got {other:?}"),
        }
        assert_eq!(mock.requests().len(), 1, "parse errors do not retry");
    }

    #[test]
    fn local_endpoint_runs_keyless_cloud_refuses_early() {
        let mock = Mock::spawn(vec![Step::Reply(
            200,
            r#"{"choices":[{"message":{"content":"local ok"}}]}"#.into(),
        )]);
        let local = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "qwen3:8b".into(),
                api_key: String::new(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        assert_eq!(
            local.complete(&[ChatMessage::user("x")]).unwrap(),
            "local ok"
        );

        let cloud = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: String::new(),
                base_url: Some("https://api.example.com/v1".into()),
            },
            Duration::from_secs(5),
        );
        match cloud.complete(&[ChatMessage::user("x")]) {
            Err(InferError::Provider(m)) => assert!(m.contains("no api key")),
            other => panic!("expected Provider(no key), got {other:?}"),
        }
    }

    #[test]
    fn wire_options_reach_the_request_body_and_default_is_clean() {
        let mock = Mock::spawn(vec![
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"a"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.into(),
            ),
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"b"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.into(),
            ),
        ]);
        let cfg = ProviderConfig {
            kind: ProviderKind::OpenAiCompat,
            model: "m".into(),
            api_key: "test-key".into(),
            base_url: Some(mock.base_url()),
        };
        // with options: seed/thinking/json all present
        let with = HttpProvider::with_options(
            cfg.clone(),
            ProviderOptions {
                seed: Some(7),
                thinking_disabled: true,
                json_mode: true,
                ..ProviderOptions::default()
            },
        );
        with.set_timeouts_for_test(Duration::from_secs(5));
        let _ = with.complete(&[ChatMessage::user("x")]).unwrap();

        // default: none of them leak into the wire
        let without =
            HttpProvider::with_timeouts(cfg, Duration::from_secs(5), Duration::from_millis(500));
        let _ = without.complete(&[ChatMessage::user("x")]).unwrap();

        let reqs = mock.requests();
        assert!(reqs[0].contains(r#""seed":7"#), "seed missing: {}", reqs[0]);
        assert!(
            reqs[0].contains(r#""thinking":{"type":"disabled"}"#),
            "thinking missing"
        );
        assert!(
            reqs[0].contains(r#""response_format":{"type":"json_object"}"#),
            "json missing"
        );
        assert!(
            !reqs[1].contains("\"seed\""),
            "default must not send seed: {}",
            reqs[1]
        );
        assert!(
            !reqs[1].contains("thinking"),
            "default must not send thinking"
        );
        assert!(
            !reqs[1].contains("response_format"),
            "default must not send response_format"
        );
    }

    #[test]
    fn url_query_secret_is_scrubbed_from_network_details() {
        // A gateway base_url carrying a query credential: ureq embeds
        // the full URL in transport messages — the query must not
        // survive into the error detail.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(format!("http://127.0.0.1:{port}/v1?token=gw-secret-42")),
            },
            Duration::from_secs(2),
        );
        match p.complete(&[ChatMessage::user("x")]) {
            Err(InferError::Network(detail)) => {
                assert!(!detail.contains("gw-secret-42"), "query leaked: {detail}");
                assert!(!detail.contains("test-key"), "key leaked: {detail}");
            }
            other => panic!("expected Network, got {other:?}"),
        }
    }

    #[test]
    fn retry_that_hits_the_deadline_reports_timeout() {
        // First attempt: transport blip (connection slammed shut).
        // Retry: connects, but the far side stalls past the deadline —
        // the reported error must be the more severe Timeout, not the
        // first attempt's Network.
        let mock = Mock::spawn(vec![
            Step::CloseAbrupt,
            Step::SleepThenReply(
                600,
                200,
                r#"{"choices":[{"message":{"content":"late"}}"#.into(),
            ),
        ]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_millis(150),
        );
        match p.complete(&[ChatMessage::user("x")]) {
            Err(InferError::Timeout) => {}
            other => panic!("expected Timeout from the retry, got {other:?}"),
        }
        assert_eq!(mock.requests().len(), 2);
    }

    // ---- RFC-3 工具调用 + SSE 流式 ----

    fn tool_defs() -> Vec<crate::provider::ToolDef> {
        vec![crate::provider::ToolDef {
            name: "spawn".into(),
            description: "在世界中生成 N 个实体".into(),
            parameters_json: r#"{"type":"object","properties":{"count":{"type":"integer","minimum":1,"maximum":50}},"required":["count"]}"#.into(),
        }]
    }

    #[test]
    fn openai_tool_calls_round_trip() {
        let mock = Mock::spawn(vec![Step::Reply(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"spawn","arguments":"{\"count\":3}"}}]}}],"usage":{"prompt_tokens":30,"completion_tokens":8}}"#.into(),
        )]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        assert!(p.supports_tools());
        let out = p
            .complete_with_tools(&[ChatMessage::user("帮我生成 3 个实体")], &tool_defs())
            .unwrap();
        match out {
            crate::provider::ToolOutcome::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].name, "spawn");
                assert!(calls[0].arguments_json.contains("\"count\""));
            }
            other => panic!("expected tool calls, got {other:?}"),
        }
        assert_eq!(p.last_usage().prompt, 30);
        // wire 侧:tools 词表 + tool_choice 都上链了
        let req = &mock.requests()[0];
        assert!(req.contains("\"tools\""), "tools vocabulary on the wire");
        assert!(req.contains("\"tool_choice\""));
        assert!(req.contains("\"spawn\""));
    }

    #[test]
    fn anthropic_tool_use_round_trip() {
        let mock = Mock::spawn(vec![Step::Reply(
            200,
            r#"{"content":[{"type":"tool_use","id":"tu_1","name":"spawn","input":{"count":5}}],"usage":{"input_tokens":18,"output_tokens":6}}"#.into(),
        )]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::Anthropic,
                model: "claude".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let out = p
            .complete_with_tools(&[ChatMessage::user("来 5 个")], &tool_defs())
            .unwrap();
        match out {
            crate::provider::ToolOutcome::ToolCalls(calls) => {
                assert_eq!(calls[0].name, "spawn");
                assert!(calls[0].arguments_json.contains("5"));
            }
            other => panic!("expected tool calls, got {other:?}"),
        }
        let req = &mock.requests()[0];
        assert!(req.contains("\"input_schema\""), "anthropic tool wire");
    }

    #[test]
    fn openai_sse_stream_chunks_and_usage() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"雨\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"停\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"了。\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3}}\n\n\
data: [DONE]\n\n";
        let mock = Mock::spawn(vec![Step::Reply(200, sse.into())]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let chunks = p.complete_stream(&[ChatMessage::user("写三个字")]).unwrap();
        assert_eq!(chunks, vec!["雨", "停", "了。"]);
        assert_eq!(chunks.concat(), "雨停了。");
        assert_eq!(p.last_usage().prompt, 9);
        assert_eq!(p.last_usage().completion, 3);
        // 流式请求带了 stream 标记与 usage 开关
        let req = &mock.requests()[0];
        assert!(req.contains("\"stream\":true"));
        assert!(req.contains("include_usage"));
    }

    #[test]
    fn anthropic_sse_stream_events() {
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":11}}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"灯\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"亮着\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let mock = Mock::spawn(vec![Step::Reply(200, sse.into())]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::Anthropic,
                model: "claude".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let chunks = p.complete_stream(&[ChatMessage::user("两个字")]).unwrap();
        assert_eq!(chunks, vec!["灯", "亮着"]);
        assert_eq!(p.last_usage().prompt, 11);
        assert_eq!(p.last_usage().completion, 4);
    }

    #[test]
    fn openai_multi_round_tool_history_on_the_wire() {
        // 第二轮请求必须携带:assistant 的 tool_calls 原文 + role:"tool"
        // 结果帧(tool_call_id 对上)——OpenAI 的硬协议要求。
        let mock = Mock::spawn(vec![
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_9","type":"function","function":{"name":"spawn","arguments":"{\"count\":2}"}}]}}],"usage":{"prompt_tokens":10,"completion_tokens":4}}"#.into(),
            ),
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":"布置完成。"}}],"usage":{"prompt_tokens":40,"completion_tokens":5}}"#.into(),
            ),
        ]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let tools = tool_defs();
        let first = p
            .complete_with_tools(&[ChatMessage::user("布置 2 个")], &tools)
            .unwrap();
        let crate::provider::ToolOutcome::ToolCalls(calls) = first else {
            panic!("round 1 should request the tool");
        };

        // 引擎侧执行后组第二轮消息(与 run_tool_agent 同构)
        let neutral: Vec<Value> = calls
            .iter()
            .map(|c| json!({ "id": c.id, "name": c.name, "arguments_json": c.arguments_json }))
            .collect();
        let round2 = vec![
            ChatMessage::user("布置 2 个"),
            ChatMessage::assistant_with_tools(serde_json::to_string(&neutral).unwrap()),
            ChatMessage::tool(&calls[0].id, "已提交:下一帧生成 2 个实体"),
        ];
        let second = p.complete_with_tools(&round2, &tools).unwrap();
        assert_eq!(
            second,
            crate::provider::ToolOutcome::Text("布置完成。".into())
        );

        let req2 = mock.requests()[1].clone();
        assert!(req2.contains("\"role\":\"tool\""), "tool result frame");
        assert!(req2.contains("\"tool_call_id\":\"call_9\""));
        assert!(
            req2.contains("\"tool_calls\""),
            "assistant history carries calls"
        );
    }

    #[test]
    fn anthropic_multi_round_tool_history_on_the_wire() {
        let mock = Mock::spawn(vec![
            Step::Reply(
                200,
                r#"{"content":[{"type":"tool_use","id":"tu_9","name":"spawn","input":{"count":3}}],"usage":{"input_tokens":9,"output_tokens":3}}"#.into(),
            ),
            Step::Reply(
                200,
                r#"{"content":[{"type":"text","text":"完成。"}],"usage":{"input_tokens":30,"output_tokens":2}}"#.into(),
            ),
        ]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::Anthropic,
                model: "claude".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let tools = tool_defs();
        let first = p
            .complete_with_tools(&[ChatMessage::user("来 3 个")], &tools)
            .unwrap();
        let crate::provider::ToolOutcome::ToolCalls(calls) = first else {
            panic!("round 1 should request the tool");
        };
        let neutral = json!([{ "id": calls[0].id, "name": calls[0].name, "arguments_json": calls[0].arguments_json }]);
        let round2 = vec![
            ChatMessage::user("来 3 个"),
            ChatMessage::assistant_with_tools(serde_json::to_string(&neutral).unwrap()),
            ChatMessage::tool(&calls[0].id, "已提交"),
        ];
        let second = p.complete_with_tools(&round2, &tools).unwrap();
        assert_eq!(second, crate::provider::ToolOutcome::Text("完成。".into()));
        let req2 = mock.requests()[1].clone();
        assert!(req2.contains("tool_result"), "anthropic tool result block");
        assert!(req2.contains("tool_use_id"));
        assert!(
            req2.contains("tool_use"),
            "assistant history as tool_use block"
        );
    }

    #[test]
    fn sse_without_any_text_delta_is_a_parse_error() {
        let mock = Mock::spawn(vec![Step::Reply(200, "data: [DONE]\n\n".into())]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        match p.complete_stream(&[ChatMessage::user("x")]) {
            Err(InferError::Parse(msg)) => assert!(msg.contains("no text deltas")),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    // ---- v0.7: B5 cache accounting / B6 schema / A1 effort (PLAN-deepseek-upgrade) ----

    #[test]
    fn deepseek_style_cache_hits_land_in_token_usage_cached() {
        let mock = Mock::spawn(vec![Step::Reply(
            200,
            r#"{"choices":[{"message":{"content":"缓存命中"}}],"usage":{"prompt_tokens":100,"completion_tokens":8,"prompt_cache_hit_tokens":92}}"#.into(),
        )]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "deepseek-chat".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let _ = p.complete(&[ChatMessage::user("x")]).unwrap();
        assert_eq!(
            p.last_usage(),
            TokenUsage {
                prompt: 100,
                completion: 8,
                cached: 92
            }
        );
    }

    #[test]
    fn openai_style_nested_cached_tokens_parse_too() {
        let mock = Mock::spawn(vec![Step::Reply(
            200,
            r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":50,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":30}}}"#.into(),
        )]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "gpt-4o-mini".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let _ = p.complete(&[ChatMessage::user("x")]).unwrap();
        assert_eq!(p.last_usage().cached, 30);
        assert_eq!(p.last_usage().billed_prompt(), 20);
    }

    #[test]
    fn anthropic_cache_read_parses_and_a_lying_vendor_is_clamped() {
        let mock = Mock::spawn(vec![
            Step::Reply(
                200,
                r#"{"content":[{"type":"text","text":"在场"}],"usage":{"input_tokens":40,"output_tokens":5,"cache_read_input_tokens":25}}"#.into(),
            ),
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"谎报"}}],"usage":{"prompt_tokens":10,"completion_tokens":1,"prompt_cache_hit_tokens":999}}"#.into(),
            ),
        ]);
        let pa = provider(
            ProviderConfig {
                kind: ProviderKind::Anthropic,
                model: "claude".into(),
                api_key: "test-key-a".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let _ = pa.complete(&[ChatMessage::user("x")]).unwrap();
        assert_eq!(pa.last_usage().cached, 25);

        let pl = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        let _ = pl.complete(&[ChatMessage::user("x")]).unwrap();
        assert_eq!(pl.last_usage().cached, 10, "cached > prompt is clamped");
    }

    #[test]
    fn schema_reaches_the_wire_as_json_schema_and_supersedes_json_mode() {
        let mock = Mock::spawn(vec![
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"{}"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.into(),
            ),
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"{}"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.into(),
            ),
        ]);
        let cfg = ProviderConfig {
            kind: ProviderKind::OpenAiCompat,
            model: "deepseek-chat".into(),
            api_key: "test-key".into(),
            base_url: Some(mock.base_url()),
        };
        let schema =
            r#"{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]}"#;
        let p = HttpProvider::with_options(
            cfg.clone(),
            ProviderOptions {
                json_mode: true,
                response_schema: Some(schema.to_string()),
                ..ProviderOptions::default()
            },
        );
        let _ = p.complete(&[ChatMessage::user("x")]).unwrap();
        let sent = mock.requests()[0].clone();
        assert!(
            sent.contains(r#""response_format":{"type":"json_schema","json_schema":{"name":"reply","strict":true,"schema":"#),
            "schema must ride response_format.json_schema: {sent}"
        );
        assert!(sent.contains(r#""type":"object""#), "schema body missing");
        assert!(
            !sent.contains(r#""json_object""#),
            "json_object must be superseded by the schema"
        );

        // Plain json_mode (no schema) still sends the old object form.
        let q = HttpProvider::with_options(
            cfg,
            ProviderOptions {
                json_mode: true,
                ..ProviderOptions::default()
            },
        );
        let _ = q.complete(&[ChatMessage::user("x")]).unwrap();
        assert!(mock.requests()[1].contains(r#""json_object""#));
    }

    #[test]
    fn invalid_schema_fails_before_any_network_or_key_spend() {
        // Zero steps: the mock serves no connections — any request would
        // hang the server thread, so passing means we never dialed.
        let mock = Mock::spawn(vec![]);
        let p = HttpProvider::with_options(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            ProviderOptions {
                response_schema: Some("{not json".to_string()),
                ..ProviderOptions::default()
            },
        );
        match p.complete(&[ChatMessage::user("x")]) {
            Err(InferError::Provider(m)) => assert!(m.contains("response_schema")),
            other => panic!("expected config-level Provider error, got {other:?}"),
        }
        assert_eq!(mock.requests().len(), 0, "no network may be touched");
    }

    #[test]
    fn reasoning_effort_rides_the_recipe_field_and_disable_wins() {
        let mock = Mock::spawn(vec![
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"a"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.into(),
            ),
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"b"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.into(),
            ),
        ]);
        let cfg = ProviderConfig {
            kind: ProviderKind::OpenAiCompat,
            model: "deepseek-chat".into(),
            api_key: "test-key".into(),
            base_url: Some(mock.base_url()),
        };
        let p = HttpProvider::with_options(
            cfg.clone(),
            ProviderOptions {
                reasoning_effort: Some(ReasoningEffort::Low),
                ..ProviderOptions::default()
            },
        );
        let _ = p.complete(&[ChatMessage::user("x")]).unwrap();
        assert!(
            mock.requests()[0].contains(r#""reasoning_effort":"low""#),
            "recipe-shaped effort field missing: {}",
            mock.requests()[0]
        );

        // thinking_disabled is the explicit off-switch: it wins.
        let both = HttpProvider::with_options(
            cfg,
            ProviderOptions {
                thinking_disabled: true,
                reasoning_effort: Some(ReasoningEffort::Max),
                ..ProviderOptions::default()
            },
        );
        let _ = both.complete(&[ChatMessage::user("x")]).unwrap();
        let sent = mock.requests()[1].clone();
        assert!(
            sent.contains(r#""thinking":{"type":"disabled"}"#),
            "disable dial missing: {sent}"
        );
        assert!(
            !sent.contains("reasoning_effort"),
            "disable must win: {sent}"
        );
    }
}
