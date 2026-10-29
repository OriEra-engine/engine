//! Tripo adapter (text→GLB) — protocol only, transport injected
//! (ADR 0003). Single-file by design: when V2 retires (announced
//! 2026-11-01), the endpoint/version migration is one diff here.
//!
//! Wire shape (V2, `POST {base}/v2/openapi/task` + poll
//! `GET {base}/v2/openapi/task/{id}`):
//!
//! ```json
//! {"code":0,"data":{"task_id":"..."}}
//! {"code":0,"data":{"status":"queued|running|success|failed",
//!                    "output":{"pbr_model":"https://...glb"}}}
//! ```
//!
//! `image_to_model` needs image input the v0.1 request surface does
//! not carry yet — the adapter reports it unsupported until the
//! request grows an image field (v0.2).

use crate::transport::{HttpRequest, SharedTransport};
use crate::{
    AssetFormat, GenAiError, GeneratedAsset, GenerationProvider, GenerationRequest,
    ProviderCapabilities, TaskId, TaskStatus,
};
use serde_json::{json, Value};

/// Prefix stamped on remote ids so polls are self-describing.
const ID_PREFIX: &str = "tripo:";

pub struct TripoProvider {
    config: crate::ProviderConfig,
    transport: SharedTransport,
}

impl TripoProvider {
    pub fn new(config: crate::ProviderConfig, transport: SharedTransport) -> Self {
        Self { config, transport }
    }

    fn create_url(&self) -> String {
        format!("{}/v2/openapi/task", self.config.base())
    }

    fn poll_url(&self, remote_id: &str) -> String {
        format!("{}/v2/openapi/task/{remote_id}", self.config.base())
    }
}

impl GenerationProvider for TripoProvider {
    fn id(&self) -> &str {
        "tripo-v2"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            text_to_3d: true,
            image_to_3d: false, // v0.1 request surface has no image field
            text_to_image: false,
            text_to_video: false,
            image_to_video: false,
            text_to_speech: false,
        }
    }

    fn request_generation(&self, req: &GenerationRequest) -> Result<TaskId, GenAiError> {
        if !self.capabilities().supports(req.kind) {
            return Err(GenAiError::Unsupported(format!(
                "tripo adapter serves text_to_3d only (v0.1); {} requested",
                req.kind.id()
            )));
        }
        if self.config.api_key.is_empty() {
            return Err(GenAiError::Provider(
                "tripo selected without ORIERA_GENAI_API_KEY".into(),
            ));
        }
        let body = build_create_task_body(req);
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
                "tripo create returned HTTP {}",
                resp.status
            )));
        }
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| GenAiError::Provider(format!("tripo create body: {e}")))?;
        parse_create(&v).map(|remote| TaskId(format!("{ID_PREFIX}{remote}")))
    }

    fn poll_task(&self, task: &TaskId) -> Result<TaskStatus, GenAiError> {
        let Some(remote) = task.0.strip_prefix(ID_PREFIX) else {
            return Err(GenAiError::Provider(format!(
                "not a tripo task: {}",
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
                "tripo poll returned HTTP {}",
                resp.status
            )));
        }
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| GenAiError::Provider(format!("tripo poll body: {e}")))?;
        match parse_status(&v)? {
            PollOutcome::Done(url) => {
                let bytes = self.fetch(&url)?;
                Ok(TaskStatus::Succeeded(GeneratedAsset {
                    format: AssetFormat::Glb,
                    payload: bytes,
                    provider_id: self.id().into(),
                    model: self.config.model.clone(),
                    usage: None,
                }))
            }
            PollOutcome::Failed(m) => Ok(TaskStatus::Failed(m)),
            PollOutcome::Pending => Ok(TaskStatus::Running),
        }
    }
}

impl TripoProvider {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, GenAiError> {
        // 资产取件走 get_public:厂商返回的签名 CDN 链接带裸 Bearer =
        // Key 外送通道,且常直接 400 → InFlight 永久轮询(与
        // seedance/rodin/cogview 同一条评审铁律)。
        let resp = self
            .transport
            .send(&HttpRequest::get_public(url))
            .map_err(GenAiError::Provider)?;
        if !resp.is_success() {
            return Err(GenAiError::Provider(format!(
                "tripo asset download returned HTTP {}",
                resp.status
            )));
        }
        Ok(resp.body)
    }
}

/// Request body for a text_to_model task.
pub fn build_create_task_body(req: &GenerationRequest) -> String {
    json!({ "type": "text_to_model", "prompt": req.prompt }).to_string()
}

/// `{"code":0,"data":{"task_id":...}}` → remote id. Non-zero `code`
/// (or a missing id) is an error.
pub fn parse_create(v: &Value) -> Result<String, GenAiError> {
    if v.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(GenAiError::Provider(format!(
            "tripo create failed: {}",
            v.get("message").and_then(Value::as_str).unwrap_or("?")
        )));
    }
    v.pointer("/data/task_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| GenAiError::Provider("tripo create missing task_id".into()))
}

#[derive(Debug)]
pub(crate) enum PollOutcome {
    Pending,
    Done(String),
    Failed(String),
}

