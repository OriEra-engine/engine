//! CogView adapter (text→PNG/JPEG) — protocol only, transport injected
//! (ADR 0003). Speaks the OpenAI **images dialect**
//! (`POST {base}/images/generations`), so one adapter serves every
//! compatible endpoint: Zhipu CogView by default, OpenAI images or any
//! gateway via `base_url`/`model` overrides. Vendors are data.
//!
//! Wire shape:
//!
//! ```json
//! {"model":"cogview-3-flash","prompt":"..."}
//! {"created":0,"data":[{"url":"https://...png"} | {"b64_json":"..."}]}
//! ```
//!
//! Unlike Tripo/Seedance this endpoint is *synchronous*: the POST blocks
//! server-side until the image exists. Mapping that onto the task-shaped
//! boundary keeps the engine in control of when the blocking call runs:
//!
//! - `request_generation` — validates capabilities/key, registers the
//!   task, performs **no network I/O** (submit stays a cheap tick);
//! - poll #1 — issues the blocking POST; a `b64_json` reply completes
//!   immediately, a `url` reply moves the task to the download phase;
//! - poll #2 — fetches the bytes from the (signed) URL with **no**
//!   Authorization header.
//!
//! Production shells bound the blocking POST with their transport's
//! timeout/worker policy — adapters never assume either.

use crate::transport::{HttpRequest, SharedTransport};
use crate::{
    AssetFormat, GenAiError, GeneratedAsset, GenerationProvider, GenerationRequest,
    ProviderCapabilities, TaskId, TaskStatus,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// Prefix stamped on task ids so polls are self-describing.
const ID_PREFIX: &str = "cogview:";

/// Vendor default when `ProviderConfig::model` is empty.
pub const DEFAULT_MODEL: &str = "cogview-3-flash";

/// Where a task is inside the two-phase (create → maybe-download) flow.
#[derive(Debug, Clone)]
enum Phase {
    /// Registered, no vendor call yet.
    Create { prompt: String },
    /// Vendor returned a hosted URL; bytes not fetched yet.
    Download { url: String },
}

#[derive(Default)]
struct State {
    next: u64,
    tasks: HashMap<String, Phase>,
}

pub struct CogViewProvider {
    config: crate::ProviderConfig,
    transport: SharedTransport,
    state: Mutex<State>,
}

impl CogViewProvider {
    pub fn new(config: crate::ProviderConfig, transport: SharedTransport) -> Self {
        Self {
            config,
            transport,
            state: Mutex::new(State::default()),
        }
    }

    fn model(&self) -> &str {
        if self.config.model.is_empty() {
            DEFAULT_MODEL
        } else {
            &self.config.model
        }
    }

    fn create_url(&self) -> String {
        format!("{}/images/generations", self.config.base())
    }

    fn register(&self, prompt: &str) -> Result<TaskId, GenAiError> {
        let Ok(mut st) = self.state.lock() else {
            return Err(GenAiError::Provider("cogview state poisoned".into()));
        };
        st.next += 1;
        let key = format!("{ID_PREFIX}{}", st.next);
        st.tasks.insert(
            key.clone(),
            Phase::Create {
                prompt: prompt.into(),
            },
        );
        Ok(TaskId(key))
    }

    /// The blocking vendor call: POST the prompt, parse the first
    /// result entry into either inline bytes or a hosted URL.
    fn create_on_vendor(&self, prompt: &str) -> Result<create::Outcome, GenAiError> {
        let body = build_request_body(self.model(), prompt);
        let resp = self
            .transport
            .send(&HttpRequest::post_json(
                self.create_url(),
                &self.config.api_key,
                body,
            ))
            .map_err(GenAiError::Provider)?;
        if !resp.is_success() {
            return Err(GenAiError::Provider(format!(
                "cogview create returned HTTP {}",
                resp.status
            )));
        }
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| GenAiError::Provider(format!("cogview create body: {e}")))?;
        create::parse_first(&v)
    }

    fn fetch_public(&self, url: &str) -> Result<Vec<u8>, GenAiError> {
        let resp = self
            .transport
            .send(&HttpRequest::get_public(url))
            .map_err(GenAiError::Provider)?;
        if !resp.is_success() {
            return Err(GenAiError::Provider(format!(
                "cogview asset download returned HTTP {}",
                resp.status
            )));
        }
        Ok(resp.body)
    }

    fn asset(&self, payload: Vec<u8>) -> GeneratedAsset {
        GeneratedAsset {
            format: AssetFormat::Image,
            payload,
            provider_id: self.id().into(),
            usage: None,
            model: self.model().to_string(),
        }
    }
}

