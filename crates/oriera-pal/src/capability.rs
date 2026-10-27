//! Capability bitmap — the contract between PAL and all upper layers.
//!
//! Upper layers MUST NOT branch on OS. They query this bitmap and pick
//! a code path. If a bit is missing, the caller degrades gracefully
//! (e.g. fall back from ray tracing to screen-space).
//!
//! The BCI bits (1 << 40+) are reserved for 2031+ stage 3; v1 always
//! reports them as 0.

use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Capabilities: u64 {
        // ===== Graphics (bits 0..=9) =====
        const GFX_WGPU            = 1 << 0;
        const GFX_VULKAN          = 1 << 1;
        const GFX_DX12            = 1 << 2;
        const GFX_METAL           = 1 << 3;
        const GFX_WEBGPU          = 1 << 4;
        const RAY_TRACING         = 1 << 5;
        const NEURAL_RENDER_3DGS  = 1 << 6;
        const FOVEATED_RENDERING  = 1 << 7;
        const VIDEO_BACKED_LAYER  = 1 << 8;  // video see-through (AR)
        const OPTICAL_COMBINER    = 1 << 9;  // optical see-through (AR)

        // ===== Input (bits 10..=19) =====
        const INPUT_KEYBOARD_MOUSE = 1 << 10;
        const INPUT_TOUCH          = 1 << 11;
        const INPUT_GAMEPAD        = 1 << 12;
        const INPUT_HAND_TRACKING  = 1 << 13;
        const INPUT_EYE_GAZE       = 1 << 14;
        const INPUT_VOICE_STREAM   = 1 << 15;
        const INPUT_SPATIAL_ANCHOR = 1 << 16;
        const INPUT_BODY_TRACKING  = 1 << 17;

        // ===== Compute budget (bits 20..=29) =====
        const NPU_TOPS_35   = 1 << 20;
        const NPU_TOPS_48   = 1 << 21;
        const NPU_TOPS_160  = 1 << 22;
        const VRAM_8GB      = 1 << 23;
        const VRAM_16GB     = 1 << 24;
        const VRAM_32GB     = 1 << 25;
        const UNIFIED_MEMORY = 1 << 26;

        // ===== Network (bits 30..=35) =====
        const NET_WEBTRANSPORT = 1 << 30;
        const NET_QUIC         = 1 << 31;
        const NET_EDGE_VPS    = 1 << 32;

        // ===== BCI reserved (bits 40..=45). v1 always 0. =====
        const BCI_EEG_LSL      = 1 << 40;
        const BCI_NON_INVASIVE  = 1 << 41;
    }
}

/// OS baseline refined by runtime probes at startup.
#[derive(Debug, Clone)]
pub struct PlatformProfile {
    pub family: PlatformFamily,
    pub os_name: String,
    pub caps: Capabilities,
    /// Frame budget in microseconds. AR glasses 90fps = 11111us; desktop 60fps = 16666us.
    pub frame_budget_us: u32,
    /// Application allocation ceiling in MB, not detected physical VRAM.
    pub vram_budget_mb: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformFamily {
    Pc,
    Mobile,
    Web,
    ArGlasses,
    BciReserved,
    Unknown,
}

/// Browser observations supplied by the platform shell. Missing probes
/// mean unavailable; no GPU, NPU, XR permission or memory size is guessed.
#[derive(Debug, Default, Clone, Copy)]
pub struct BrowserCapabilities {
    /// `requestAdapter()` returned an adapter; device creation can still fail.
    pub webgpu: bool,
    pub touch: bool,
    /// A fine pointer was observed (a hint, not proof of a physical keyboard).
    pub keyboard_mouse: bool,
    /// Secure-context WebTransport API presence, not endpoint connectivity.
    pub webtransport: bool,
}

impl PlatformProfile {
    /// Native API candidates by OS. Drivers/backends must still initialize
    /// successfully before use. Hardware tiers require a separate probe.
    pub fn pc(os: &str) -> Self {
        let graphics = match os {
            "windows" => Capabilities::GFX_WGPU | Capabilities::GFX_DX12 | Capabilities::GFX_VULKAN,
            "macos" => Capabilities::GFX_WGPU | Capabilities::GFX_METAL,
            "linux" => Capabilities::GFX_WGPU | Capabilities::GFX_VULKAN,
            _ => Capabilities::empty(),
        };
        Self {
            family: PlatformFamily::Pc,
            os_name: os.to_string(),
            caps: graphics | Capabilities::INPUT_KEYBOARD_MOUSE,
            frame_budget_us: 16_666,
            vram_budget_mb: 1024,
        }
    }

