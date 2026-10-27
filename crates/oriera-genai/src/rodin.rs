//! Hyper3D Rodin adapter (text→GLB) — protocol only, transport
//! injected (ADR 0003), the second 3D vendor beside
//! [`crate::tripo`]（双供应商冗余，2026-10-23 用户拍板「先接 Hyper3D」）.
//!
//! Wire verified against docs.hyper3d.ai (2026-10-23, Rodin v2):
//!
//! ```text
//! POST {base}/rodin      multipart/form-data, Bearer
//!   → 201 {"uuid":"…","jobs":{"uuids":[…],"subscription_key":"…"}}
//! POST {base}/status     JSON {"subscription_key"}
//!   → {"jobs":[{"uuid":"…","status":"Waiting|Generating|Done|Failed"}]}
//! POST {base}/download   JSON {"task_uuid": <top-level uuid>}
//!   → {"list":[{"name":"model.glb","url":"https://…"}]}
//! ```
//!
//! Deliberate wiring details:
//! - `status` is keyed by `jobs.subscription_key`, `download` by the
//!   TOP-LEVEL `uuid` — they are different identifiers, so the
//!   [`TaskId`] carries both (`rodin:{uuid}|{subkey}`).
//! - The download URL is vendor-signed and **expiring** — we fetch
//!   immediately inside the poll that observed `Done`, never persist it.
//! - CDN GET goes out without the `Authorization` header
//!   ([`HttpRequest::get_public`]); a stray bearer can 400 a signed URL.
//! - `tier`/`quality` ship conservative defaults (`Low`/`medium`); the
//!   request surface grows fields when a shell needs the dials.

use crate::transport::{HttpRequest, SharedTransport};
use crate::{
    AssetFormat, GenAiError, GeneratedAsset, GenerationProvider, GenerationRequest,
    ProviderCapabilities, TaskId, TaskStatus,
};
use serde_json::{json, Value};

/// Prefix stamped on remote ids; `uuid|subscription_key` after it.
const ID_PREFIX: &str = "rodin:";

/// Multipart boundary for the create call (no binary parts yet, so a
/// fixed string cannot collide with form content we control).
const BOUNDARY: &str = "oriera-rodin-f3a9";

pub struct RodinProvider {
    config: crate::ProviderConfig,
    transport: SharedTransport,
}

impl RodinProvider {
    pub fn new(config: crate::ProviderConfig, transport: SharedTransport) -> Self {
        Self { config, transport }
    }

    fn create_url(&self) -> String {
        format!("{}/rodin", self.config.base())
    }

    fn status_url(&self) -> String {
        format!("{}/status", self.config.base())
    }

    fn download_url(&self) -> String {
        format!("{}/download", self.config.base())
    }

    fn post_json(
        &self,
        url: String,
        body: String,
    ) -> Result<crate::transport::HttpResponse, GenAiError> {
        self.transport
            .send(&HttpRequest::post_json(url, &self.config.api_key, body))
            .map_err(GenAiError::Provider)
    }

    fn fetch_public(&self, url: &str) -> Result<Vec<u8>, GenAiError> {
        let resp = self
            .transport
            .send(&HttpRequest::get_public(url))
            .map_err(GenAiError::Provider)?;
        if !resp.is_success() {
            return Err(GenAiError::Provider(format!(
                "rodin asset download returned HTTP {}",
                resp.status
            )));
        }
        Ok(resp.body)
    }
}

