//! L10 ASR 入口座（#108 批②）——音频字节 → 文本。
//!
//! wire 对齐 OpenAI `/v1/audio/transcriptions` 形状（whisper.cpp /
//! CrispASR / transcribe.cpp 等本地 server 的兼容实现直接可用）：
//! POST `{base}/audio/transcriptions`，multipart/form-data
//! （`file`=音频字节、`model`、`language?`），响应 JSON `{"text":"…"}`。
//! Protocol only, transport injected（ADR 0003，tts 同款）。

use crate::transport::{HttpRequest, SharedTransport};
use crate::{GenAiError, ProviderConfig};
use serde_json::Value;

/// ASR 供应商边界（L10 入口）。
pub trait AsrProvider: Send + Sync {
    /// 稳定供应商 id（如 `"whisper:base-zh"`）。
    fn id(&self) -> &str;
    /// 转写音频字节。`language` 传空 = 供应商自动检测（ISO 639-1）。
    fn transcribe(&self, audio: &[u8], language: &str) -> Result<String, GenAiError>;
}

/// OpenAI `/v1/audio/transcriptions` 兼容客户端。
pub struct HttpAsrProvider {
    config: ProviderConfig,
    transport: SharedTransport,
    model: String,
    label: String,
}

const BOUNDARY: &str = "oriera-asr-b41c";

impl HttpAsrProvider {
    pub fn new(
        config: ProviderConfig,
        model: impl Into<String>,
        transport: SharedTransport,
    ) -> Self {
        let model = model.into();
        let label = format!("{}:{}", config.kind.id(), model);
        Self {
            config,
            transport,
            model,
            label,
        }
    }

    fn transcriptions_url(&self) -> String {
        format!("{}/audio/transcriptions", self.config.base())
    }

    fn scrub(&self, s: &str) -> String {
        let key = self.config.api_key.trim();
        if key.is_empty() {
            return s.to_string();
        }
        s.replace(key, "***")
            .replace(&format!("Bearer {key}"), "Bearer ***")
    }

    /// multipart：`file`（二进制，octet-stream）+ `model` + `language?`。
    fn multipart(&self, audio: &[u8], language: &str) -> (String, Vec<u8>) {
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\ncontent-disposition: form-data; name=\"file\"; filename=\"audio.bin\"\r\ncontent-type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(audio);
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\ncontent-disposition: form-data; name=\"model\"\r\n\r\n{}\r\n",
                self.model
            )
            .as_bytes(),
        );
        if !language.trim().is_empty() {
            body.extend_from_slice(
                format!("--{BOUNDARY}\r\ncontent-disposition: form-data; name=\"language\"\r\n\r\n{}\r\n", language.trim())
                    .as_bytes(),
            );
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        (BOUNDARY.to_string(), body)
    }
}

impl AsrProvider for HttpAsrProvider {
    fn id(&self) -> &str {
        &self.label
    }

    fn transcribe(&self, audio: &[u8], language: &str) -> Result<String, GenAiError> {
        if audio.is_empty() {
            return Err(GenAiError::Unsupported("asr: empty audio".into()));
        }
        let (boundary, body) = self.multipart(audio, language);
        let req = HttpRequest {
            method: crate::transport::Method::Post,
            url: self.transcriptions_url(),
            headers: vec![
                (
                    "Authorization".into(),
                    format!("Bearer {}", self.config.api_key),
                ),
                (
                    "content-type".into(),
                    format!("multipart/form-data; boundary={boundary}"),
                ),
            ],
            body: Some(body),
        };
        let resp = self
            .transport
            .send(&req)
            .map_err(|e| GenAiError::Provider(self.scrub(&e)))?;
        if resp.status >= 400 {
            let text = String::from_utf8_lossy(&resp.body);
            return Err(GenAiError::Provider(self.scrub(&format!(
                "asr HTTP {}: {}",
                resp.status,
                &text[..text.len().min(160)]
            ))));
        }
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| GenAiError::Validation(format!("asr body unreadable: {e}")))?;
        let text = v
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| GenAiError::Validation("asr reply missing \"text\"".into()))?;
        Ok(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{fixture_transport, ScriptedTransport};
    use crate::transport::HttpResponse;
    use crate::ProviderKind;
    use std::sync::Arc;

    fn provider(responses: Vec<HttpResponse>) -> (HttpAsrProvider, Arc<ScriptedTransport>) {
        let transport = fixture_transport(responses);
        let handle = transport.clone();
        let p = HttpAsrProvider::new(
            ProviderConfig {
                kind: ProviderKind::Piper,
                model: "whisper-base".into(),
                api_key: "test-key-asr".into(),
                base_url: Some("http://127.0.0.1:5000/v1".into()),
            },
            "whisper-base",
            transport,
        );
        (p, handle)
    }

    /// wire：URL/multipart 三要素（file 部件+model+language）/Bearer。
    #[test]
    fn multipart_shape_and_parse() {
        let (p, transport) = provider(vec![HttpResponse::ok(
            "{\"text\":\"你好世界\"}".as_bytes().to_vec(),
        )]);
        let text = p.transcribe(b"RIFFfakeaudio", "zh").unwrap();
        assert_eq!(text, "你好世界");
        let req = transport.requests.lock().unwrap()[0].clone();
        assert!(req.url.ends_with("/audio/transcriptions"));
        let ct = req
            .headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert!(
            ct.starts_with("multipart/form-data; boundary=oriera-asr-"),
            "{ct}"
        );
        let body = String::from_utf8_lossy(req.body.as_deref().unwrap());
        assert!(body.contains("name=\"file\"; filename=\"audio.bin\""));
        assert!(body.contains("name=\"model\""));
        assert!(body.contains("whisper-base"));
        assert!(body.contains("name=\"language\""));
        assert!(body.contains("RIFFfakeaudio"), "音频字节在部件里");
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer test-key-asr"));
    }

    /// 空语言：language 字段整体省略（自动检测交给供应商）。
    #[test]
    fn empty_language_omits_the_field() {
        let (p, transport) = provider(vec![HttpResponse::ok(br#"{"text":"x"}"#.to_vec())]);
        p.transcribe(b"a", "").unwrap();
        let req = transport.requests.lock().unwrap()[0].clone();
        let body = String::from_utf8_lossy(req.body.as_deref().unwrap());
        assert!(!body.contains("name=\"language\""), "{body}");
    }

    /// 空音频：Unsupported 快速失败（不花一次网络）。
    #[test]
    fn empty_audio_fails_fast() {
        let (p, _) = provider(vec![]);
        assert!(matches!(
            p.transcribe(b"", "zh"),
            Err(GenAiError::Unsupported(_))
        ));
    }

    /// 非 2xx：状态码+脱敏片段进 Provider 错。
    #[test]
    fn http_error_scrubs_the_key() {
        let (p, _) = provider(vec![HttpResponse {
            status: 401,
            body: b"bad test-key-asr".to_vec(),
        }]);
        let err = p.transcribe(b"a", "").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("401"));
        assert!(!msg.contains("test-key-asr"), "key 必须脱敏：{msg}");
    }

    /// 200 但缺 text 字段：Parse 诚实报错（不把空串当转写结果）。
    #[test]
    fn missing_text_field_is_parse_error() {
        let (p, _) = provider(vec![HttpResponse::ok(br#"{"segments":[]}"#.to_vec())]);
        let err = p.transcribe(b"a", "").unwrap_err();
        assert!(
            err.to_string().contains("missing \\\"text\\\"") || err.to_string().contains("missing")
        );
    }
}
