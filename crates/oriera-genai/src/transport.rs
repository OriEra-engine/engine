//! Transport abstraction for vendor adapters (ADR 0003: the engine
//! core vendors zero SDKs). Adapters speak protocol; the platform
//! shell injects an HTTP-capable [`Transport`] (native client behind a
//! future feature, `fetch` on wasm, a test double in CI). Without an
//! injected transport, remote providers resolve to the offline
//! [`crate::MockProvider`] — the same stance L7's `create_provider`
//! takes while native HTTP adapters are pending.

use std::sync::Arc;

/// HTTP request as an adapter sees it: method, absolute URL, headers,
/// optional body. Deliberately minimal — no streaming, no timeouts;
/// those belong to the shell's implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    /// A JSON POST with a bearer token.
    pub fn post_json(url: impl Into<String>, token: &str, body: String) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            headers: vec![
                ("Authorization".into(), format!("Bearer {token}")),
                ("Content-Type".into(), "application/json".into()),
            ],
            body: Some(body.into_bytes()),
        }
    }

    /// A GET with a bearer token.
    pub fn get(url: impl Into<String>, token: &str) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            headers: vec![("Authorization".into(), format!("Bearer {token}"))],
            body: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// The response half: status code and body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            body: body.into(),
        }
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Injection point for HTTP. Implementations must be deterministic
/// for tests; production ones add timeouts and retries on their own
/// terms — adapters never assume either.
pub trait Transport: Send + Sync {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, String>;
}

/// Shared handle type for transports.
pub type SharedTransport = Arc<dyn Transport>;
