//! Seedance adapter (ByteDance video models via a Volcano-Ark-compatible
//! tasks API) — protocol only, transport injected (ADR 0003).
//!
//! Wire shape (`POST {base}/api/v3/contents/generations/tasks`, then
//! poll `GET {base}/api/v3/contents/generations/tasks/{id}`):
//!
//! ```json
//! {"model":"doubao-seedance-2-0","content":[{"type":"text","text":"..."}]}
//! {"id":"cgt-..."}
//! {"status":"queued|running|succeeded|failed","content":{"video_url":"https://...mp4"}}
//! ```
//!
//! Positioning (ADR 0003): Seedance is a *video* model. In this layer
//! it produces `AssetFormat::Video` payloads (cutscenes, B-roll,
//! future image-to-3D source material) — not 3D meshes. The BytePlus
//! international variant only differs in `base_url`.

use crate::transport::{HttpRequest, SharedTransport};
use crate::{
    AssetFormat, GenAiError, GeneratedAsset, GenerationProvider, GenerationRequest,
    ProviderCapabilities, TaskId, TaskStatus,
};
use serde_json::{json, Value};

const ID_PREFIX: &str = "seedance:";

/// Vendor default when `ProviderConfig::model` is empty.
pub const DEFAULT_MODEL: &str = "doubao-seedance-2-0";

pub struct SeedanceProvider {
    config: crate::ProviderConfig,
    transport: SharedTransport,
}

impl SeedanceProvider {
    pub fn new(config: crate::ProviderConfig, transport: SharedTransport) -> Self {
        Self { config, transport }
    }

    fn create_url(&self) -> String {
        format!("{}/api/v3/contents/generations/tasks", self.config.base())
    }

    fn poll_url(&self, remote_id: &str) -> String {
        format!(
            "{}/api/v3/contents/generations/tasks/{remote_id}",
            self.config.base()
        )
    }

    fn model(&self) -> &str {
        if self.config.model.is_empty() {
            DEFAULT_MODEL
        } else {
            &self.config.model
        }
    }
}

impl GenerationProvider for SeedanceProvider {
    fn id(&self) -> &str {
        "seedance-ark-v3"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            text_to_3d: false,
            image_to_3d: false,
            text_to_video: true,
            image_to_video: false, // v0.1 request surface has no image field
        }
    }

    fn request_generation(&self, req: &GenerationRequest) -> Result<TaskId, GenAiError> {
        if !self.capabilities().supports(req.kind) {
            return Err(GenAiError::Unsupported(format!(
                "seedance adapter serves text_to_video only (v0.1); {} requested",
                req.kind.id()
            )));
        }
        if self.config.api_key.is_empty() {
            return Err(GenAiError::Provider(
                "seedance selected without ORIERA_GENAI_API_KEY".into(),
            ));
        }
        let body = build_create_task_body(self.model(), req);
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
                "seedance create returned HTTP {}",
                resp.status
            )));
        }
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| GenAiError::Provider(format!("seedance create body: {e}")))?;
        parse_create(&v).map(|remote| TaskId(format!("{ID_PREFIX}{remote}")))
    }

    fn poll_task(&self, task: &TaskId) -> Result<TaskStatus, GenAiError> {
        let Some(remote) = task.0.strip_prefix(ID_PREFIX) else {
            return Err(GenAiError::Provider(format!(
                "not a seedance task: {}",
                task.0
            )));
        };
        let resp = self
            .transport
            .send(&HttpRequest::get(
                self.poll_url(remote),
                &self.config.api_key,
            ))
            .map_err(GenAiError::Provider)?;
        if !resp.is_success() {
            return Err(GenAiError::Provider(format!(
                "seedance poll returned HTTP {}",
                resp.status
            )));
        }
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| GenAiError::Provider(format!("seedance poll body: {e}")))?;
        match parse_status(&v)? {
            PollOutcome::Done(url) => {
                let bytes = self.fetch(&url)?;
                Ok(TaskStatus::Succeeded(GeneratedAsset {
                    format: AssetFormat::Video,
                    payload: bytes,
                    provider_id: self.id().into(),
                    model: self.model().to_string(),
                }))
            }
            PollOutcome::Failed(m) => Ok(TaskStatus::Failed(m)),
            PollOutcome::Pending => Ok(TaskStatus::Running),
        }
    }
}

impl SeedanceProvider {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, GenAiError> {
        let resp = self
            .transport
            .send(&HttpRequest::get(url, &self.config.api_key))
            .map_err(GenAiError::Provider)?;
        if !resp.is_success() {
            return Err(GenAiError::Provider(format!(
                "seedance video download returned HTTP {}",
                resp.status
            )));
        }
        Ok(resp.body)
    }
}

/// Request body for a text-to-video task.
pub fn build_create_task_body(model: &str, req: &GenerationRequest) -> String {
    json!({
        "model": model,
        "content": [ { "type": "text", "text": req.prompt } ]
    })
    .to_string()
}