impl GenerationProvider for CogViewProvider {
    fn id(&self) -> &str {
        "cogview-images-v1"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            text_to_3d: false,
            image_to_3d: false,
            text_to_image: true,
            text_to_video: false,
            image_to_video: false,
            text_to_speech: false,
        }
    }

    fn request_generation(&self, req: &GenerationRequest) -> Result<TaskId, GenAiError> {
        if !self.capabilities().supports(req.kind) {
            return Err(GenAiError::Unsupported(format!(
                "cogview adapter serves text_to_image only; {} requested",
                req.kind.id()
            )));
        }
        if self.config.api_key.is_empty() {
            return Err(GenAiError::Provider(
                "cogview selected without ORIERA_GENAI_API_KEY".into(),
            ));
        }
        // No network here — submit stays a cheap tick; the blocking
        // vendor POST runs inside the first poll slice.
        self.register(&req.prompt)
    }

    fn poll_task(&self, task: &TaskId) -> Result<TaskStatus, GenAiError> {
        let phase = {
            let Ok(mut st) = self.state.lock() else {
                return Err(GenAiError::Provider("cogview state poisoned".into()));
            };
            match st.tasks.get_mut(&task.0) {
                Some(p) => p.clone(),
                None => return Err(GenAiError::Provider(format!("unknown task {}", task.0))),
            }
        };
        match phase {
            Phase::Create { prompt } => match self.create_on_vendor(&prompt)? {
                create::Outcome::Inline(bytes) => {
                    self.finish(task);
                    Ok(TaskStatus::Succeeded(self.asset(bytes)))
                }
                create::Outcome::Hosted(url) => {
                    let Ok(mut st) = self.state.lock() else {
                        return Err(GenAiError::Provider("cogview state poisoned".into()));
                    };
                    st.tasks.insert(task.0.clone(), Phase::Download { url });
                    Ok(TaskStatus::Running)
                }
            },
            Phase::Download { url } => {
                let bytes = self.fetch_public(&url)?;
                self.finish(task);
                Ok(TaskStatus::Succeeded(self.asset(bytes)))
            }
        }
    }
}

impl CogViewProvider {
    fn finish(&self, task: &TaskId) {
        if let Ok(mut st) = self.state.lock() {
            st.tasks.remove(&task.0);
        }
    }
}

/// Request body for the images dialect.
pub fn build_request_body(model: &str, prompt: &str) -> String {
    json!({ "model": model, "prompt": prompt }).to_string()
}

/// Parsing half, split out so tests can drive fixtures directly.
mod create {
    use super::decode_base64;
    use crate::GenAiError;
    use serde_json::Value;

    pub(crate) enum Outcome {
        /// `b64_json` — bytes are already here.
        Inline(Vec<u8>),
        /// `url` — one more fetch needed.
        Hosted(String),
    }

    pub(crate) fn parse_first(v: &Value) -> Result<Outcome, GenAiError> {
        let entry = v
            .pointer("/data/0")
            .ok_or_else(|| GenAiError::Provider("cogview reply missing data[0]".into()))?;
        if let Some(url) = entry.get("url").and_then(Value::as_str) {
            if !url.is_empty() {
                return Ok(Outcome::Hosted(url.to_string()));
            }
        }
        if let Some(b64) = entry.get("b64_json").and_then(Value::as_str) {
            let bytes = decode_base64(b64)
                .map_err(|e| GenAiError::Provider(format!("cogview b64_json: {e}")))?;
            return Ok(Outcome::Inline(bytes));
        }
        Err(GenAiError::Provider(
            "cogview reply has neither url nor b64_json".into(),
        ))
    }
}

