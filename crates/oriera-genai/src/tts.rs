//! L10 TTS 出口适配器（#108 批①）——文本 → 音频字节（MP3）。
//!
//! wire 对齐 OpenAI `/v1/audio/speech` 形状（piper 等本地 server 的
//! 兼容实现直接可用）：POST `{base}/audio/speech`，JSON
//! `{model, input, voice?, response_format:"mp3"}`，响应体即音频字节。
//! Protocol only, transport injected（ADR 0003，Tripo/Rodin 同款）。
//!
//! 引擎不落盘——产物写到哪（Studio 的 assets/vo/、宿主的播放器）是
//! 壳层的事；这里只负责把文本诚实地变成字节。
//!
//! 空音色 → 请求**省略** `voice` 字段（用供应商自己的默认），不发明
//! 假音色名。

use crate::transport::{HttpRequest, SharedTransport};
use crate::{GenAiError, ProviderConfig};
use serde_json::json;

/// TTS 供应商边界（L10 出口）。
pub trait TtsProvider: Send + Sync {
    /// 稳定供应商 id（如 `"piper:zh_CN-huayan"`）。
    fn id(&self) -> &str;
    /// 合成一段话。`voice` 传空 = 供应商默认音色；返回音频字节（MP3）。
    fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, GenAiError>;
}

/// OpenAI `/v1/audio/speech` 兼容客户端。
pub struct HttpTtsProvider {
    config: ProviderConfig,
    transport: SharedTransport,
    model: String,
    label: String,
}

impl HttpTtsProvider {
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

    fn speech_url(&self) -> String {
        format!("{}/audio/speech", self.config.base())
    }

    /// 与聊天/生成路径同款的 key 纪律：错误信息里的 key 与 Bearer 形态
    /// 全部脱敏（key 只进请求头，永不进错误文本）。
    fn scrub(&self, s: &str) -> String {
        let key = self.config.api_key.trim();
        if key.is_empty() {
            return s.to_string();
        }
        s.replace(key, "***")
            .replace(&format!("Bearer {key}"), "Bearer ***")
    }
}

impl TtsProvider for HttpTtsProvider {
    fn id(&self) -> &str {
        &self.label
    }

    fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, GenAiError> {
        if text.trim().is_empty() {
            return Err(GenAiError::Unsupported("tts: empty text".into()));
        }
        // 空音色省略字段——供应商默认，不发明音色名
        let mut body = json!({
            "model": self.model,
            "input": text,
            "response_format": "mp3",
        });
        if !voice.trim().is_empty() {
            body["voice"] = json!(voice.trim());
        }
        let req = HttpRequest::post_json(self.speech_url(), &self.config.api_key, body.to_string());
        let resp = self
            .transport
            .send(&req)
            .map_err(|e| GenAiError::Provider(self.scrub(&e)))?;
        if resp.status >= 400 {
            let text = String::from_utf8_lossy(&resp.body);
            return Err(GenAiError::Provider(self.scrub(&format!(
                "tts HTTP {}: {}",
                resp.status,
                &text[..text.len().min(160)]
            ))));
        }
        if resp.body.is_empty() {
            return Err(GenAiError::Provider(
                "tts reply carried no audio bytes (200 but empty body)".into(),
            ));
        }
        Ok(resp.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderKind;
    use crate::testing::fixture_transport;
    use crate::testing::ScriptedTransport;
    use crate::transport::HttpResponse;
    use std::sync::Arc;

    fn provider(responses: Vec<HttpResponse>) -> (HttpTtsProvider, Arc<ScriptedTransport>) {
        let transport = fixture_transport(responses);
        let handle = transport.clone();
        let p = HttpTtsProvider::new(
            ProviderConfig {
                kind: ProviderKind::Piper,
                model: "piper".into(),
                api_key: "test-key-tts".into(),
                base_url: Some("http://127.0.0.1:5000/v1".into()),
            },
            "piper",
            transport,
        );
        (p, handle)
    }

    /// wire：URL/请求体五要素（model/input/voice/response_format/编码）+ Bearer。
    #[test]
    fn request_shape_and_bearer() {
        let (p, transport) = provider(vec![HttpResponse::ok(b"ID3mp3bytes".to_vec())]);
        let audio = p.synthesize("末班车要开了", "zh_CN-huayan").unwrap();
        assert_eq!(audio, b"ID3mp3bytes", "音频字节原样返回（引擎不落盘）");
        let recorded = transport.requests.lock().unwrap()[0].clone();
        assert!(recorded.url.ends_with("/audio/speech"));
        let body = String::from_utf8_lossy(recorded.body.as_deref().unwrap());
        assert!(body.contains("\"model\":\"piper\""));
        assert!(body.contains("\"input\":\"末班车要开了\""));
        assert!(body.contains("\"voice\":\"zh_CN-huayan\""));
        assert!(body.contains("\"response_format\":\"mp3\""));
        assert!(recorded
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer test-key-tts"));
    }

    /// 空音色：字段整体省略（不发明默认音色名）。
    #[test]
    fn empty_voice_omits_the_field() {
        let (p, transport) = provider(vec![HttpResponse::ok(b"a".to_vec())]);
        p.synthesize("你好", "").unwrap();
        let recorded = transport.requests.lock().unwrap()[0].clone();
        let body = String::from_utf8_lossy(recorded.body.as_deref().unwrap());
        assert!(!body.contains("voice"), "空音色省略字段：{body}");
    }

    /// 空文本：Unsupported 快速失败（不花一次网络）。
    #[test]
    fn empty_text_fails_fast() {
        let (p, _) = provider(vec![]);
        assert!(matches!(
            p.synthesize("  ", "v"),
            Err(GenAiError::Unsupported(_))
        ));
    }

    /// 非 2xx：状态码+脱敏后的片段进 Provider 错（key 不外泄）。
    #[test]
    fn http_error_scrubs_the_key() {
        let (p, _) = provider(vec![HttpResponse {
            status: 500,
            body: b"boom with test-key-tts inside".to_vec(),
        }]);
        let err = p.synthesize("你好", "v").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("500"));
        assert!(!msg.contains("test-key-tts"), "key 必须脱敏：{msg}");
    }

    /// 200 但空体：诚实报错（不把空字节当成功音频）。
    #[test]
    fn empty_body_is_an_error() {
        let (p, _) = provider(vec![HttpResponse::ok(Vec::new())]);
        let err = p.synthesize("你好", "v").unwrap_err();
        assert!(err.to_string().contains("no audio bytes"));
    }
}
