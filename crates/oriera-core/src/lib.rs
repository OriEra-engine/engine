//! L2 Core — real-time frame scheduler.
//!
//! Splits each frame into fixed budget slots, runs systems in order,
//! and reports timing per slot.

use oriera_common::{FrameIndex, Timestamp};

/// Target frame time in nanoseconds (60fps ≈ 16.6ms).
pub const FRAME_BUDGET_NS: u64 = 16_666_666;

/// A named system that runs each frame.
pub trait System: Send + Sync {
    fn name(&self) -> &str;
    fn run(&mut self, ctx: &FrameCtx);
}

/// Context passed to every system each frame.
pub struct FrameCtx {
    pub frame: FrameIndex,
    pub dt: f32,
    pub now: Timestamp,
}

/// The frame scheduler.
pub struct Scheduler {
    systems: Vec<Box<dyn System>>,
    frame: FrameIndex,
    last: Timestamp,
    pub frame_times: Vec<f32>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            systems: Vec::new(),
            frame: FrameIndex(0),
            last: Timestamp::now(),
            frame_times: Vec::new(),
        }
    }

    pub fn add_system(&mut self, s: Box<dyn System>) {
        self.systems.push(s);
    }

    /// Run one frame. Returns frame time in ms.
    pub fn tick(&mut self) -> f32 {
        let now = Timestamp::now();
        let dt = now.elapsed(self.last);
        self.last = now;

        let ctx = FrameCtx {
            frame: self.frame,
            dt,
            now,
        };

        for s in &mut self.systems {
            s.run(&ctx);
        }

        let frame_ms = now.elapsed(self.last) * 1000.0;
        self.frame.0 += 1;
        self.frame_times.push(frame_ms);
        if self.frame_times.len() > 60 {
            self.frame_times.remove(0);
        }
        frame_ms
    }

    pub fn avg_frame_ms(&self) -> f32 {
        if self.frame_times.is_empty() {
            return 0.0;
        }
        self.frame_times.iter().sum::<f32>() / self.frame_times.len() as f32
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
