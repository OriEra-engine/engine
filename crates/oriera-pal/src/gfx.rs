//! OriGFX — unified graphics backend boundary (L1).
//!
//! Contract: `oriera.pal.gfx.v1` (Appendix B, Table B-1:
//! CreateGfxBackend / SubmitFrame). Upper layers request a backend by
//! kind and receive an opaque handle — native API objects never leak
//! upward. wgpu is the reference implementation; hot paths may swap to
//! raw Vulkan/DX12/Metal behind the same trait.

/// Which backend to request. Availability is reported in
/// [`crate::Capabilities`] — callers fall back, they never assume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GfxBackendKind {
    /// wgpu / WebGPU — the portable default.
    Wgpu,
    Vulkan,
    Dx12,
    Metal,
}

/// Frame submission result reported to L2 for budget accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitResult {
    /// Backend-side submission time in microseconds.
    pub submit_us: u64,
    /// True when the device was lost and PAL fell back to software.
    pub used_software_fallback: bool,
}

/// The graphics boundary every upper layer talks to.
pub trait GfxBackend {
    /// Begin a frame. Returns false when the surface is unavailable
    /// (minimized window, lost device) and the frame should be skipped.
    fn begin_frame(&mut self) -> bool;

    /// Submit the encoded frame. P99 budget: ≤ 0.5 ms (Table B-1).
    fn submit_frame(&mut self) -> SubmitResult;
}

/// No-op backend for tests and headless CI runs.
#[derive(Debug, Default)]
pub struct NullGfxBackend;

impl GfxBackend for NullGfxBackend {
    fn begin_frame(&mut self) -> bool {
        true
    }
    fn submit_frame(&mut self) -> SubmitResult {
        SubmitResult {
            submit_us: 0,
            used_software_fallback: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_backend_round_trips() {
        let mut b = NullGfxBackend;
        assert!(b.begin_frame());
        assert_eq!(b.submit_frame().submit_us, 0);
    }
}
