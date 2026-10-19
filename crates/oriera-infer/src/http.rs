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

// Native-only feature: the wasm channel is the kernel.js mirror, and
// ureq does not compile for wasm targets — fail fast with a message
// that says which channel to use instead of a wall of foreign errors.
#[cfg(target_family = "wasm")]
compile_error!(
    "oriera-infer `native-http` is native-only; wasm builds use the \
     kernel.js HttpChatProvider mirror (examples/11-ai-cafe/engine/kernel.js)"
);

use crate::provider::{
    ChatMessage, InferError, ModelProvider, ProviderConfig, ProviderKind, ProviderOptions, Role,
};
use oriera_common::TokenUsage;
use serde_json::{json, Value};
use std::io::Read;
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

    fn build_request(&self, messages: &[ChatMessage]) -> String {
        match self.dialect {
            Dialect::OpenAi => {
                let mut body = json!({
                    "model": self.cfg.model,
                    "messages": messages
                        .iter()
                        .map(|m| json!({ "role": role_str(m.role), "content": m.content }))
                        .collect::<Vec<_>>(),
                    "temperature": 0.85,   // kernel.js parity
                    "max_tokens": 260,
                });
                // v2.1 kernel-v2 parity: construction-time wire options.
                if let Some(seed) = self.options.seed {
                    body["seed"] = json!(seed);
                }
                if self.options.thinking_disabled {
                    body["thinking"] = json!({ "type": "disabled" });
                }
                if self.options.json_mode {
                    body["response_format"] = json!({ "type": "json_object" });
                }
                body.to_string()
            }
            Dialect::Anthropic => {
                let system = messages
                    .iter()
                    .filter(|m| m.role == Role::System)
                    .map(|m| m.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                json!({
                    "model": self.cfg.model,
                    "system": system,
                    "max_tokens": 260,
                    "messages": messages
                        .iter()
                        .filter(|m| m.role != Role::System)
                        .map(|m| json!({ "role": role_str(m.role), "content": m.content }))
                        .collect::<Vec<_>>(),
                })
                .to_string()
            }
        }
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

    fn attempt(&self, body: &str) -> Result<String, InferError> {
        let req = self.apply_headers(self.agent.post(&self.url));
        match req.send_string(body) {
            Ok(resp) => self.read_body(resp.into_reader()),
            Err(ureq::Error::Status(code, resp)) => {
                // Headers first — into_reader consumes the response.
                let retry_after = resp.header("retry-after").map(str::to_string);
                let mut detail = self.read_body(resp.into_reader())?;
                if let Some(ra) = retry_after {
                    detail.push_str(&format!(" [retry-after: {ra}]"));
                }
                Err(InferError::Status {
                    code,
                    detail: self.scrub(detail),
                })
            }
            Err(ureq::Error::Transport(t)) => {
                let msg = t.to_string();
                // Two signals, both needed:
                // - ureq's `kind` separates the connect phase
                //   (ConnectionFailed/Dns/…) from mid-stream io — the
                //   design page's line is "connection phase = never
                //   reached the far side = Network", and ureq even
                //   words a refused connect as "connection timed out"
                //   on Windows, so the kind, not the wording, decides;
                // - for kind Io the deadline shows up in the (possibly
                //   localized!) message: the word, or the OS code —
                //   WSAETIMEDOUT (10060) / ETIMEDOUT (110), which are
                //   locale-independent by construction.
                let deadline = {
                    let lower = msg.to_lowercase();
                    lower.contains("timed out")
                        || lower.contains("timeout")
                        || msg.contains("(os error 10060)")
                        || msg.contains("(os error 110)")
                };
                if t.kind() == ureq::ErrorKind::Io && deadline {
                    Err(InferError::Timeout)
                } else {
                    Err(InferError::Network(self.scrub(msg)))
                }
            }
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
        let usage = TokenUsage {
            prompt: v
                .pointer("/usage/prompt_tokens")
                .or_else(|| v.pointer("/usage/input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            completion: v
                .pointer("/usage/completion_tokens")
                .or_else(|| v.pointer("/usage/output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        };
        Ok((text, usage))
    }
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
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
        let body = self.build_request(messages);
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

    fn last_usage(&self) -> TokenUsage {
        self.usage.lock().map(|u| *u).unwrap_or(TokenUsage::ZERO)
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
                completion: 3
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
                completion: 2
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
}
