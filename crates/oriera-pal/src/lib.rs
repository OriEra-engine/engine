//! # OriEra L1 Platform Abstraction Layer (PAL)
//!
//! PAL is the entire engine's I/O boundary. It isolates upper layers from
//! hardware/OS differences. Upper layers NEVER branch on `#[cfg(target_os)]`;
//! they only read [`Capabilities`] and call the trait objects exposed here.
//!
//! Boundary traits (contracts live in `contracts/oriera/pal/v1/`):
//!   * [`gfx`]     — OriGFX backend (`CreateGfxBackend`, `SubmitFrame`)
//!   * [`input`]   — unified `SpatialInputEvent` stream (`PollInputEvents`)
//!   * [`storage`] — tiered persistence (`OpenAssetStream`)
//!   * [`net`]     — edge/cloud transport (`RouteRemoteTask`)
//!   * [`xr`]      — OpenXR session surface
//!
//! Five first-class targets:
//!   * PC            (Win / macOS / Linux)
//!   * Mobile        (iOS / Android)
//!   * Web           (Wasm + WebGPU, browser)
//!   * AR Glasses    (visionOS / Snapdragon Spatial / WebXR)
//!   * BCI reserved  (EEG/LSL, v1 only a trait — see `platforms/bci_reserved`)

pub mod capability;
pub mod gfx;
pub mod input;
pub mod net;
pub mod storage;
pub mod xr;

pub use capability::{BrowserCapabilities, Capabilities, PlatformFamily, PlatformProfile};
pub use gfx::{GfxBackend, GfxBackendKind, NullGfxBackend, SubmitResult};
pub use input::{
    EyeGazeEvent, HandPoseEvent, InputBody, InputSource, KeyEvent, NullInputSource, PointerEvent,
    SpatialAnchorEvent, SpatialInputEvent, VoiceEvent,
};
pub use net::{
    route, Endpoint, ExecutionSite, NetTransport, OfflineTransport, Privacy, RouteHint,
    NPU_PAYLOAD_LIMIT_BYTES,
};
pub use storage::{MemoryStorage, Storage, StorageTier};
pub use xr::{HeadPose, NoXrSession, XrSession};

/// Select an OS baseline, not a hardware or permission probe.
/// Browser hosts refine this with `PlatformProfile::web_with_capabilities`.
/// WASI is not a browser and deliberately receives the unknown profile.
pub fn detect_platform() -> PlatformProfile {
    if cfg!(all(target_family = "wasm", target_os = "unknown")) {
        return PlatformProfile::web();
    }
    match std::env::consts::OS {
        "windows" | "macos" | "linux" => PlatformProfile::pc(std::env::consts::OS),
        "ios" | "android" => PlatformProfile::mobile(std::env::consts::OS),
        "visionos" => PlatformProfile::ar_glasses("visionos"),
        _ => PlatformProfile::unknown(),
    }
}
