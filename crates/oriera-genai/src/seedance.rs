//! Seedance adapter (ByteDance video models via a Volcano-Ark-compatible
//! tasks API) — protocol only, transport injected (ADR 0003).
//!
//! Wire shape (`POST {base}/api/v3/contents/generations/tasks`, then
//! poll `GET {base}/api/v3/contents/generations/tasks/{id}`):
//!
//! ```json
//! {"model":"doubao-seedance-2-0-260128","content":[{"type":"text","text":"..."}],
//!  "ratio":"16:9","duration":5,"watermark":false}
//! {"id":"cgt-..."}
//! {"status":"queued|running|succeeded|failed|cancelled",
//!  "content":{"video_url":"https://...mp4"},"usage":{"completion_tokens":108900}}
//! ```
//!
//! Field-tested 2026-10-24 against the live Ark endpoint (R0 calibration
//! run, interfilm/docs/R0-report.md):
//! * `watermark` MUST be sent explicitly — the vendor default is `true`.
//! * A succeeded poll carries `usage.completion_tokens` (~108.9k tokens
//!   for 5 s @720p on the mini tier, ~130.5k on the 2.5 tier); metering
//!   hookup is future work (L0 TokenUsage), tolerated by the parser today.
//! * Image-to-video exists on the wire as extra `content` entries with
//!   `role` `"reference_image"` (2.x tiers) or `"first_frame"` (mini
//!   tier); the v0.1 engine `GenerationRequest` has no image field yet,
//!   so `image_to_video` stays `false` until the contract grows one.
//! * `generate_audio: true` (2.5 tier) returns an AAC track inside the
//!   mp4; also out of scope for the v0.1 request surface.
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

/// Vendor default when `ProviderConfig::model` is empty — a real,
/// on-sale model id as listed by `GET /api/v3/models` (2026-10-24).
pub const DEFAULT_MODEL: &str = "doubao-seedance-2-0-260128";

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
            text_to_image: false,
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
        // 签名 CDN URL 不带 Authorization（评审 R2 安全 P1+性能 P1 双命中）：
        // 裸 Bearer 打向供应商响应里的任意 host = Key 外送通道，且多数
        // 签名 URL 会因多余头直接 400 → 永久轮询。对齐 rodin/cogview 的
        // get_public 惯例。
        let resp = self
            .transport
            .send(&HttpRequest::get_public(url))
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

/// Request body for a text-to-video task. `watermark` is sent explicitly
/// because the vendor default stamps one (R0 field test, 2026-10-24);
/// `ratio`/`duration` pin v0.2 defaults until the request contract grows
/// per-call knobs.
pub fn build_create_task_body(model: &str, req: &GenerationRequest) -> String {
    json!({
        "model": model,
        "content": [ { "type": "text", "text": req.prompt } ],
        "ratio": "16:9",
        "duration": 5,
        "watermark": false
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
    use crate::testing::{fixture_transport, ScriptedTransport};
    use crate::transport::HttpResponse;
    use std::sync::Arc;

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
        assert!(body.contains("\"model\":\"doubao-seedance-2-0-260128\""));
        assert!(body.contains("雨夜咖啡馆"));
        // R0 field test: watermark must be explicitly off (vendor default is on),
        // and the v0.2 defaults pin ratio/duration.
        assert!(body.contains("\"watermark\":false"));
        assert!(body.contains("\"ratio\":\"16:9\""));
        assert!(body.contains("\"duration\":5"));
        assert_eq!(
            reqs[1].url,
            "https://ark.example.test/api/v3/contents/generations/tasks/cgt-42"
        );
        assert_eq!(reqs[2].url, "https://v.example/rain.mp4");
    }

    #[test]
    fn r0_shaped_succeeded_poll_is_tolerated() {
        // Real poll body from the 2026-10-24 R0 calibration run: status +
        // video_url + usage block (metering not wired yet — must not break).
        let v: Value = serde_json::from_str(
            r#"{"id":"cgt-20261024121745-4wsgp","status":"succeeded",
                "content":{"video_url":"https://ark.tos/rain.mp4"},
                "usage":{"completion_tokens":108900,"total_tokens":108900}}"#,
        )
        .unwrap();
        match parse_status(&v) {
            Ok(PollOutcome::Done(url)) => assert_eq!(url, "https://ark.tos/rain.mp4"),
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[test]
    fn cancelled_maps_to_failed() {
        let v: Value = serde_json::from_str(r#"{"status":"cancelled"}"#).unwrap();
        match parse_status(&v) {
            Ok(PollOutcome::Failed(m)) => assert!(m.contains("cancelled")),
            other => panic!("expected failed, got {other:?}"),
        }
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
    fn video_download_carries_no_authorization() {
        // 评审 R2 安全 P1：签名 CDN URL 带裸 Bearer = Key 外送通道（且常
        // 直接 400 → InFlight 永久轮询）。钉死 Done 分支的取件 GET 为
        // get_public（零鉴权头），与 rodin/cogview 惯例一致。
        let t = fixture_transport(vec![
            HttpResponse::ok(b"fake-mp4".to_vec()),
            HttpResponse::ok(
                r#"{"id":"c1","status":"succeeded","content":{"video_url":"https://tos.example/v.mp4"}}"#,
            ),
            HttpResponse::ok(r#"{"id":"c1","status":"running"}"#),
            HttpResponse::ok(r#"{"id":"c1"}"#),
        ]);
        let p = provider(t.clone());
        let id = p
            .request_generation(&GenerationRequest::text_to_video("v", "雨夜站台"))
            .unwrap();
        assert!(matches!(p.poll_task(&id).unwrap(), TaskStatus::Running));
        match p.poll_task(&id).unwrap() {
            TaskStatus::Succeeded(a) => assert_eq!(a.format, AssetFormat::Video),
            other => panic!("expected asset, got {other:?}"),
        }
        let reqs = t.requests.lock().unwrap();
        let last = reqs.last().unwrap();
        assert_eq!(last.url, "https://tos.example/v.mp4");
        assert!(
            last.headers.is_empty(),
            "CDN GET must carry no Authorization: {:?}",
            last.headers
        );
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
