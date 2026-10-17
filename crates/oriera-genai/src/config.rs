//! Boot-time provider selection — the L8 mirror of L7's
//! `ProviderConfig` / `create_provider` (AI-NATIVE.md). Vendors are
//! **data**, chosen by the application shell (config file / env / UI);
//! the engine never hardcodes one and never logs the key.

use crate::{GenerationProvider, MockProvider};
use std::sync::Arc;

/// Which generation backend a creator plugged in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// Deterministic offline backend — tests, CI, keyless mode.
    Mock,
    /// Tripo (tripo3d.ai) text→GLB. Protocol adapter behind the
    /// `providers` feature; current endpoint generation V2 (retires
    /// 2026-11-01 — `base_url` is config, migrating is one diff).
    Tripo,
    /// Text→PNG/JPEG stills through the OpenAI **images dialect**
    /// (`POST {base}/images/generations`). Default base is Zhipu
    /// CogView; OpenAI images or any compatible gateway is the same
    /// adapter with `base_url`/`model` overrides.
    CogView,
    /// ByteDance Seedance video models via a Volcano-Ark-compatible
    /// tasks API. Also behind the `providers` feature.
    Seedance,
}

impl ProviderKind {
    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::Mock => "mock",
            ProviderKind::Tripo => "tripo",
            ProviderKind::CogView => "cogview",
            ProviderKind::Seedance => "seedance",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "mock" | "" => Some(ProviderKind::Mock),
            "tripo" | "tripo3d" => Some(ProviderKind::Tripo),
            "cogview" | "image" | "images" => Some(ProviderKind::CogView),
            "seedance" | "ark" | "doubao-seedance" => Some(ProviderKind::Seedance),
            _ => None,
        }
    }

    /// Default API base per kind. Override for gateways/proxies.
    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::Mock => "",
            // V2 generation; watch the 2026-11-01 retirement notice.
            ProviderKind::Tripo => "https://api.tripo3d.ai",
            // Zhipu open platform (CogView); OpenAI-compatible images
            // endpoints override via base_url.
            ProviderKind::CogView => "https://open.bigmodel.cn/api/paas/v4",
            // Volcano Ark (volcengine); BytePlus variants override the base.
            ProviderKind::Seedance => "https://ark.cn-beijing.volces.com",
        }
    }
}

/// The creator's choice, resolved once at boot. `api_key` lives only
/// in the shell (env/config), never in engine logs or world state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// Vendor model id (e.g. `doubao-seedance-2-0`); empty = vendor default.
    pub model: String,
    pub api_key: String,
    pub base_url: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Mock,
            model: String::new(),
            api_key: String::new(),
            base_url: None,
        }
    }
}

impl ProviderConfig {
    /// Read the shell-side selection from the environment — the native
    /// counterpart of the browser's `localStorage` boot. `mock` is the
    /// default so every demo runs keyless:
    ///
    /// - `ORIERA_GENAI_PROVIDER` — `mock` (default) | `tripo` | `seedance`
    /// - `ORIERA_GENAI_API_KEY` — vendor key (never printed)
    /// - `ORIERA_GENAI_MODEL` — vendor model id override
    /// - `ORIERA_GENAI_BASE_URL` — endpoint override (gateway/proxy)
    pub fn from_env() -> Self {
        let read = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let kind = read("ORIERA_GENAI_PROVIDER")
            .and_then(|v| ProviderKind::from_id(&v.to_lowercase()))
            .unwrap_or(ProviderKind::Mock);
        Self {
            kind,
            model: read("ORIERA_GENAI_MODEL").unwrap_or_default(),
            api_key: read("ORIERA_GENAI_API_KEY").unwrap_or_default(),
            base_url: read("ORIERA_GENAI_BASE_URL"),
        }
    }

    /// Effective API base (override or per-kind default).
    pub fn base(&self) -> &str {
        match &self.base_url {
            Some(b) => b,
            None => self.kind.default_base_url(),
        }
    }
}

/// Resolve a config into a provider. Remote kinds need the
/// `providers` feature **and** an injected transport; without them
/// this returns the offline backend with the choice recorded for
/// observability — exactly how L7 behaves while its native HTTP
/// adapters are pending. Remote kinds with an empty key also degrade
/// to mock instead of failing every request at runtime.
pub fn create_generation_provider(cfg: &ProviderConfig) -> Arc<dyn GenerationProvider> {
    #[cfg(feature = "providers")]
    {
        if !cfg.api_key.is_empty() {
            match cfg.kind {
                ProviderKind::Mock => {}
                kind => {
                    tracing::info!(
                        provider = kind.id(),
                        base = cfg.base(),
                        "L8 remote generation provider selected without transport — inject one via create_generation_provider_with_transport"
                    );
                }
            }
        }
    }
    let _ = cfg;
    Arc::new(MockProvider::new())
}

/// Resolve a config with a transport into a real remote adapter
/// (`providers` feature). Mock still resolves to the offline backend.
#[cfg(feature = "providers")]
pub fn create_generation_provider_with_transport(
    cfg: &ProviderConfig,
    transport: crate::transport::SharedTransport,
) -> Arc<dyn GenerationProvider> {
    use crate::{cogview::CogViewProvider, seedance::SeedanceProvider, tripo::TripoProvider};
    match cfg.kind {
        ProviderKind::Mock => Arc::new(MockProvider::new()),
        ProviderKind::Tripo => Arc::new(TripoProvider::new(cfg.clone(), transport)),
        ProviderKind::CogView => Arc::new(CogViewProvider::new(cfg.clone(), transport)),
        ProviderKind::Seedance => Arc::new(SeedanceProvider::new(cfg.clone(), transport)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_roundtrip_and_default_to_mock() {
        for id in [
            "mock", "tripo", "tripo3d", "cogview", "image", "images", "seedance", "ark", "",
        ] {
            assert!(ProviderKind::from_id(id).is_some(), "{id}");
        }
        assert_eq!(ProviderKind::from_id("nope"), None);
        assert_eq!(ProviderConfig::default().kind, ProviderKind::Mock);
    }

    #[test]
    fn base_urls_are_overridable() {
        let mut cfg = ProviderConfig {
            kind: ProviderKind::Tripo,
            ..ProviderConfig::default()
        };
        assert!(cfg.base().contains("tripo3d"));
        cfg.base_url = Some("https://my-gateway.example".into());
        assert_eq!(cfg.base(), "https://my-gateway.example");
        assert!(ProviderKind::Seedance.default_base_url().contains("volces"));
        // The images dialect defaults to Zhipu CogView and is overridable
        // to any OpenAI-images-compatible endpoint.
        assert!(ProviderKind::CogView
            .default_base_url()
            .contains("bigmodel"));
    }

    #[test]
    fn factory_offline_by_default() {
        // No feature/transport: every remote choice resolves to the
        // deterministic offline backend — keyless, hermetic.
        for kind in [
            ProviderKind::Mock,
            ProviderKind::Tripo,
            ProviderKind::Seedance,
        ] {
            let cfg = ProviderConfig {
                kind,
                ..ProviderConfig::default()
            };
            let p = create_generation_provider(&cfg);
            assert!(p.capabilities().text_to_3d); // mock serves all kinds
        }
    }
}