/// Multipart form for the text→3D create call. Fields per the v2 spec:
/// `prompt` (required without images), `geometry_file_format=glb`,
/// conservative `tier=Low` / `quality=medium` defaults.
pub fn build_create_form(prompt: &str) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    for (name, value) in [
        ("prompt", prompt),
        ("geometry_file_format", "glb"),
        ("tier", "Low"),
        ("quality", "medium"),
    ] {
        body.extend_from_slice(
            format!("--{BOUNDARY}\r\ncontent-disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    (BOUNDARY.to_string(), body)
}

/// `201 {"uuid":…,"jobs":{"subscription_key":…}}` → `(uuid, subkey)`.
/// 201 alone is not acceptance: `error` must be absent and `uuid`
/// non-empty (per the spec's own definition of "submitted").
pub fn parse_create(v: &Value) -> Result<(String, String), GenAiError> {
    let uuid = v
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            GenAiError::Provider(format!(
                "rodin create not accepted: {}",
                v.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("no uuid")
            ))
        })?
        .to_string();
    let subkey = v
        .pointer("/jobs/subscription_key")
        .and_then(Value::as_str)
        .ok_or_else(|| GenAiError::Provider("rodin create missing jobs.subscription_key".into()))?
        .to_string();
    Ok((uuid, subkey))
}

/// Fold the per-job status list onto the engine lifecycle: any `Failed`
/// kills the task (spec: a failed job never produces output); `Done`
/// only counts when EVERY job is done.
pub(crate) fn parse_status(v: &Value) -> Result<TaskStatusFold, GenAiError> {
    let jobs = v
        .get("jobs")
        .and_then(Value::as_array)
        .ok_or_else(|| GenAiError::Provider("rodin status missing jobs".into()))?;
    if jobs.is_empty() {
        return Err(GenAiError::Provider("rodin status empty jobs".into()));
    }
    let mut all_done = true;
    for j in jobs {
        match j.get("status").and_then(Value::as_str) {
            Some("Done") => {}
            Some("Waiting") | Some("Generating") => all_done = false,
            Some(other) => {
                return Ok(TaskStatusFold::Failed(format!("rodin job status: {other}")));
            }
            None => {
                return Err(GenAiError::Provider(
                    "rodin status job missing status".into(),
                ));
            }
        }
    }
    Ok(if all_done {
        TaskStatusFold::Done
    } else {
        TaskStatusFold::Pending
    })
}

#[derive(Debug)]
pub(crate) enum TaskStatusFold {
    Pending,
    Done,
    Failed(String),
}

