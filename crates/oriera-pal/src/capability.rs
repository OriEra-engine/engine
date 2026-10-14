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

/// Per-platform profile detected at startup.
#[derive(Debug, Clone)]
pub struct PlatformProfile {
    pub family: PlatformFamily,
    pub os_name: String,
    pub caps: Capabilities,
    /// Frame budget in microseconds. AR glasses 90fps = 11111us; desktop 60fps = 16666us.
    pub frame_budget_us: u32,
    /// VR budget in MB (端侧 8GB budget).
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

impl PlatformProfile {
    pub fn pc(os: &str) -> Self {
        Self {
            family: PlatformFamily::Pc,
            os_name: os.to_string(),
            caps: Capabilities::GFX_WGPU
                | Capabilities::GFX_VULKAN
                | Capabilities::GFX_DX12
                | Capabilities::GFX_METAL
                | Capabilities::INPUT_KEYBOARD_MOUSE
                | Capabilities::INPUT_GAMEPAD
                | Capabilities::NET_QUIC
                | Capabilities::VRAM_16GB,
            frame_budget_us: 16_666,
            vram_budget_mb: 8192,
        }
    }

    pub fn mobile(os: &str) -> Self {
        Self {
            family: PlatformFamily::Mobile,
            os_name: os.to_string(),
            caps: Capabilities::GFX_WGPU
                | Capabilities::GFX_METAL
                | Capabilities::GFX_VULKAN
                | Capabilities::INPUT_TOUCH
                | Capabilities::INPUT_VOICE_STREAM
                | Capabilities::NPU_TOPS_48
                | Capabilities::VRAM_8GB
                | Capabilities::UNIFIED_MEMORY,
            frame_budget_us: 16_666,
            vram_budget_mb: 4096,
        }
    }

    pub fn web() -> Self {
        Self {
            family: PlatformFamily::Web,
            os_name: "browser".into(),
            caps: Capabilities::GFX_WEBGPU
                | Capabilities::INPUT_KEYBOARD_MOUSE
                | Capabilities::INPUT_TOUCH
                | Capabilities::NET_WEBTRANSPORT,
            frame_budget_us: 16_666,
            vram_budget_mb: 4096, // Chrome caps WebGPU context ~ half of system VRAM
        }
    }

    pub fn ar_glasses(os: &str) -> Self {
        Self {
            family: PlatformFamily::ArGlasses,
            os_name: os.to_string(),
            caps: Capabilities::GFX_WGPU
                | Capabilities::GFX_WEBGPU
                | Capabilities::INPUT_HAND_TRACKING
                | Capabilities::INPUT_EYE_GAZE
                | Capabilities::INPUT_VOICE_STREAM
                | Capabilities::INPUT_SPATIAL_ANCHOR
                | Capabilities::FOVEATED_RENDERING
                | Capabilities::VIDEO_BACKED_LAYER
                | Capabilities::NPU_TOPS_48
                | Capabilities::VRAM_8GB
                | Capabilities::UNIFIED_MEMORY,
            frame_budget_us: 11_111, // 90fps
            vram_budget_mb: 3072,
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

    #[test]
    fn bci_bits_are_zero_in_v1() {
        let p = PlatformProfile::pc("linux");
        assert!(!p.caps.contains(Capabilities::BCI_EEG_LSL));
        assert!(!p.caps.contains(Capabilities::BCI_NON_INVASIVE));
    }

    #[test]
    fn ar_glasses_has_90fps_budget() {
        let p = PlatformProfile::ar_glasses("visionos");
        assert_eq!(p.frame_budget_us, 11_111);
        assert!(p.caps.contains(Capabilities::FOVEATED_RENDERING));
        assert!(p.caps.contains(Capabilities::INPUT_EYE_GAZE));
    }
}
