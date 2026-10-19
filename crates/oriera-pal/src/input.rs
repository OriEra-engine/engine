//! Unified spatial input (L1).
//!
//! Contract: `oriera.pal.input.v1`, machine-readable in
//! `contracts/oriera/pal/v1/spatial_input.proto`. Every source — keyboard,
//! pointer, hand tracking, eye gaze, voice, spatial anchors — normalizes
//! into [`SpatialInputEvent`] so upper layers never care which device
//! produced an event. BCI (`bci` field in the proto) is reserved: v1
//! never emits it.

use serde::{Deserialize, Serialize};

/// A normalized input event with its timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpatialInputEvent {
    pub timestamp_us: u64,
    pub body: InputBody,
}

/// The device-neutral payload. Mirrors the proto `oneof body`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputBody {
    Key(KeyEvent),
    Pointer(PointerEvent),
    Hand(HandPoseEvent),
    Gaze(EyeGazeEvent),
    Voice(VoiceEvent),
    Anchor(SpatialAnchorEvent),
    #[deprecated(note = "Reserved for stage 3. v1 never emits this.")]
    Bci(BciIntentEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub code: u32,
    pub pressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub buttons: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandPoseEvent {
    /// 0 = left, 1 = right.
    pub hand: u32,
    /// Flat joint positions, 3 floats per joint.
    pub joints: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EyeGazeEvent {
    pub x: f32,
    pub y: f32,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceEvent {
    pub transcript: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpatialAnchorEvent {
    pub anchor_id: String,
    pub p: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BciIntentEvent {
    pub intent: String,
    pub confidence: f32,
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

    #[test]
    fn pointer_event_round_trips_and_matches_json_shape() {
        let pv = PointerEvent {
            x: 12.5,
            y: -3.25,
            buttons: 5,
        };
        let json = serde_json::to_string(&pv).expect("serialize PointerEvent");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse json to Value");
        assert_eq!(
            value,
            serde_json::json!({ "x": 12.5, "y": -3.25, "buttons": 5 }),
            "wire shape changed"
        );
        let back: PointerEvent = serde_json::from_str(&json).expect("deserialize PointerEvent");
        assert_eq!(pv, back, "round-trip via string lost data");
    }
}
