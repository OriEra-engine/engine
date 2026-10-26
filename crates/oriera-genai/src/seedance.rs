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
    AssetFormat, GenAiError, GeneratedAsset, GenerationKind, GenerationProvider, GenerationRequest,
    ImageRole, ProviderCapabilities, QualityTier, TaskId, TaskStatus,
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

    /// Tier-aware model resolution (v0.2): an explicit
    /// `ProviderConfig::model` always wins; otherwise the quality tier
    /// maps onto the field-tested Ark ids (R0 2026-10-24).
    fn model_for(&self, tier: QualityTier) -> &str {
        if !self.config.model.is_empty() {
            return &self.config.model;
        }
        match tier {
            QualityTier::Draft => "doubao-seedance-2-0-mini-260615",
            QualityTier::Hero => "doubao-seedance-2-5-260628",
            QualityTier::Standard => DEFAULT_MODEL,
        }
    }

    fn tier_tag(tier: QualityTier) -> &'static str {
        match tier {
            QualityTier::Draft => "draft",
            QualityTier::Hero => "hero",
            QualityTier::Standard => "std",
        }
    }

    fn tier_from_tag(tag: &str) -> QualityTier {
        match tag {
            "draft" => QualityTier::Draft,
            "hero" => QualityTier::Hero,
            _ => QualityTier::Standard,
        }
    }

    /// Peak-usage price per kilotoken in micro-yuan (field-tested
    /// 2026-10-24; off-peak bills ~55% of the Hero tier figure).
    fn price_micros_per_ktoken(model: &str) -> u64 {
        if model.contains("mini") {
            23_000 // 0.023 CNY per 1k tokens
        } else if model.contains("2-5") {
            77_000 // 0.077 CNY per 1k tokens, peak
        } else {
            31_000 // 2.0 standard, 0.031
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
            image_to_video: true,
            text_to_speech: false, // v0.1 request surface has no image field
        }
    }

    fn request_generation(&self, req: &GenerationRequest) -> Result<TaskId, GenAiError> {
        if !self.capabilities().supports(req.kind) {
            return Err(GenAiError::Unsupported(format!(
                "seedance adapter serves video kinds only; {} requested",
                req.kind.id()
            )));
        }
        if req.kind == GenerationKind::ImageToVideo && req.image_refs.is_empty() {
            return Err(GenAiError::Unsupported(
                "image_to_video needs at least one image_ref".into(),
            ));
        }
        if self.config.api_key.is_empty() {
            return Err(GenAiError::Provider(
                "seedance selected without ORIERA_GENAI_API_KEY".into(),
            ));
        }
        let model = self.model_for(req.quality);
        let tag = Self::tier_tag(req.quality);
        let body = build_create_task_body(model, req);
        match self.submit(&body)? {
            Some(remote) => Ok(TaskId(format!("{ID_PREFIX}{remote}|{tag}"))),
            // Content-moderation fallback (field-tested interfilm M1):
            // resubmit text-only; identity then rests on the prompt locks.
            None => self.degraded_text_only(model, tag, req),
        }
    }

    fn poll_task(&self, task: &TaskId) -> Result<TaskStatus, GenAiError> {
        let Some(payload) = task.0.strip_prefix(ID_PREFIX) else {
            return Err(GenAiError::Provider(format!(
                "not a seedance task: {}",
                task.0
            )));
        };
        // TaskId carries the tier tag ("cgt-...|draft") so poll bills the
        // model the request actually targeted; pre-v0.2 ids parse as std.
        let (remote, tag) = match payload.split_once('|') {
            Some((r, t)) => (r, t),
            None => (payload, "std"),
        };
        let tier = Self::tier_from_tag(tag);
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
            PollOutcome::Done(url, usage) => {
                let bytes = self.fetch(&url)?;
                let model = self.model_for(tier);
                let usage = usage.map(|mut u| {
                    // 先乘后除（评审 R3 QA P2：先除后乘每条截断少记
                    // ~0.9k tokens 等效成本）
                    u.cost_micros = Some(u.tokens * Self::price_micros_per_ktoken(model) / 1000);
                    u
                });
                Ok(TaskStatus::Succeeded(GeneratedAsset {
                    format: AssetFormat::Video,
                    payload: bytes,
                    provider_id: self.id().into(),
                    model: model.to_string(),
                    usage,
                }))
            }
            PollOutcome::Failed(m) => Ok(TaskStatus::Failed(m)),
            PollOutcome::Pending => Ok(TaskStatus::Running),
        }
    }
}