    pub fn mobile(os: &str) -> Self {
        let graphics = match os {
            "ios" => Capabilities::GFX_WGPU | Capabilities::GFX_METAL,
            "android" => Capabilities::GFX_WGPU | Capabilities::GFX_VULKAN,
            _ => Capabilities::empty(),
        };
        Self {
            family: PlatformFamily::Mobile,
            os_name: os.to_string(),
            caps: graphics | Capabilities::INPUT_TOUCH,
            frame_budget_us: 33_333,
            vram_budget_mb: 512,
        }
    }

    /// Safe pre-probe browser baseline, including headless/unsupported hosts.
    pub fn web() -> Self {
        Self::web_with_capabilities(BrowserCapabilities::default())
    }

    pub fn web_with_capabilities(detected: BrowserCapabilities) -> Self {
        let mut caps = Capabilities::empty();
        caps.set(Capabilities::GFX_WEBGPU, detected.webgpu);
        caps.set(Capabilities::INPUT_TOUCH, detected.touch);
        caps.set(Capabilities::INPUT_KEYBOARD_MOUSE, detected.keyboard_mouse);
        caps.set(Capabilities::NET_WEBTRANSPORT, detected.webtransport);
        Self {
            family: PlatformFamily::Web,
            os_name: "browser".into(),
            caps,
            frame_budget_us: 16_666,
            vram_budget_mb: 512,
        }
    }

    pub fn ar_glasses(os: &str) -> Self {
        let graphics = match os {
            "visionos" => Capabilities::GFX_WGPU | Capabilities::GFX_METAL,
            "android" => Capabilities::GFX_WGPU | Capabilities::GFX_VULKAN,
            _ => Capabilities::empty(),
        };
        Self {
            family: PlatformFamily::ArGlasses,
            os_name: os.to_string(),
            // Tracking/foveation/see-through modes are session capabilities,
            // not guarantees inferred from the device's operating system.
            caps: graphics,
            frame_budget_us: 11_111,
            vram_budget_mb: 512,
        }
    }

    /// BCI is a *reserved* channel. v1 never constructs a real profile;
    /// the bits stay 0 until stage 3 (2031+).
    pub fn bci_reserved() -> Self {
        Self {
            family: PlatformFamily::BciReserved,
            os_name: "bci-stub".into(),
            caps: Capabilities::empty(),
            frame_budget_us: 16_666,
            vram_budget_mb: 4096,
        }
    }

