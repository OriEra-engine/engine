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
//! - **parameter healing (v3)** — two protocol adaptations, each at
//!   most once per call, are NOT transport retries: a 400 whose body
//!   names `temperature` (gateways exposing Claude/Kimi channels that
//!   only support the default) is re-sent with temperature 1.0, and a
//!   200 with `finish_reason=length` and empty content (reasoning
//!   models spending the whole budget on thinking) is re-sent with a
//!   doubled `max_tokens` (capped 2048);
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

/// SSE per-line byte cap (安全审计 P2): one `data:` line beyond this is
/// a hostile stream, not a model reply.
const SSE_LINE_CAP: usize = 1024 * 1024;
/// SSE whole-stream byte cap.
const SSE_TOTAL_CAP: usize = 16 * 1024 * 1024;

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
    ChatMessage, EmbeddingProvider, InferError, ModelProvider, ProviderConfig, ProviderKind,
    ProviderOptions, Role, StreamedOutcome, ToolCall, ToolDef, ToolOutcome,
};
use oriera_common::TokenUsage;
use serde_json::{json, Value};
use std::io::{BufReader, Read};
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
        self.build_request_param(messages, tools, stream, 0.85, 260)
    }

    /// Parameterized wire builder (v3 参数自愈轮复用)：temperature 与
    /// max_tokens 由调用方给出，其余与 build_request_ex 完全一致。
    fn build_request_param(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[crate::provider::ToolDef]>,
        stream: bool,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, InferError> {
        let mut body: Value = match self.dialect {
            Dialect::OpenAi => json!({
                "model": self.cfg.model,
                "messages": messages
                    .iter()
                    .map(message_openai)
                    .collect::<Vec<_>>(),
                "temperature": temperature,
                "max_tokens": max_tokens,
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
                    "max_tokens": max_tokens,
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
                    // 去依赖化（#106，评审共识）：wire 值就是 snake_case
                    // 字符串，本地 as_str 与 recipe-core 序列化逐字节等价
                    //（every_effort_variant_maps_to_the_recipe_snake_case
                    // wire 测试钉死），proc-macro 供应链面不再值得。
                    body["reasoning_effort"] = serde_json::json!(effort.as_str());
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
            .map_err(|e| InferError::Network(self.scrub(e.to_string())))?;
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
        // Anthropic bills `input_tokens` and `cache_read_input_tokens`
        // as DISJOINT slices of the prompt — normalize here so the
        // engine-wide invariant "cached ⊆ prompt" is manufactured at the
        // parse boundary instead of trusting the vendor's shape (the
        // min() clamp alone would truncate real cache reads to the
        // fresh-token count and misbill the world).
        let (prompt, cached) = if self.dialect == Dialect::Anthropic {
            let fresh = v
                .pointer("/usage/input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let read = v
                .pointer("/usage/cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (fresh + read, read)
        } else {
            let p = v
                .pointer("/usage/prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (p, v.pointer("/usage").map_or(0, |u| usage_cached(u, p)))
        };
        let usage = TokenUsage {
            prompt,
            completion: v
                .pointer("/usage/completion_tokens")
                .or_else(|| v.pointer("/usage/output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cached,
        };
        Ok((text, usage))
    }
}

/// `BufRead::read_line` without the unbounded-growth footgun: stops at
/// `cap` bytes and reports an overflow via `Err`. Returns bytes read
/// (0 = EOF); the bytes land in `out` (without the terminator on
/// overflow — the caller errors out anyway).
/// A line exceeded the cap — a distinct type so the caller ranks it
/// `Parse` (the endpoint answered garbage) without overloading
/// `ErrorKind::InvalidData` (which is where real TLS protocol faults
/// also land; those must keep their one Network retry).
#[derive(Debug)]
enum LineCapError {
    /// Real transport fault (keeps the caller's Network retry).
    Io(std::io::Error),
    /// The line blew the cap (ranks Parse: the endpoint answered garbage).
    Overflow(usize),
}

/// `BufRead::read_line` without the unbounded-growth footgun: stops at
/// `cap` bytes (a terminator AT byte `cap` is accepted — closed
/// interval); overflow returns [`LineCapOverflow`]. Returns bytes read
/// (0 = EOF).
fn read_line_capped(
    reader: &mut impl std::io::BufRead,
    out: &mut Vec<u8>,
    cap: usize,
) -> Result<usize, LineCapError> {
    let mut read = 0usize;
    loop {
        let available = reader.fill_buf().map_err(LineCapError::Io)?;
        if available.is_empty() {
            return Ok(read);
        }
        let take = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |pos| pos + 1);
        if read + take > cap {
            return Err(LineCapError::Overflow(cap));
        }
        out.extend_from_slice(&available[..take]);
        reader.consume(take);
        read += take;
        if take > 0 && *out.last().expect("just extended") == b'\n' {
            return Ok(read);
        }
    }
}

/// finish_reason=length 且内容为空/null —— 推理模型把预算全花在思考上的
/// wire 判据（v3 自愈②）。content 字段缺失也按空处理。
fn is_length_truncated_empty(raw: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if v.pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        != Some("length")
    {
        return false;
    }
    v.pointer("/choices/0/message/content")
        .is_none_or(|c| c.as_str().is_none_or(str::is_empty))
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
        // v3 参数自愈（与 kernel.js 镜像，各至多一轮，非传输重试）：
        // ①400 点名 temperature → 归一 1.0 重发；②finish_reason=length 且
        // 内容为空 → max_tokens 加倍重发（上限 2048）。Status/Parse/Timeout
        // 的既有分类与零重试语义不变（设计 §5）。
        let mut temperature = 0.85f32;
        let mut max_tokens = 260u32;
        let raw = match self.post_json(&body) {
            Ok(raw) => raw,
            Err(InferError::Status { code: 400, detail })
                if self.dialect == Dialect::OpenAi
                    && temperature != 1.0
                    && detail.to_lowercase().contains("temperature") =>
            {
                temperature = 1.0;
                let healed =
                    self.build_request_param(messages, None, false, temperature, max_tokens)?;
                self.post_json(&healed)?
            }
            Err(e) => return Err(e),
        };
        let raw = if max_tokens < 2048 && is_length_truncated_empty(&raw) {
            max_tokens = (max_tokens * 2).min(2048);
            let healed =
                self.build_request_param(messages, None, false, temperature, max_tokens)?;
            self.post_json(&healed)?
        } else {
            raw
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
        let (chunks, calls) = self.sse_stream(messages, None, SSE_LINE_CAP, SSE_TOTAL_CAP)?;
        if !calls.is_empty() {
            return Err(InferError::Parse(
                "stream carried tool calls but none were requested".into(),
            ));
        }
        Ok(chunks)
    }

    /// #105 second half: streaming + tools. Tool requests arrive as
    /// vendor stream fragments and are aggregated into complete
    /// [`ToolCall`]s here — the caller sees the same [`ToolCall`] shape
    /// as the blocking path, so downstream tool loops need no branching.
    fn complete_stream_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<StreamedOutcome, InferError> {
        let (chunks, calls) =
            self.sse_stream(messages, Some(tools), SSE_LINE_CAP, SSE_TOTAL_CAP)?;
        Ok(StreamedOutcome {
            chunks,
            tool_calls: calls,
        })
    }

    fn last_usage(&self) -> TokenUsage {
        self.usage.lock().map(|u| *u).unwrap_or(TokenUsage::ZERO)
    }

    fn current_seed(&self) -> Option<u64> {
        // #102: the ledger needs the run's seed identity to tell
        // same-prompt/different-seed runs apart (Pinned 对账)。
        self.options.seed
    }

    fn determinism(&self) -> oriera_common::DeterminismLevel {
        // B2: seed pinned = best-effort replay; unseeded cloud calls are
        // recorded-only. (架构评审 P1：本 override 曾在批二 splice 中丢失、
        // Pinned 档一度成死代码——pinned_level_is_self_reported 钉死。)
        if self.options.seed.is_some() {
            oriera_common::DeterminismLevel::Pinned
        } else {
            oriera_common::DeterminismLevel::Recorded
        }
    }
}

/// Per-tool-call accumulator for SSE tool aggregation (#105): OpenAI
/// streams `id`/`name` once then slices `arguments` across frames;
/// Anthropic names the block up front and slices `partial_json`.
#[derive(Default)]
struct StreamToolAcc {
    id: String,
    name: String,
    args: String,
}

impl HttpProvider {
    /// One SSE pass: text deltas in arrival order + tool calls
    /// aggregated from vendor fragments (#105). `tools == None` = plain
    /// stream (the wire request omits the vocabulary entirely, so a
    /// tool-call reply there is the endpoint misbehaving and surfaces as
    /// [`InferError::Parse`]). Cap guard (安全审计 P2): one `data:` line
    /// may never exceed `line_cap` bytes and the whole stream
    /// `total_cap` bytes — a hostile endpoint streaming one endless line
    /// must not be able to balloon the client; overflow is `Parse`.
    fn sse_stream(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDef]>,
        line_cap: usize,
        total_cap: usize,
    ) -> Result<(Vec<String>, Vec<ToolCall>), InferError> {
        if self.cfg.api_key.trim().is_empty() && !self.is_local_endpoint() {
            return Err(InferError::Provider(
                "no api key configured — the key only ever enters via the shell (env/UI), never the engine".into(),
            ));
        }
        let body = self.build_request_ex(messages, tools, true)?;
        let resp = self.send(&body)?;
        let mut reader = BufReader::new(resp.into_reader());
        let mut chunks: Vec<String> = Vec::new();
        // OpenAI: `delta.tool_calls[]` slots by `index` (args arrive as
        // string fragments); Anthropic: content blocks by block `index`
        // (args arrive as `input_json_delta.partial_json` fragments).
        // `None` slot = text block (skipped).
        let mut tool_acc: Vec<Option<StreamToolAcc>> = Vec::new();
        let mut usage = TokenUsage::ZERO;
        let mut line = String::new();
        let mut total: usize = 0;
        loop {
            line.clear();
            let mut chunk = Vec::new();
            let n = match read_line_capped(&mut reader, &mut chunk, line_cap) {
                Ok(n) => n,
                // 帽溢出 = 端点答了垃圾（Parse），不是没够到对端——
                // 不吃掉 Network 该有的唯一一次重试。
                Err(LineCapError::Overflow(cap)) => {
                    return Err(InferError::Parse(format!("sse line exceeded {cap} bytes")))
                }
                Err(LineCapError::Io(e)) => {
                    return Err(InferError::Network(self.scrub(e.to_string())))
                }
            };
            total += n;
            if total > total_cap {
                return Err(InferError::Parse(format!(
                    "stream exceeded {total_cap} bytes total"
                )));
            }
            if n == 0 {
                break; // EOF before [DONE]/message_stop — take what landed
            }
            line.push_str(&String::from_utf8_lossy(&chunk));
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
                    if let Some(tcs) = v
                        .pointer("/choices/0/delta/tool_calls")
                        .and_then(Value::as_array)
                    {
                        for tc in tcs {
                            let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            if tool_acc.len() <= idx {
                                tool_acc.resize_with(idx + 1, || None);
                            }
                            let slot = tool_acc[idx].get_or_insert_with(StreamToolAcc::default);
                            if let Some(id) = tc.get("id").and_then(Value::as_str) {
                                if slot.id.is_empty() {
                                    slot.id = id.to_string();
                                }
                            }
                            if let Some(name) = tc.pointer("/function/name").and_then(Value::as_str)
                            {
                                if slot.name.is_empty() {
                                    slot.name = name.to_string();
                                }
                            }
                            if let Some(frag) =
                                tc.pointer("/function/arguments").and_then(Value::as_str)
                            {
                                slot.args.push_str(frag);
                            }
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
                    Some("content_block_start") => {
                        if v.pointer("/content_block/type").and_then(Value::as_str)
                            == Some("tool_use")
                        {
                            let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            if tool_acc.len() <= idx {
                                tool_acc.resize_with(idx + 1, || None);
                            }
                            tool_acc[idx] = Some(StreamToolAcc {
                                id: v
                                    .pointer("/content_block/id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                name: v
                                    .pointer("/content_block/name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                args: String::new(),
                            });
                        }
                    }
                    Some("content_block_delta") => {
                        if let Some(t) = v.pointer("/delta/text").and_then(Value::as_str) {
                            chunks.push(t.to_string());
                        }
                        if v.pointer("/delta/type").and_then(Value::as_str)
                            == Some("input_json_delta")
                        {
                            let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            if let Some(frag) =
                                v.pointer("/delta/partial_json").and_then(Value::as_str)
                            {
                                if let Some(slot) = tool_acc.get_mut(idx).and_then(Option::as_mut) {
                                    slot.args.push_str(frag);
                                }
                            }
                        }
                    }
                    Some("message_start") => {
                        // Disjoint slices, same normalization as the
                        // blocking parse path (cached ⊆ prompt).
                        let fresh = v
                            .pointer("/message/usage/input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        let read = v
                            .pointer("/message/usage/cache_read_input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        usage.prompt = fresh + read;
                        usage.cached = read;
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
        let tool_calls: Vec<ToolCall> = tool_acc
            .into_iter()
            .flatten()
            .filter(|a| !a.name.is_empty())
            .map(|a| ToolCall {
                id: a.id,
                name: a.name,
                arguments_json: a.args,
            })
            .collect();
        if chunks.is_empty() && tool_calls.is_empty() {
            return Err(InferError::Parse(
                "stream carried no text deltas (200 but empty SSE body)".into(),
            ));
        }
        if let Ok(mut u) = self.usage.lock() {
            *u = usage;
        }
        Ok((chunks, tool_calls))
    }

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
        // Anthropic bills `input_tokens` and `cache_read_input_tokens`
        // as DISJOINT slices of the prompt — normalize here so the
        // engine-wide invariant "cached ⊆ prompt" is manufactured at the
        // parse boundary instead of trusting the vendor's shape (the
        // min() clamp alone would truncate real cache reads to the
        // fresh-token count and misbill the world).
        let (prompt, cached) = if self.dialect == Dialect::Anthropic {
            let fresh = v
                .pointer("/usage/input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let read = v
                .pointer("/usage/cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (fresh + read, read)
        } else {
            let p = v
                .pointer("/usage/prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (p, v.pointer("/usage").map_or(0, |u| usage_cached(u, p)))
        };
        let usage = TokenUsage {
            prompt,
            completion: v
                .pointer("/usage/completion_tokens")
                .or_else(|| v.pointer("/usage/output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cached,
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

/// OpenAI-compatible embeddings client (#101 step two): POST
/// `{base}/embeddings` `{"model","input":[…]}` →
/// `{"data":[{"index","embedding":[f32]}]}`. Zhipu/OpenAiCompat only —
/// Anthropic has no embeddings endpoint and Rule is offline, so
/// construction REFUSES them (an honest failure, not a half-working
/// seat). Vectors are reassembled by the vendor's `index` field, never
/// by arrival order.
pub struct HttpEmbeddingProvider {
    agent: ureq::Agent,
    cfg: ProviderConfig,
    url: String,
    label: String,
}

impl HttpEmbeddingProvider {
    /// 20s overall / 5s connect — same defaults as [`HttpProvider::new`].
    pub fn new(cfg: ProviderConfig) -> Result<Self, InferError> {
        Self::with_timeouts(cfg, Duration::from_secs(20), Duration::from_secs(5))
    }

    /// Timeout override for constrained shells and tests.
    pub fn with_timeouts(
        cfg: ProviderConfig,
        overall: Duration,
        connect: Duration,
    ) -> Result<Self, InferError> {
        if Dialect::of(cfg.kind) != Dialect::OpenAi {
            return Err(InferError::Provider(format!(
                "embeddings need an OpenAI-compatible endpoint ({:?} has none)",
                cfg.kind
            )));
        }
        let base = cfg
            .base_url
            .clone()
            .unwrap_or_else(|| cfg.kind.default_base_url().to_string());
        let base = base.trim_end_matches('/').to_string();
        let label = format!("{}:{}", cfg.kind.id(), cfg.model);
        Ok(Self {
            agent: ureq::AgentBuilder::new()
                .timeout(overall)
                .timeout_connect(connect)
                .build(),
            url: format!("{base}/embeddings"),
            cfg,
            label,
        })
    }

    fn is_local(&self) -> bool {
        self.url.starts_with("http://127.0.0.1") || self.url.starts_with("http://localhost")
    }

    /// Same scrub discipline as the chat path: the key (and its Bearer
    /// form) must never survive into an error message.
    fn scrub(&self, s: &str) -> String {
        let key = self.cfg.api_key.trim();
        if key.is_empty() {
            return s.to_string();
        }
        s.replace(key, "***")
            .replace(&format!("Bearer {key}"), "Bearer ***")
    }
}

impl EmbeddingProvider for HttpEmbeddingProvider {
    fn id(&self) -> &str {
        &self.label
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, InferError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if self.cfg.api_key.trim().is_empty() && !self.is_local() {
            return Err(InferError::Provider(
                "no api key configured — the key only ever enters via the shell (env/UI), never the engine".into(),
            ));
        }
        let body = json!({ "model": self.cfg.model, "input": texts });
        let resp = self
            .agent
            .post(&self.url)
            .set("Authorization", &format!("Bearer {}", self.cfg.api_key))
            .send_string(&body.to_string())
            .map_err(|e| InferError::Network(self.scrub(&e.to_string())))?;
        let raw = resp
            .into_string()
            .map_err(|e| InferError::Network(self.scrub(&e.to_string())))?;
        let v: Value = serde_json::from_str(&raw)
            .map_err(|e| InferError::Parse(format!("embeddings body unreadable: {e}")))?;
        let n = texts.len();
        let mut out: Vec<Option<Vec<f32>>> = (0..n).map(|_| None).collect();
        if let Some(rows) = v.get("data").and_then(Value::as_array) {
            for row in rows {
                let idx = row.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let emb: Option<Vec<f32>> =
                    row.get("embedding").and_then(Value::as_array).map(|a| {
                        a.iter()
                            .filter_map(Value::as_f64)
                            .map(|x| x as f32)
                            .collect()
                    });
                if idx < n {
                    if let Some(emb) = emb {
                        out[idx] = Some(emb);
                    }
                }
            }
        }
        out.into_iter()
            .enumerate()
            .map(|(i, slot)| {
                slot.ok_or_else(|| {
                    InferError::Parse(format!("embeddings response missing index {i}"))
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    // 测试腿独用（lib 侧 wire 赋值只读字段不再需要类型 import）
    use crate::provider::ReasoningEffort;
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
    fn current_seed_reports_the_configured_option() {
        // #102: ledger 对账依赖 seed 身份——HttpProvider 必须如实上报
        // ProviderOptions.seed（未配置 = None）。
        let cfg = ProviderConfig {
            kind: ProviderKind::OpenAiCompat,
            model: "m".into(),
            api_key: "k".into(),
            base_url: Some("https://example.invalid/v1".into()),
        };
        let plain = HttpProvider::with_options(cfg.clone(), ProviderOptions::default());
        assert_eq!(plain.current_seed(), None);
        let seeded = HttpProvider::with_options(
            cfg,
            ProviderOptions {
                seed: Some(42),
                ..ProviderOptions::default()
            },
        );
        assert_eq!(seeded.current_seed(), Some(42));
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

    // ---- v3 参数自愈 ----

    #[test]
    fn temperature_400_heals_to_default_one() {
        // 自愈①：网关 400 点名 temperature（如仅支持默认温度的
        // Claude/Kimi 渠道）→ 归一 1.0 重发一次。
        let mock = Mock::spawn(vec![
            Step::Reply(
                400,
                r#"{"error":{"message":"claude-sonnet-5-5 does not support non-default temperature"}}"#.into(),
            ),
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"好了"}}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#.into(),
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
        assert_eq!(p.complete(&[ChatMessage::user("hi")]).unwrap(), "好了");
        let reqs = mock.requests();
        assert!(
            reqs[0].contains(r#""temperature":0.85"#),
            "first sends default"
        );
        assert!(
            reqs[1].contains(r#""temperature":1.0"#),
            "heal round sends 1.0: {}",
            reqs[1]
        );
    }

    #[test]
    fn length_truncated_empty_content_doubles_budget_once() {
        // 自愈②：200 但 finish_reason=length 且内容空（思考吃光预算）
        // → max_tokens 加倍重发一次（260 → 520）。
        let mock = Mock::spawn(vec![
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":""},"finish_reason":"length"}],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#.into(),
            ),
            Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"有内容了"}}],"usage":{"prompt_tokens":10,"completion_tokens":30}}"#.into(),
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
        assert_eq!(p.complete(&[ChatMessage::user("hi")]).unwrap(), "有内容了");
        let reqs = mock.requests();
        assert!(reqs[0].contains(r#""max_tokens":260"#));
        assert!(
            reqs[1].contains(r#""max_tokens":520"#),
            "heal round doubles the budget: {}",
            reqs[1]
        );
    }

    #[test]
    fn plain_400_still_fails_without_parameter_healing() {
        // 自愈不误伤：非温度类 400（如额度不足）不触发任何重发。
        let mock = Mock::spawn(vec![
            Step::Reply(400, r#"{"error":{"message":"insufficient quota"}}"#.into()),
            Step::Reply(400, r#"{"error":{"message":"insufficient quota"}}"#.into()),
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
        match p.complete(&[ChatMessage::user("x")]) {
            Err(InferError::Status { code: 400, .. }) => {}
            other => panic!("expected Status(400), got {other:?}"),
        }
        assert_eq!(
            mock.requests().len(),
            1,
            "no heal round, no transport retry"
        );
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
            r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"spawn","arguments":"{\"count\":3}"}}]}}],"usage":{"prompt_tokens":30,"completion_tokens":8,"prompt_cache_hit_tokens":12}}"#.into(),
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
        assert_eq!(
            p.last_usage().cached,
            12,
            "tools path meters cache hits too"
        );
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
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"prompt_cache_hit_tokens\":7}}\n\n\
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
        assert_eq!(p.last_usage().cached, 7, "SSE path meters cache hits too");
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

    /// #105 second half: OpenAI streams a tool request as index-sliced
    /// fragments (`id`/`name` once, `arguments` in pieces) — the caller
    /// must see ONE complete [`ToolCall`] with concatenated JSON.
    #[test]
    fn openai_sse_stream_tool_calls_aggregate() {
        let sse = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"function\":{\"name\":\"set_gravity\",\"arguments\":\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"g\\\":\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"9.8}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":5}}\n\n\
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
        let tools = vec![crate::provider::ToolDef {
            name: "set_gravity".into(),
            description: "set world gravity".into(),
            parameters_json: "{}".into(),
        }];
        let out = p
            .complete_stream_with_tools(&[ChatMessage::user("重力调大")], &tools)
            .unwrap();
        assert!(out.chunks.is_empty(), "tool-only stream has no text");
        assert_eq!(out.tool_calls.len(), 1);
        let call = &out.tool_calls[0];
        assert_eq!(call.id, "call_9");
        assert_eq!(call.name, "set_gravity");
        assert_eq!(call.arguments_json, "{\"g\":9.8}");
        assert_eq!(p.last_usage().completion, 5);
    }

    /// #105: Anthropic streams tools as `content_block_start`(tool_use)
    /// + `input_json_delta` partial-JSON fragments + stop.
    #[test]
    fn anthropic_sse_stream_tool_use_aggregate() {
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":8}}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"spawn\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"n\\\":\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"3}\"}}\n\n\
event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n\
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
        let tools = vec![crate::provider::ToolDef {
            name: "spawn".into(),
            description: "spawn entities".into(),
            parameters_json: "{}".into(),
        }];
        let out = p
            .complete_stream_with_tools(&[ChatMessage::user("刷三个")], &tools)
            .unwrap();
        assert!(out.chunks.is_empty());
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].id, "toolu_1");
        assert_eq!(out.tool_calls[0].name, "spawn");
        assert_eq!(out.tool_calls[0].arguments_json, "{\"n\":3}");
        assert_eq!(p.last_usage().prompt, 8);
        assert_eq!(p.last_usage().completion, 7);
    }

    /// #105: mixed stream — text deltas land in order AND the tool call
    /// aggregates; multi-call streams keep vendor index order.
    #[test]
    fn openai_sse_stream_mixed_text_and_two_tools() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"好的，\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"安排。\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"function\":{\"name\":\"despawn\",\"arguments\":\"{}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"set_gravity\",\"arguments\":\"{\\\"g\\\":1}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
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
        let mk = |name: &str| crate::provider::ToolDef {
            name: name.into(),
            description: String::new(),
            parameters_json: "{}".into(),
        };
        let tools = vec![mk("set_gravity"), mk("despawn")];
        let out = p
            .complete_stream_with_tools(&[ChatMessage::user("整理世界")], &tools)
            .unwrap();
        assert_eq!(out.chunks, vec!["好的，", "安排。"]);
        assert_eq!(out.tool_calls.len(), 2, "vendor index order kept");
        assert_eq!(out.tool_calls[0].id, "call_a");
        assert_eq!(out.tool_calls[1].id, "call_b");
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
        // Anthropic's input_tokens (40) and cache_read (25) are disjoint:
        // normalized prompt = 65, cached = 25 — the clamp must NOT cut
        // the cache read down to the fresh-token count.
        assert_eq!(
            pa.last_usage(),
            TokenUsage {
                prompt: 65,
                completion: 5,
                cached: 25
            }
        );

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
        // Structural assertion — immune to serde_json key-order drift.
        let body: serde_json::Value =
            serde_json::from_str(sent.split("\r\n\r\n").nth(1).expect("http body"))
                .expect("wire body is json");
        let rf = &body["response_format"];
        assert_eq!(rf["type"], "json_schema");
        assert_eq!(rf["json_schema"]["name"], "reply");
        assert_eq!(rf["json_schema"]["strict"], true);
        assert_eq!(
            rf["json_schema"]["schema"],
            serde_json::from_str::<serde_json::Value>(schema).unwrap(),
            "schema body must survive the round trip intact"
        );
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

    #[test]
    fn anthropic_with_schema_records_but_never_sends_response_format() {
        // Anthropic 的 messages 协议没有 response_format：schema 经校验
        // 后记录不上线（caps 行 structured_output: false 的兜底证据）。
        let mock = Mock::spawn(vec![Step::Reply(
            200,
            r#"{"content":[{"type":"text","text":"好"}],"usage":{"input_tokens":1,"output_tokens":1}}"#.into(),
        )]);
        let p = HttpProvider::with_options(
            ProviderConfig {
                kind: ProviderKind::Anthropic,
                model: "claude".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            ProviderOptions {
                response_schema: Some(r#"{"type":"object"}"#.to_string()),
                ..ProviderOptions::default()
            },
        );
        let _ = p.complete(&[ChatMessage::user("x")]).unwrap();
        let sent = mock.requests()[0].clone();
        assert!(
            !sent.contains("response_format"),
            "anthropic must not send it: {sent}"
        );
    }

    #[test]
    fn usage_cached_prefers_the_unambiguous_deepseek_field() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"usage":{"prompt_tokens":50,"prompt_cache_hit_tokens":40,"prompt_tokens_details":{"cached_tokens":30},"cache_read_input_tokens":20}}"#,
        )
        .unwrap();
        let u = v.pointer("/usage").unwrap();
        assert_eq!(usage_cached(u, 50), 40);
    }

    #[test]
    fn every_effort_variant_maps_to_the_recipe_snake_case() {
        for (e, wire) in [
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Xhigh, "xhigh"),
            (ReasoningEffort::Max, "max"),
        ] {
            let mock = Mock::spawn(vec![Step::Reply(
                200,
                r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.into(),
            )]);
            let p = HttpProvider::with_options(
                ProviderConfig {
                    kind: ProviderKind::OpenAiCompat,
                    model: "m".into(),
                    api_key: "k".into(),
                    base_url: Some(mock.base_url()),
                },
                ProviderOptions {
                    reasoning_effort: Some(e),
                    ..ProviderOptions::default()
                },
            );
            let _ = p.complete(&[ChatMessage::user("x")]).unwrap();
            let sent = mock.requests()[0].clone();
            assert!(
                sent.contains(&format!(r#""reasoning_effort":"{wire}""#)),
                "{e:?} wire missing: {sent}"
            );
        }
    }

    #[test]
    fn sse_stream_caps_hostile_one_line_payloads() {
        // 安全审计 P2 的回归钉：一条 2MB 的 data 行必须 Parse 报错，
        // 而不是把客户端内存灌爆。
        let huge = format!(
            "data: {{\"junk\":\"{}\"}}

",
            "x".repeat(2 * 1024 * 1024)
        );
        let mock = Mock::spawn(vec![Step::Reply(200, huge)]);
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
            Err(InferError::Parse(m)) => assert!(m.contains("exceeded"), "{m}"),
            other => panic!("expected Parse(capped), got {other:?}"),
        }
    }

    #[test]
    fn read_line_capped_boundary_semantics() {
        use std::io::Cursor;
        // 恰在 cap 处的 \n：闭区间语义，接受。
        let mut out = Vec::new();
        let n = read_line_capped(&mut Cursor::new(b"abcde\nxy"), &mut out, 6).unwrap();
        assert_eq!(n, 6);
        assert_eq!(out, b"abcde\n");
        // cap+1 → Overflow（cap 值上报）。
        let mut out = Vec::new();
        match read_line_capped(&mut Cursor::new(b"abcdef\n"), &mut out, 6) {
            Err(LineCapError::Overflow(cap)) => assert_eq!(cap, 6),
            other => panic!("expected Overflow, got {other:?}"),
        }
        // 无换行的 EOF 尾行（cap 内）当数据行返回。
        let mut out = Vec::new();
        let n = read_line_capped(&mut Cursor::new(b"tail"), &mut out, 6).unwrap();
        assert_eq!(n, 4);
        assert_eq!(out, b"tail");
    }

    #[test]
    fn sse_total_cap_is_enforced_too() {
        let sse = "data: {\"a\":1}\n\n".repeat(10); // 16B 行 × 10 = 160B > 64B total
        let mock = Mock::spawn(vec![Step::Reply(200, sse)]);
        let p = provider(
            ProviderConfig {
                kind: ProviderKind::OpenAiCompat,
                model: "m".into(),
                api_key: "test-key".into(),
                base_url: Some(mock.base_url()),
            },
            Duration::from_secs(5),
        );
        match p.sse_stream(&[ChatMessage::user("x")], None, 1024, 64) {
            Err(InferError::Parse(m)) => assert!(m.contains("exceeded 64 bytes"), "{m}"),
            other => panic!("expected total-cap Parse, got {other:?}"),
        }
    }
}
