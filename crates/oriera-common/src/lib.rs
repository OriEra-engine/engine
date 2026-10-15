//! L0 Common — shared types used across all layers.
//! No dependencies on other oriera crates.

use std::time::{SystemTime, UNIX_EPOCH};

/// A nanosecond timestamp from a monotonic clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub fn now() -> Self {
        Self(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        )
    }
    pub fn elapsed(self, since: Self) -> f32 {
        (self.0.saturating_sub(since.0)) as f32 / 1_000_000_000.0
    }
}

/// Frame counter, starts at 0, increments each tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameIndex(pub u64);

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug)]
pub enum EngineError {
    UnsupportedPlatform(String),
    AssetNotFound(String),
    Ecs(String),
}
