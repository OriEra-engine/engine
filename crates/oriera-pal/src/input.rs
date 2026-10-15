//! Unified spatial input (L1).
//!
//! Contract: `oriera.pal.input.v1`, machine-readable in
//! `contracts/oriera/pal/v1/spatial_input.proto`. Every source — keyboard,
//! pointer, hand tracking, eye gaze, voice, spatial anchors — normalizes
//! into [`SpatialInputEvent`] so upper layers never care which device
//! produced an event. BCI (`bci` field in the proto) is reserved: v1
//! never emits it.

/// A normalized input event with its timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct SpatialInputEvent {
    pub timestamp_us: u64,
    pub body: InputBody,
}

/// The device-neutral payload. Mirrors the proto `oneof body`.
#[derive(Debug, Clone, PartialEq)]
pub enum InputBody {
    Key(KeyEvent),
    Pointer(PointerEvent),
    Hand(HandPoseEvent),
    Gaze(EyeGazeEvent),
    Voice(VoiceEvent),
    Anchor(SpatialAnchorEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: u32,
    pub pressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub buttons: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HandPoseEvent {
    /// 0 = left, 1 = right.
    pub hand: u32,
    /// Flat joint positions, 3 floats per joint.
    pub joints: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeGazeEvent {
    pub x: f32,
    pub y: f32,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceEvent {
    pub transcript: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpatialAnchorEvent {
    pub anchor_id: String,
    pub p: [f32; 3],
}

/// A source of input events. Implementations live in `platforms/`;
/// the engine core only sees this trait.
pub trait InputSource {
    /// Drain pending events into `out`. Timestamps must be monotonic —
    /// events are never dropped silently (Table B-1, PollInputEvents).
    fn poll(&mut self, out: &mut Vec<SpatialInputEvent>);
}

/// Empty source for tests and headless platforms.
#[derive(Debug, Default)]
pub struct NullInputSource;

impl InputSource for NullInputSource {
    fn poll(&mut self, _out: &mut Vec<SpatialInputEvent>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_source_polls_nothing() {
        let mut src = NullInputSource;
        let mut out = Vec::new();
        src.poll(&mut out);
        assert!(out.is_empty());
    }
}