/// Standard-alphabet base64 decode (padding required, whitespace
/// tolerated). Hand-rolled so the `providers` feature still vendors
/// zero crates beyond `serde_json` — the wasm slice stays tiny.
pub fn decode_base64(s: &str) -> Result<Vec<u8>, String> {
    const PAD: u8 = 64;
    let mut table = [PAD; 256];
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut invalid = [false; 256];
    for (i, &c) in alphabet.iter().enumerate() {
        table[c as usize] = i as u8;
    }
    for c in b" \t\r\n" {
        invalid[*c as usize] = true;
    }
    let clean: Vec<u8> = s.bytes().filter(|&b| !invalid[b as usize]).collect();
    if clean.is_empty() {
        return Ok(Vec::new());
    }
    if !clean.len().is_multiple_of(4) {
        return Err("length is not a multiple of 4".into());
    }
    // Padding is legal only as a suffix of at most two '=' characters —
    // anything else is malformed input, not data.
    let first_pad = clean.iter().position(|&b| b == b'=');
    if let Some(pos) = first_pad {
        let suffix = clean.len() - pos;
        if suffix > 2 || clean[pos..].iter().any(|&b| b != b'=') {
            return Err("misplaced padding".into());
        }
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    let (chunks, _) = clean.as_chunks::<4>();
    for chunk in chunks {
        let pad = |b: &u8| -> Result<u8, String> {
            if *b == b'=' {
                Ok(PAD)
            } else {
                let v = table[*b as usize];
                if v == PAD {
                    return Err(format!("invalid character 0x{:02x}", b));
                }
                Ok(v)
            }
        };
        let seps = {
            let mut n = 0;
            for b in chunk.iter().rev() {
                if *b == b'=' {
                    n += 1;
                } else {
                    break;
                }
            }
            n
        };
        if seps == 0 {
            let vals: [u8; 4] = [
                pad(&chunk[0])?,
                pad(&chunk[1])?,
                pad(&chunk[2])?,
                pad(&chunk[3])?,
            ];
            let n = (u32::from(vals[0]) << 18)
                | (u32::from(vals[1]) << 12)
                | (u32::from(vals[2]) << 6)
                | u32::from(vals[3]);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
            out.push(n as u8);
        } else if seps == 1 {
            let vals: [u8; 3] = [pad(&chunk[0])?, pad(&chunk[1])?, pad(&chunk[2])?];
            let n =
                (u32::from(vals[0]) << 18) | (u32::from(vals[1]) << 12) | (u32::from(vals[2]) << 6);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
        } else if seps == 2 {
            let vals: [u8; 2] = [pad(&chunk[0])?, pad(&chunk[1])?];
            let n = (u32::from(vals[0]) << 18) | (u32::from(vals[1]) << 12);
            out.push((n >> 16) as u8);
        } else {
            return Err("chunk is all padding".into());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{fixture_transport, ScriptedTransport};
    use crate::transport::HttpResponse;
    use std::sync::Arc;

    /// Scripted transport: pops queued responses per request (same
    /// pattern as the tripo adapter's tests).
    fn provider(t: Arc<ScriptedTransport>) -> CogViewProvider {
        CogViewProvider::new(
            crate::ProviderConfig {
                kind: crate::ProviderKind::CogView,
                model: String::new(),
                api_key: "test-key".into(),
                base_url: Some("https://api.example.test".into()),
            },
            t,
        )
    }

    fn png_response(bytes: Vec<u8>) -> HttpResponse {
        HttpResponse::ok(bytes)
    }

    #[test]
    fn url_path_is_create_then_download() {
        // Pop order is reversed: download bytes, create reply.
        let t = fixture_transport(vec![
            png_response(crate::tiny_png()),
            HttpResponse::ok(
                r#"{"created":0,"data":[{"url":"https://cdn.example.test/sky.png"}]}"#,
            ),
        ]);
        let p = provider(t.clone());

        let id = p
            .request_generation(&GenerationRequest::text_to_image(
                "sky_dome",
                "雨夜霓虹的天空",
            ))
            .unwrap();
        // Submit must not have touched the network.
        assert!(t.requests.lock().unwrap().is_empty());

        assert_eq!(p.poll_task(&id).unwrap(), TaskStatus::Running);
        match p.poll_task(&id).unwrap() {
            TaskStatus::Succeeded(a) => {
                assert_eq!(a.format, AssetFormat::Image);
                assert!(crate::validate_asset(&a, &Default::default()).is_ok());
                assert_eq!(a.model, DEFAULT_MODEL);
            }
            other => panic!("expected success, got {other:?}"),
        }

        // Requests the adapter actually issued.
        let reqs = t.requests.lock().unwrap();
        assert_eq!(reqs[0].url, "https://api.example.test/images/generations");
        assert_eq!(reqs[0].method, crate::transport::Method::Post);
        let body = String::from_utf8(reqs[0].body.clone().unwrap()).unwrap();
        assert!(body.contains("cogview-3-flash"));
        assert!(body.contains("雨夜霓虹的天空"));
        // Signed CDN link: fetched without an Authorization header.
        assert_eq!(reqs[1].url, "https://cdn.example.test/sky.png");
        assert!(reqs[1].headers.is_empty());
    }

    #[test]
    fn b64_path_succeeds_on_the_first_poll() {
        let b64 = "iVBORw0KGgo="; // the 8-byte PNG signature
        let t = fixture_transport(vec![HttpResponse::ok(format!(
            r#"{{"created":0,"data":[{{"b64_json":"{b64}"}}]}}"#
        ))]);
        let p = provider(t);
        let id = p
            .request_generation(&GenerationRequest::text_to_image("ground", "草地"))
            .unwrap();
        match p.poll_task(&id).unwrap() {
            TaskStatus::Succeeded(a) => {
                assert_eq!(
                    a.payload,
                    vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
                );
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn http_error_is_a_provider_error() {
        let t = fixture_transport(vec![HttpResponse {
            status: 429,
            body: b"{}".to_vec(),
        }]);
        let p = provider(t);
        let id = p
            .request_generation(&GenerationRequest::text_to_image("x", "y"))
            .unwrap();
        assert!(matches!(p.poll_task(&id), Err(GenAiError::Provider(_))));
    }

    #[test]
    fn reply_without_results_is_a_provider_error() {
        let t = fixture_transport(vec![HttpResponse::ok(r#"{"created":0,"data":[]}"#)]);
        let p = provider(t);
        let id = p
            .request_generation(&GenerationRequest::text_to_image("x", "y"))
            .unwrap();
        assert!(matches!(p.poll_task(&id), Err(GenAiError::Provider(_))));
    }

    #[test]
    fn non_image_kinds_are_unsupported() {
        let t = fixture_transport(vec![]);
        let p = provider(t);
        assert!(matches!(
            p.request_generation(&GenerationRequest::text_to_3d("x", "y")),
            Err(GenAiError::Unsupported(_))
        ));
        assert!(!p.capabilities().text_to_3d);
    }

    #[test]
    fn missing_key_fails_fast() {
        let t = fixture_transport(vec![]);
        let p = CogViewProvider::new(
            crate::ProviderConfig {
                kind: crate::ProviderKind::CogView,
                api_key: String::new(),
                ..crate::ProviderConfig::default()
            },
            t,
        );
        assert!(matches!(
            p.request_generation(&GenerationRequest::text_to_image("x", "y")),
            Err(GenAiError::Provider(_))
        ));
    }

    #[test]
    fn foreign_task_ids_are_rejected() {
        let t = fixture_transport(vec![]);
        let p = provider(t);
        assert!(matches!(
            p.poll_task(&TaskId("tripo:t_1".into())),
            Err(GenAiError::Provider(_))
        ));
    }

    #[test]
    fn base64_decoder_vectors() {
        assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_base64("QQ==").unwrap(), vec![0x41]);
        assert_eq!(decode_base64("QUI=").unwrap(), vec![0x41, 0x42]);
        assert_eq!(decode_base64("QUJD").unwrap(), vec![0x41, 0x42, 0x43]);
        // Whitespace tolerated; the canonical PNG signature round-trips.
        assert_eq!(
            decode_base64("iVBORw0KGgo=\n").unwrap(),
            vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
        assert!(decode_base64("!!!!").is_err());
        assert!(decode_base64("QQ=").is_err()); // bad length
        assert!(decode_base64("=QQ=").is_err()); // misplaced padding
    }
}