/// `{"id":"cgt-..."}` → remote id.
pub fn parse_create(v: &Value) -> Result<String, GenAiError> {
    v.get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            GenAiError::Provider(format!(
                "seedance create failed: {}",
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("missing task id")
            ))
        })
}

#[derive(Debug)]
pub(crate) enum PollOutcome {
    Pending,
    Done(String),
    Failed(String),
}

/// Map the documented status strings onto the engine lifecycle.
pub(crate) fn parse_status(v: &Value) -> Result<PollOutcome, GenAiError> {
    let status = v
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| GenAiError::Provider("seedance poll missing status".into()))?;
    match status {
        "queued" | "running" => Ok(PollOutcome::Pending),
        "succeeded" => {
            let url = v
                .pointer("/content/video_url")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    GenAiError::Provider("seedance succeeded without video_url".into())
                })?;
            Ok(PollOutcome::Done(url.to_string()))
        }
        other => {
            let detail = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("no detail");
            Ok(PollOutcome::Failed(format!(
                "vendor status: {other} ({detail})"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{HttpResponse, Transport};
    use std::sync::{Arc, Mutex};

    /// Scripted transport: pops queued responses per request.
    struct ScriptedTransport {
        requests: Mutex<Vec<HttpRequest>>,
        responses: Mutex<Vec<HttpResponse>>,
    }

    impl Transport for ScriptedTransport {
        fn send(&self, req: &HttpRequest) -> Result<HttpResponse, String> {
            self.requests.lock().unwrap().push(req.clone());
            self.responses
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| "no scripted response".to_string())
        }
    }

    fn fixture_transport(responses: Vec<HttpResponse>) -> Arc<ScriptedTransport> {
        Arc::new(ScriptedTransport {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses),
        })
    }

    fn provider(t: Arc<ScriptedTransport>) -> SeedanceProvider {
        SeedanceProvider::new(
            crate::ProviderConfig {
                kind: crate::ProviderKind::Seedance,
                model: String::new(), // exercise the vendor default
                api_key: "test-key".into(),
                base_url: Some("https://ark.example.test".into()),
            },
            t,
        )
    }

    #[test]
    fn create_then_poll_then_download() {
        // Pop order reversed: video download, poll succeeded, create.
        let t = fixture_transport(vec![
            HttpResponse::ok(crate::tiny_mp4_stub()),
            HttpResponse::ok(
                r#"{"id":"cgt-42","status":"succeeded","content":{"video_url":"https://v.example/rain.mp4"}}"#,
            ),
            HttpResponse::ok(r#"{"id":"cgt-42"}"#),
        ]);
        let p = provider(t.clone());

        let id = p
            .request_generation(&GenerationRequest::text_to_video(
                "intro",
                "雨夜咖啡馆的门帘",
            ))
            .unwrap();
        assert_eq!(id.0, "seedance:cgt-42");

        match p.poll_task(&id).unwrap() {
            TaskStatus::Succeeded(a) => {
                assert_eq!(a.format, AssetFormat::Video);
                assert!(crate::validate_asset(&a, &Default::default()).is_ok());
                assert_eq!(a.model, DEFAULT_MODEL);
            }
            other => panic!("expected success, got {other:?}"),
        }

        let reqs = t.requests.lock().unwrap();
        assert_eq!(
            reqs[0].url,
            "https://ark.example.test/api/v3/contents/generations/tasks"
        );
        let body = String::from_utf8(reqs[0].body.clone().unwrap()).unwrap();
        assert!(body.contains("\"model\":\"doubao-seedance-2-0\""));
        assert!(body.contains("雨夜咖啡馆"));
        assert_eq!(
            reqs[1].url,
            "https://ark.example.test/api/v3/contents/generations/tasks/cgt-42"
        );
        assert_eq!(reqs[2].url, "https://v.example/rain.mp4");
    }

    #[test]
    fn model_override_flows_into_the_request() {
        assert!(build_create_task_body(
            "doubao-seedance-2-5",
            &GenerationRequest::text_to_video("v", "x")
        )
        .contains("seedance-2-5"));
    }

    #[test]
    fn pending_and_failed_statuses_map_correctly() {
        assert!(matches!(
            parse_status(&serde_json::from_str(r#"{"status":"running"}"#).unwrap()),
            Ok(PollOutcome::Pending)
        ));
        let failed: Value =
            serde_json::from_str(r#"{"status":"failed","error":{"message":"content policy"}}"#)
                .unwrap();
        match parse_status(&failed) {
            Ok(PollOutcome::Failed(m)) => assert!(m.contains("content policy")),
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn non_video_kinds_are_unsupported() {
        let t = fixture_transport(vec![]);
        let p = provider(t);
        assert!(matches!(
            p.request_generation(&GenerationRequest::text_to_3d("x", "y")),
            Err(GenAiError::Unsupported(_))
        ));
    }
}