/// Map the documented status strings onto the engine lifecycle.
pub(crate) fn parse_status(v: &Value) -> Result<PollOutcome, GenAiError> {
    if v.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(GenAiError::Provider(format!(
            "tripo poll failed: {}",
            v.get("message").and_then(Value::as_str).unwrap_or("?")
        )));
    }
    let status = v
        .pointer("/data/status")
        .and_then(Value::as_str)
        .ok_or_else(|| GenAiError::Provider("tripo poll missing status".into()))?;
    match status {
        "queued" => Ok(PollOutcome::Pending),
        "running" => Ok(PollOutcome::Pending),
        "success" => {
            let url = v
                .pointer("/data/output/pbr_model")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    GenAiError::Provider("tripo success without pbr_model url".into())
                })?;
            Ok(PollOutcome::Done(url.to_string()))
        }
        other => Ok(PollOutcome::Failed(format!("vendor status: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{fixture_transport, ScriptedTransport};
    use crate::transport::HttpResponse;
    use std::sync::Arc;

    fn provider(t: Arc<ScriptedTransport>) -> TripoProvider {
        TripoProvider::new(
            crate::ProviderConfig {
                kind: crate::ProviderKind::Tripo,
                model: "tripo-v2.5".into(),
                api_key: "test-key".into(),
                base_url: Some("https://api.example.test".into()),
            },
            t,
        )
    }

    fn glb_response() -> HttpResponse {
        HttpResponse::ok(crate::tiny_glb())
    }

    #[test]
    fn create_then_poll_then_download() {
        // Pop order is reversed: asset download, poll success, create.
        let t = fixture_transport(vec![
            glb_response(),
            HttpResponse::ok(
                r#"{"code":0,"data":{"status":"success","output":{"pbr_model":"https://cdn.example/guitar.glb"}}}"#,
            ),
            HttpResponse::ok(r#"{"code":0,"data":{"task_id":"t_001"}}"#),
        ]);
        let p = provider(t.clone());

        let id = p
            .request_generation(&GenerationRequest::text_to_3d("npc_prop", "木吉他"))
            .unwrap();
        assert_eq!(id.0, "tripo:t_001");

        match p.poll_task(&id).unwrap() {
            TaskStatus::Succeeded(a) => {
                assert_eq!(a.format, AssetFormat::Glb);
                assert!(crate::validate_asset(&a, &Default::default()).is_ok());
                assert_eq!(a.model, "tripo-v2.5");
            }
            other => panic!("expected success, got {other:?}"),
        }

        // Requests the adapter actually issued.
        let reqs = t.requests.lock().unwrap();
        assert_eq!(reqs[0].url, "https://api.example.test/v2/openapi/task");
        assert_eq!(reqs[0].method, crate::transport::Method::Post);
        let body = String::from_utf8(reqs[0].body.clone().unwrap()).unwrap();
        assert!(body.contains("\"type\":\"text_to_model\""));
        assert!(body.contains("木吉他"));
        assert_eq!(
            reqs[1].url,
            "https://api.example.test/v2/openapi/task/t_001"
        );
        assert_eq!(reqs[2].url, "https://cdn.example/guitar.glb");
        // 签名 CDN 链接不带任何鉴权头(评审铁律,与 seedance 钉子同款)
        assert!(
            reqs[2].headers.is_empty(),
            "asset download must not leak Authorization to vendor CDN: {:?}",
            reqs[2].headers
        );
    }

    #[test]
    fn pending_statuses_map_to_running() {
        assert!(matches!(
            parse_status(
                &serde_json::from_str(r#"{"code":0,"data":{"status":"queued"}}"#).unwrap()
            ),
            Ok(PollOutcome::Pending)
        ));
        assert!(matches!(
            parse_status(
                &serde_json::from_str(r#"{"code":0,"data":{"status":"running"}}"#).unwrap()
            ),
            Ok(PollOutcome::Pending)
        ));
    }

    #[test]
    fn vendor_failure_maps_to_failed_status() {
        let v: Value = serde_json::from_str(r#"{"code":0,"data":{"status":"failed"}}"#).unwrap();
        assert!(matches!(parse_status(&v), Ok(PollOutcome::Failed(_))));
    }

    #[test]
    fn nonzero_code_is_an_error() {
        let v: Value = serde_json::from_str(r#"{"code":1001,"message":"quota exceeded"}"#).unwrap();
        assert!(parse_create(&v).is_err());
        assert!(parse_status(&v).is_err());
    }

    #[test]
    fn non_3d_kinds_are_unsupported() {
        let t = fixture_transport(vec![]);
        let p = provider(t);
        assert!(matches!(
            p.request_generation(&GenerationRequest::text_to_video("v", "clip")),
            Err(GenAiError::Unsupported(_))
        ));
        assert!(!p.capabilities().image_to_3d);
    }

    #[test]
    fn foreign_task_ids_are_rejected() {
        let t = fixture_transport(vec![]);
        let p = provider(t);
        assert!(matches!(
            p.poll_task(&TaskId("seedance:cgt-1".into())),
            Err(GenAiError::Provider(_))
        ));
    }

    #[test]
    fn missing_key_fails_fast() {
        let t = fixture_transport(vec![]);
        let p = TripoProvider::new(
            crate::ProviderConfig {
                kind: crate::ProviderKind::Tripo,
                api_key: String::new(),
                ..crate::ProviderConfig::default()
            },
            t,
        );
        assert!(matches!(
            p.request_generation(&GenerationRequest::text_to_3d("x", "y")),
            Err(GenAiError::Provider(_))
        ));
    }
}