    pub fn unknown() -> Self {
        Self {
            family: PlatformFamily::Unknown,
            os_name: "unknown".into(),
            caps: Capabilities::empty(),
            frame_budget_us: 16_666,
            vram_budget_mb: 4096,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_profiles() -> [PlatformProfile; 6] {
        [
            PlatformProfile::pc("linux"),
            PlatformProfile::mobile("android"),
            PlatformProfile::web(),
            PlatformProfile::ar_glasses("visionos"),
            PlatformProfile::bci_reserved(),
            PlatformProfile::unknown(),
        ]
    }

    #[test]
    fn native_backends_follow_os() {
        for (profile, expected, forbidden) in [
            (
                PlatformProfile::pc("windows"),
                Capabilities::GFX_DX12,
                Capabilities::GFX_METAL,
            ),
            (
                PlatformProfile::pc("macos"),
                Capabilities::GFX_METAL,
                Capabilities::GFX_DX12 | Capabilities::GFX_VULKAN,
            ),
            (
                PlatformProfile::pc("linux"),
                Capabilities::GFX_VULKAN,
                Capabilities::GFX_METAL | Capabilities::GFX_DX12,
            ),
            (
                PlatformProfile::mobile("ios"),
                Capabilities::GFX_METAL,
                Capabilities::GFX_VULKAN | Capabilities::GFX_DX12,
            ),
            (
                PlatformProfile::mobile("android"),
                Capabilities::GFX_VULKAN,
                Capabilities::GFX_METAL | Capabilities::GFX_DX12,
            ),
            (
                PlatformProfile::ar_glasses("visionos"),
                Capabilities::GFX_METAL,
                Capabilities::GFX_WEBGPU,
            ),
        ] {
            assert!(profile.caps.contains(expected), "{}", profile.os_name);
            assert!(!profile.caps.intersects(forbidden), "{}", profile.os_name);
        }
    }

    #[test]
    fn profiles_do_not_invent_hardware_or_permissions() {
        let unprobed = Capabilities::NPU_TOPS_35
            | Capabilities::NPU_TOPS_48
            | Capabilities::NPU_TOPS_160
            | Capabilities::VRAM_8GB
            | Capabilities::VRAM_16GB
            | Capabilities::VRAM_32GB
            | Capabilities::INPUT_EYE_GAZE
            | Capabilities::INPUT_HAND_TRACKING
            | Capabilities::BCI_EEG_LSL
            | Capabilities::BCI_NON_INVASIVE;
        for p in [
            PlatformProfile::pc("macos"),
            PlatformProfile::mobile("android"),
            PlatformProfile::web(),
            PlatformProfile::ar_glasses("visionos"),
            PlatformProfile::bci_reserved(),
            PlatformProfile::unknown(),
        ] {
            assert!(!p.caps.intersects(unprobed));
        }
        // An ordinary phone must not route AI to a fictional 48 TOPS NPU.
        assert_eq!(
            crate::route(
                PlatformProfile::mobile("android").caps,
                crate::RouteHint {
                    payload_bytes: 1024,
                    latency_budget_ms: 500,
                    privacy: crate::Privacy::Any,
                }
            ),
            crate::ExecutionSite::Cloud
        );
    }

    #[test]
    fn browser_capabilities_only_follow_observations() {
        assert!(PlatformProfile::web().caps.is_empty());
        let p = PlatformProfile::web_with_capabilities(BrowserCapabilities {
            touch: true,
            webgpu: true,
            ..Default::default()
        });
        assert_eq!(p.caps, Capabilities::INPUT_TOUCH | Capabilities::GFX_WEBGPU);
        assert_eq!(p.family, PlatformFamily::Web);
    }

    #[test]
    fn bci_bits_are_zero_in_v1() {
        let bci_bits = Capabilities::BCI_EEG_LSL | Capabilities::BCI_NON_INVASIVE;

        for profile in all_profiles() {
            assert!(
                !profile.caps.intersects(bci_bits),
                "{:?} must not expose BCI capabilities in v1: {:?}",
                profile.family,
                profile.caps
            );
        }
    }

    #[test]
    fn pc_has_graphics_and_keyboard_mouse_without_eye_gaze() {
        let p = PlatformProfile::pc("linux");
        assert!(p.caps.contains(Capabilities::GFX_WGPU));
        assert!(p.caps.contains(Capabilities::INPUT_KEYBOARD_MOUSE));
        assert!(!p.caps.contains(Capabilities::INPUT_EYE_GAZE));
    }

    #[test]
    fn mobile_does_not_assume_npu_or_8gb_vram() {
        let p = PlatformProfile::mobile("android");
        assert!(!p.caps.contains(Capabilities::NPU_TOPS_48));
        assert!(!p.caps.contains(Capabilities::VRAM_8GB));
    }

    #[test]
    fn web_has_60fps_budget() {
        let p = PlatformProfile::web();
        assert_eq!(p.frame_budget_us, 16_666);
    }

    #[test]
    fn ar_glasses_has_90fps_budget() {
        let p = PlatformProfile::ar_glasses("visionos");
        assert_eq!(p.frame_budget_us, 11_111);
        assert!(!p.caps.contains(Capabilities::FOVEATED_RENDERING));
        assert!(!p.caps.contains(Capabilities::INPUT_EYE_GAZE));
    }

    #[test]
    fn bci_reserved_has_no_capabilities() {
        let p = PlatformProfile::bci_reserved();
        assert!(p.caps.is_empty());
    }

    #[test]
    fn all_platforms_stay_within_8gb_vram_budget() {
        for profile in all_profiles() {
            assert!(
                profile.vram_budget_mb <= 8192,
                "{:?} VRAM budget exceeds 8192 MB: {} MB",
                profile.family,
                profile.vram_budget_mb
            );
        }
    }
}
