//! XR session boundary (L1).
//!
//! OpenXR 1.1 adapter surface. On AR glasses this is where camera pose,
//! hand/eye tracking and spatial anchors enter the engine — normalized
//! into the same [`crate::input::SpatialInputEvent`] stream as every
//! other input. v0.1 defines the surface only; visionOS/骁龙/WebXR
//! adapters land with the AR platform work (ROADMAP 2027 H2).

/// Head pose for one display frame: position + orientation quaternion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadPose {
    pub position: [f32; 3],
    /// (x, y, z, w).
    pub orientation: [f32; 4],
}

/// A running XR session. One per process, owned by the platform shell.
pub trait XrSession {
    /// Latest head pose, or None when tracking is lost.
    fn head_pose(&mut self) -> Option<HeadPose>;

    /// Whether the compositor needs reprojection (motion-to-photon
    /// budget is 20 ms — Table B-5, CompositeAR).
    fn needs_reprojection(&self) -> bool;
}

/// Stub session for PC/web: no tracking, never reprojects.
#[derive(Debug, Default)]
pub struct NoXrSession;

impl XrSession for NoXrSession {
    fn head_pose(&mut self) -> Option<HeadPose> {
        None
    }
    fn needs_reprojection(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_session_has_no_tracking() {
        let mut s = NoXrSession;
        assert!(s.head_pose().is_none());
        assert!(!s.needs_reprojection());
    }
}
