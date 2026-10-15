//! L2 Core — real-time frame scheduler and determinism frame.
//!
//! The scheduler is generic over the world type `W`: L2 stays below the
//! object model (L3) and must not know what a `World` is. The application
//! shell instantiates `Scheduler<oriera_ecs::World>` and owns the world —
//! systems receive `&mut W` each frame. No shared global state crosses
//! layer boundaries (book T3 §3.8.1).

use oriera_common::{FrameIndex, Timestamp};

/// Target frame time in nanoseconds (60fps ≈ 16.6ms).
pub const FRAME_BUDGET_NS: u64 = 16_666_666;

/// Context passed to every system each frame.
pub struct FrameCtx {
    pub frame: FrameIndex,
    pub dt: f32,
    pub now: Timestamp,
}

/// A named system that runs each frame against the world `W`.
pub trait System<W> {
    fn name(&self) -> &str;
    fn run(&mut self, ctx: &FrameCtx, world: &mut W);
}

/// The frame scheduler: runs registered systems in order, tracks frame time.
pub struct Scheduler<W> {
    systems: Vec<Box<dyn System<W>>>,
    frame: FrameIndex,
    last: Timestamp,
    frame_times: Vec<f32>,
}

impl<W> Scheduler<W> {
    pub fn new() -> Self {
        Self {
            systems: Vec::new(),
            frame: FrameIndex(0),
            last: Timestamp::now(),
            frame_times: Vec::new(),
        }
    }

    pub fn add_system(&mut self, s: Box<dyn System<W>>) {
        self.systems.push(s);
    }

    /// Run one frame. Returns the measured frame time in ms.
    pub fn tick(&mut self, world: &mut W) -> f32 {
        let start = Timestamp::now();
        let dt = start.elapsed(self.last);
        self.last = start;

        let ctx = FrameCtx {
            frame: self.frame,
            dt,
            now: start,
        };

        for s in &mut self.systems {
            s.run(&ctx, world);
        }

        let frame_ms = Timestamp::now().elapsed(start) * 1000.0;
        self.frame.0 += 1;
        self.frame_times.push(frame_ms);
        if self.frame_times.len() > 60 {
            self.frame_times.remove(0);
        }
        frame_ms
    }

    /// Mean frame time over the last 60 frames, in ms.
    pub fn avg_frame_ms(&self) -> f32 {
        if self.frame_times.is_empty() {
            return 0.0;
        }
        self.frame_times.iter().sum::<f32>() / self.frame_times.len() as f32
    }
}

impl<W> Default for Scheduler<W> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct IncSystem;

    impl System<u32> for IncSystem {
        fn name(&self) -> &str {
            "inc"
        }
        fn run(&mut self, _ctx: &FrameCtx, world: &mut u32) {
            *world += 1;
        }
    }

    #[test]
    fn scheduler_passes_world_to_systems() {
        let mut sched: Scheduler<u32> = Scheduler::new();
        sched.add_system(Box::new(IncSystem));
        let mut world = 0u32;
        sched.tick(&mut world);
        sched.tick(&mut world);
        assert_eq!(world, 2);
    }
}
