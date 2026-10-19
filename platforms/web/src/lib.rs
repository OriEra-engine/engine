//! Browser host. `boot` preserves the synchronous, conservative baseline;
//! `boot_detected` returns a JSON profile after asynchronous API probes.
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/probe.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = probeBrowser)]
    async fn probe_browser() -> Result<JsValue, JsValue>;
}

#[wasm_bindgen]
pub fn boot() -> String {
    format!("{:?}", oriera_pal::detect_platform())
}

/// Capabilities are decimal strings so JavaScript can use BigInt without
/// losing future u64 bits. Budget is policy, not detected physical memory.
#[wasm_bindgen]
pub async fn boot_detected() -> String {
    let observed = probe_browser().await.unwrap_or(JsValue::NULL);
    let flag = |name: &str| {
        js_sys::Reflect::get(&observed, &JsValue::from_str(name))
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    };
    let profile =
        oriera_pal::PlatformProfile::web_with_capabilities(oriera_pal::BrowserCapabilities {
            webgpu: flag("webgpu"),
            touch: flag("touch"),
            keyboard_mouse: flag("keyboardMouse"),
            webtransport: flag("webtransport"),
        });
    serde_json::json!({
        "family": "web",
        "os_name": profile.os_name,
        "capabilities": profile.caps.bits().to_string(),
        "frame_budget_us": profile.frame_budget_us,
        "vram_budget_mb": profile.vram_budget_mb,
    })
    .to_string()
}

#[cfg(all(test, target_arch = "wasm32", target_os = "unknown"))]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;
    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn scheduler_clock_runs_in_browser() {
        let mut scheduler = oriera_core::Scheduler::<()>::new();
        assert!(scheduler.tick(&mut ()).is_finite());
    }

    #[wasm_bindgen_test]
    async fn detected_profile_is_valid_json() {
        let profile: serde_json::Value = serde_json::from_str(&boot_detected().await).unwrap();
        assert_eq!(profile["family"], "web");
        assert!(profile["capabilities"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .is_ok());
    }
}
