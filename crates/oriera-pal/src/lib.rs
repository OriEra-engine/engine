//! # OriEra L1 Platform Abstraction Layer (PAL)
//!
//! PAL is the entire engine's I/O boundary. It isolates upper layers from
//! hardware/OS differences. Upper layers NEVER branch on `#[cfg(target_os)]`;
//! they only read [`Capabilities`] and call the trait objects exposed here.
//!
//! Five first-class targets:
//!   * PC            (Win / macOS / Linux)
//!   * Mobile        (iOS / Android)
//!   * Web           (Wasm + WebGPU, browser)
//!   * AR Glasses    (visionOS / Snapdragon Spatial / WebXR)
//!   * BCI reserved  (EEG/LSL, v1 only a trait — see `platforms/bci_reserved`)

pub mod capability;
pub mod input;
pub mod gfx;
pub mod storage;
pub mod net;
pub mod xr;

pub use capability::{Capabilities, PlatformProfile};

/// Detect the current platform and return its capability bitmap.
/// Called exactly once at startup; the result is immutable thereafter.
pub fn detect_platform() -> PlatformProfile {
    #[cfg(target_os = "windows")]
    { PlatformProfile::pc("windows") }
    #[cfg(target_os = "macos")]
    { PlatformProfile::pc("macos") }
    #[cfg(target_os = "linux")]
    { PlatformProfile::pc("linux") }
    #[cfg(target_os = "ios")]
    { PlatformProfile::mobile("ios") }
    #[cfg(target_os = "android")]
    { PlatformProfile::mobile("android") }
    #[cfg(target_arch = "wasm32")]
    { PlatformProfile::web() }
    #[cfg(all(target_os = "visionos"))]
    { PlatformProfile::ar_glasses("visionos") }
    #[cfg(not(any(
        target_os = "windows", target_os = "macos", target_os = "linux",
        target_os = "ios", target_os = "android", target_arch = "wasm32",
        target_os = "visionos"
    )))]
    {
        PlatformProfile::unknown()
    }
}