/// `{"list":[{"name","url"}]}` → the `.glb` entry's URL.
pub fn parse_download(v: &Value) -> Result<String, GenAiError> {
    v.get("list")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter().find_map(|e| {
                let name = e.get("name").and_then(Value::as_str)?;
                if name.to_ascii_lowercase().ends_with(".glb") {
                    e.get("url").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| GenAiError::Provider("rodin download has no .glb entry".into()))
}

impl GenerationProvider for RodinProvider {
    fn id(&self) -> &str {
        "rodin-v2"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            text_to_3d: true,
            image_to_3d: false, // v0.1 form has no image part yet (v0.2)
            text_to_image: false,
            text_to_video: false,
            image_to_video: false,
            text_to_speech: false,
        }
    }

    fn request_generation(&self, req: &GenerationRequest) -> Result<TaskId, GenAiError> {
        if !self.capabilities().supports(req.kind) {
            return Err(GenAiError::Unsupported(format!(
                "rodin adapter serves text_to_3d only (v0.1); {} requested",
                req.kind.id()
            )));
        }
        if self.config.api_key.is_empty() {
            return Err(GenAiError::Provider(
                "rodin selected without ORIERA_GENAI_API_KEY".into(),
            ));
        }
        let (boundary, body) = build_create_form(&req.prompt);
        let resp = self
            .transport
            .send(&HttpRequest {
                method: crate::transport::Method::Post,
                url: self.create_url(),
                headers: vec![
                    (
                        "Authorization".into(),
                        format!("Bearer {}", self.config.api_key),
                    ),
                    (
                        "Content-Type".into(),
                        format!("multipart/form-data; boundary={boundary}"),
                    ),
                ],
                body: Some(body),
            })
            .map_err(GenAiError::Provider)?;
        if !resp.is_success() {
            return Err(GenAiError::Provider(format!(
                "rodin create returned HTTP {}",
                resp.status
            )));
        }
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| GenAiError::Provider(format!("rodin create body: {e}")))?;
        let (uuid, subkey) = parse_create(&v)?;
        Ok(TaskId(format!("{ID_PREFIX}{uuid}|{subkey}")))
    }

    fn poll_task(&self, task: &TaskId) -> Result<TaskStatus, GenAiError> {
        let Some(remote) = task.0.strip_prefix(ID_PREFIX) else {
            return Err(GenAiError::Provider(format!(
                "not a rodin task: {}",
                task.0
            )));
        };
        let (uuid, subkey) = remote
            .split_once('|')
            .ok_or_else(|| GenAiError::Provider("rodin task id missing subkey".into()))?;

        let resp = self.post_json(
            self.status_url(),
            json!({ "subscription_key": subkey }).to_string(),
        )?;
        if !resp.is_success() {
            return Err(GenAiError::Provider(format!(
                "rodin status returned HTTP {}",
                resp.status
            )));
        }
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| GenAiError::Provider(format!("rodin status body: {e}")))?;
        match parse_status(&v)? {
            TaskStatusFold::Pending => Ok(TaskStatus::Running),
            TaskStatusFold::Failed(m) => Ok(TaskStatus::Failed(m)),
            TaskStatusFold::Done => {
                let resp = self.post_json(
                    self.download_url(),
                    json!({ "task_uuid": uuid }).to_string(),
                )?;
                if !resp.is_success() {
                    return Err(GenAiError::Provider(format!(
                        "rodin download returned HTTP {}",
                        resp.status
                    )));
                }
                let v: Value = serde_json::from_slice(&resp.body)
                    .map_err(|e| GenAiError::Provider(format!("rodin download body: {e}")))?;
                let url = parse_download(&v)?;
                let bytes = self.fetch_public(&url)?;
                Ok(TaskStatus::Succeeded(GeneratedAsset {
                    format: AssetFormat::Glb,
                    payload: bytes,
                    provider_id: self.id().into(),
                    model: self.config.model.clone(),
                    usage: None,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{fixture_transport, ScriptedTransport};
    use crate::transport::HttpResponse;
    use std::sync::Arc;

    fn provider(t: Arc<ScriptedTransport>) -> RodinProvider {
        RodinProvider::new(
            crate::ProviderConfig {
                kind: crate::ProviderKind::Rodin,
                model: "rodin-gen-2".into(),
                api_key: "test-key".into(),
                base_url: Some("https://api.example.test/api/v2".into()),
            },
            t,
        )
    }

    #[test]
    fn create_then_status_then_download_serves_glb() {
        // Pop order reversed: asset GET (public), download, status Done,
        // status Generating, create.
        let t = fixture_transport(vec![
            HttpResponse::ok(crate::tiny_glb()),
            HttpResponse::ok(
                r#"{"list":[{"name":"textures.zip","url":"https://cdn.example/t.zip"},{"name":"model.glb","url":"https://cdn.example/m.glb"}]}"#,
            ),
            HttpResponse::ok(
                r#"{"jobs":[{"uuid":"j1","status":"Done"},{"uuid":"j2","status":"Done"}]}"#,
            ),
            HttpResponse::ok(r#"{"jobs":[{"uuid":"j1","status":"Generating"}]}"#),
            HttpResponse::ok(
                r#"{"uuid":"task-9","jobs":{"uuids":["j1","j2"],"subscription_key":"sk-sub"}}"#,
            ),
        ]);
        let p = provider(t.clone());

        let id = p
            .request_generation(&GenerationRequest::text_to_3d("npc_prop", "木吉他"))
            .unwrap();
        assert_eq!(id.0, "rodin:task-9|sk-sub");

        // First poll: Generating → Running.
        assert!(matches!(p.poll_task(&id).unwrap(), TaskStatus::Running));

        // Second poll: all Done → download (top-level uuid!) → CDN GET
        // without bearer → validated GLB asset.
        match p.poll_task(&id).unwrap() {
            TaskStatus::Succeeded(a) => {
                assert_eq!(a.format, AssetFormat::Glb);
                assert!(crate::validate_asset(&a, &Default::default()).is_ok());
                assert_eq!(a.provider_id, "rodin-v2");
            }
            other => panic!("expected asset, got {other:?}"),
        }

        let reqs = t.requests.lock().unwrap();
        // create: multipart with the documented fields, bearer auth
        assert!(reqs[0].url.ends_with("/rodin"));
        let ct = &reqs[0].headers[1].1;
        assert!(ct.starts_with("multipart/form-data; boundary="), "{ct}");
        let body = reqs[0].body.as_ref().unwrap();
        let text = String::from_utf8_lossy(body);
        assert!(text.contains("name=\"prompt\""));
        assert!(text.contains("木吉他"));
        assert!(text.contains("geometry_file_format"));
        // status: JSON keyed by subscription_key (NOT the uuid)
        assert!(reqs[1].url.ends_with("/status"));
        let status_body = String::from_utf8_lossy(reqs[1].body.as_ref().unwrap());
        assert!(status_body.contains("sk-sub"));
        assert!(!status_body.contains("task-9"));
        // download: keyed by the TOP-LEVEL uuid (request #3: create,
        // status Generating, status Done, download)
        assert!(reqs[3].url.ends_with("/download"));
        let dl_body = String::from_utf8_lossy(reqs[3].body.as_ref().unwrap());
        assert!(dl_body.contains("task-9"));
        // CDN fetch carries NO Authorization (signed URL)
        assert!(reqs[4].headers.is_empty());
    }

    #[test]
    fn any_failed_job_fails_the_task() {
        let t = fixture_transport(vec![HttpResponse::ok(
            r#"{"jobs":[{"uuid":"j1","status":"Done"},{"uuid":"j2","status":"Failed"}]}"#,
        )]);
        let p = provider(t);
        let id = TaskId("rodin:t|s".into());
        match p.poll_task(&id).unwrap() {
            TaskStatus::Failed(m) => assert!(m.contains("Failed"), "{m}"),
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn create_rejects_before_any_network_without_key() {
        let t = fixture_transport(vec![]);
        let mut cfg = crate::ProviderConfig {
            kind: crate::ProviderKind::Rodin,
            ..crate::ProviderConfig::default()
        };
        cfg.kind = crate::ProviderKind::Rodin;
        cfg.api_key = String::new();
        let p = RodinProvider::new(cfg, t);
        let err = p
            .request_generation(&GenerationRequest::text_to_3d("x", "y"))
            .unwrap_err();
        assert!(err.to_string().contains("ORIERA_GENAI_API_KEY"));
    }

    #[test]
    fn http_401_maps_to_provider_error() {
        let t = fixture_transport(vec![HttpResponse {
            status: 401,
            body: b"{}".to_vec(),
        }]);
        let p = provider(t);
        let err = p
            .request_generation(&GenerationRequest::text_to_3d("x", "y"))
            .unwrap_err();
        assert!(err.to_string().contains("401"));
        // #106 vocabulary: auth failures are not retryable
        assert!(!err.retryable());
    }

    #[test]
    fn wire_shapes_are_pinned() {
        // parse_create: acceptance needs uuid + subkey
        let v: Value = serde_json::from_str(
            r#"{"message":"Submitted.","uuid":"u1","jobs":{"uuids":["a"],"subscription_key":"s1"},"consumed":0.5}"#,
        )
        .unwrap();
        assert_eq!(parse_create(&v).unwrap(), ("u1".into(), "s1".into()));
        assert!(parse_create(&serde_json::json!({"message":"err"})).is_err());
        // parse_download: picks the .glb entry among siblings
        let v: Value = serde_json::from_str(
            r#"{"list":[{"name":"a.zip","url":"u1"},{"name":"b.GLB","url":"u2"}]}"#,
        )
        .unwrap();
        assert_eq!(parse_download(&v).unwrap(), "u2", "case-insensitive .glb");
    }
}