impl SeedanceProvider {
    /// One create attempt. `Ok(None)` = moderation rejection of the input
    /// image (`InputImageSensitiveContentDetected`, HTTP 400) — the caller
    /// degrades instead of failing the whole batch.
    fn submit(&self, body: &str) -> Result<Option<String>, GenAiError> {
        let resp = self
            .transport
            .send(&HttpRequest::post_json(
                self.create_url(),
                &self.config.api_key,
                body.into(),
            ))
            .map_err(GenAiError::Provider)?;
        if !resp.is_success() {
            let text = String::from_utf8_lossy(&resp.body).to_string();
            if resp.status == 400 && text.contains("InputImageSensitiveContentDetected") {
                return Ok(None);
            }
            return Err(GenAiError::Provider(format!(
                "seedance create returned HTTP {}: {}",
                resp.status,
                text.chars().take(160).collect::<String>()
            )));
        }
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| GenAiError::Provider(format!("seedance create body: {e}")))?;
        parse_create(&v).map(Some)
    }

    fn degraded_text_only(
        &self,
        model: &str,
        tag: &str,
        req: &GenerationRequest,
    ) -> Result<TaskId, GenAiError> {
        let mut stripped = req.clone();
        stripped.kind = GenerationKind::TextToVideo;
        stripped.image_refs.clear();
        let body = build_create_task_body(model, &stripped);
        match self.submit(&body)? {
            Some(remote) => Ok(TaskId(format!("{ID_PREFIX}{remote}|{tag}"))),
            None => Err(GenAiError::Provider(
                "text-only resubmit also tripped moderation (unexpected)".into(),
            )),
        }
    }

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
    let mut content = vec![json!({ "type": "text", "text": req.prompt })];
    if req.kind == GenerationKind::ImageToVideo {
        if let Some(img) = req.image_refs.first() {
            let role = if model.contains("mini") {
                ImageRole::FirstFrame.ark_role()
            } else {
                ImageRole::ReferenceImage.ark_role()
            };
            let b64 = crate::base64_encode(&img.data);
            content.push(json!({
                "type": "image_url",
                "image_url": { "url": format!("data:image/png;base64,{b64}") },
                "role": role
            }));
        }
    }
    json!({
        "model": model,
        "content": content,
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
    Done(String, Option<crate::UsageReport>),
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
            let usage = v
                .pointer("/usage/completion_tokens")
                .and_then(Value::as_u64)
                .map(|tokens| crate::UsageReport {
                    tokens,
                    cost_micros: None,
                });
            Ok(PollOutcome::Done(url.to_string(), usage))
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
        assert_eq!(id.0, "seedance:cgt-42|std"); // v0.2: tier rides in the TaskId

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
    fn quality_tier_maps_onto_field_tested_models() {
        let t = fixture_transport(vec![]);
        let mut p = provider(t);
        assert_eq!(
            p.model_for(QualityTier::Draft),
            "doubao-seedance-2-0-mini-260615"
        );
        assert_eq!(p.model_for(QualityTier::Hero), "doubao-seedance-2-5-260628");
        assert_eq!(p.model_for(QualityTier::Standard), DEFAULT_MODEL);
        // explicit config.model always wins
        p.config.model = "my-finetune".into();
        assert_eq!(p.model_for(QualityTier::Hero), "my-finetune");
    }

    #[test]
    fn i2v_body_carries_first_frame_with_tier_appropriate_role() {
        // mini tier (draft) → role first_frame; png bytes ride as data URL.
        let req = GenerationRequest::image_to_video("lin", "她接过伞", crate::tiny_png());
        let mut r = req.clone();
        r.quality = QualityTier::Draft;
        let body = build_create_task_body("doubao-seedance-2-0-mini-260615", &r);
        assert!(body.contains("\"role\":\"first_frame\""));
        assert!(body.contains("data:image/png;base64,"));
        // 2.5 tier (hero) → role reference_image (both field-tested R0).
        let mut r2 = req;
        r2.quality = QualityTier::Hero;
        let body2 = build_create_task_body("doubao-seedance-2-5-260628", &r2);
        assert!(body2.contains("\"role\":\"reference_image\""));
    }

    #[test]
    fn moderation_rejection_degrades_to_text_only() {
        // Pop order: text-only resubmit OK, moderation 400 for the image attempt.
        let t = fixture_transport(vec![
            HttpResponse::ok(r#"{"id":"cgt-deg"}"#),
            HttpResponse {
                status: 400,
                body: br#"{"error":{"code":"InputImageSensitiveContentDetected.PrivacyInformation","message":"may contain real person"}}"#.to_vec(),
            },
        ]);
        let p = provider(t.clone());
        let id = p
            .request_generation(&GenerationRequest::image_to_video(
                "lin",
                "近景：林晚站在站台上",
                crate::tiny_png(),
            ))
            .unwrap();
        assert!(id.0.starts_with("seedance:cgt-deg|"));
        let reqs = t.requests.lock().unwrap();
        let first = String::from_utf8(reqs[0].body.clone().unwrap()).unwrap();
        let second = String::from_utf8(reqs[1].body.clone().unwrap()).unwrap();
        assert!(
            first.contains("image_url"),
            "initial i2v attempt carries the image"
        );
        assert!(
            !second.contains("image_url"),
            "degraded resubmit must drop the image"
        );
    }

    #[test]
    fn usage_tokens_and_cost_ride_the_asset() {
        // 108900 tokens on the mini tier → 108 * 23000 = 2_484_000 micros (¥2.48).
        let t = fixture_transport(vec![
            HttpResponse::ok(crate::tiny_mp4_stub()),
            HttpResponse::ok(
                r#"{"id":"cgt-m","status":"succeeded",
                    "content":{"video_url":"https://v.example/m.mp4"},
                    "usage":{"completion_tokens":108900}}"#,
            ),
            HttpResponse::ok(r#"{"id":"cgt-m"}"#),
        ]);
        let p = provider(t);
        let id = p
            .request_generation(&{
                let mut r = GenerationRequest::text_to_video("v", "雨");
                r.quality = QualityTier::Draft;
                r
            })
            .unwrap();
        match p.poll_task(&id).unwrap() {
            TaskStatus::Succeeded(a) => {
                let u = a.usage.expect("usage present");
                assert_eq!(u.tokens, 108_900);
                assert_eq!(u.cost_micros, Some(108 * 23_000));
                assert!(a.model.contains("mini"));
            }
            other => panic!("expected success, got {other:?}"),
        }
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
            Ok(PollOutcome::Done(url, usage)) => {
                assert_eq!(url, "https://ark.tos/rain.mp4");
                assert_eq!(usage.map(|u| u.tokens), Some(108_900));
            }
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
