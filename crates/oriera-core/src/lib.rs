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
    /// Frames in a row that exceeded the budget by >20% (reset on any
    /// in-budget frame).
    consecutive_overruns: u32,
    /// How many times the sustained-overrun warning has fired. Read by
    /// tests and the observability cross-cut (book T15).
    pub budget_warnings: u64,
}

/// Sustained overruns that trigger one warning, and the overrun ratio.
const OVERRUN_STREAK: u32 = 60;
const OVERRUN_RATIO: f32 = 1.2;

impl<W> Scheduler<W> {
    pub fn new() -> Self {
        Self {
            systems: Vec::new(),
            frame: FrameIndex(0),
            last: Timestamp::now(),
            frame_times: Vec::new(),
            consecutive_overruns: 0,
            budget_warnings: 0,
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
        self.record_budget(frame_ms);
        frame_ms
    }

    /// Book T5 `ReportBudget`: a frame >20% over budget extends the
    /// streak; 60 in a row emits one `tracing::warn!` with the mean
    /// frame time, then the streak restarts. Pure function of the
    /// measured frame time so tests can drive it deterministically.
    fn record_budget(&mut self, frame_ms: f32) {
        let budget_ms = FRAME_BUDGET_NS as f32 / 1_000_000.0;
        if frame_ms > budget_ms * OVERRUN_RATIO {
            self.consecutive_overruns += 1;
            if self.consecutive_overruns >= OVERRUN_STREAK {
                tracing::warn!(
                    frame = self.frame.0,
                    avg_frame_ms = self.avg_frame_ms(),
                    budget_ms,
                    "L2 scheduler over budget for {OVERRUN_STREAK} consecutive frames"
                );
                self.budget_warnings += 1;
                self.consecutive_overruns = 0;
            }
        } else {
            self.consecutive_overruns = 0;
        }
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

    #[test]
    fn sustained_overruns_fire_one_warning_per_streak() {
        let mut sched: Scheduler<()> = Scheduler::new();
        let over_ms = (FRAME_BUDGET_NS as f32 / 1_000_000.0) * 1.5;
        for _ in 0..60 {
            sched.record_budget(over_ms);
        }
        assert_eq!(sched.budget_warnings, 1);

        // Next 60 overruns → a second warning (streak restarted).
        for _ in 0..60 {
            sched.record_budget(over_ms);
        }
        assert_eq!(sched.budget_warnings, 2);
    }

    #[test]
    fn single_in_budget_frame_resets_the_streak() {
        let mut sched: Scheduler<()> = Scheduler::new();
        let over_ms = (FRAME_BUDGET_NS as f32 / 1_000_000.0) * 1.5;
        for _ in 0..59 {
            sched.record_budget(over_ms);
        }
        sched.record_budget(1.0); // in budget: streak resets
        for _ in 0..59 {
            sched.record_budget(over_ms);
        }
        assert_eq!(sched.budget_warnings, 0);
    }

    #[test]
    fn exactly_at_ratio_threshold_is_not_an_overrun() {
        let mut sched: Scheduler<()> = Scheduler::new();
        let at_threshold = (FRAME_BUDGET_NS as f32 / 1_000_000.0) * 1.2;
        for _ in 0..120 {
            sched.record_budget(at_threshold);
        }
        assert_eq!(sched.budget_warnings, 0);
        assert_eq!(sched.consecutive_overruns, 0);
    }
}
