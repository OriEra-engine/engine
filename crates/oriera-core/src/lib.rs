//! L2 Core — real-time frame scheduler and determinism frame.
//!
//! The scheduler is generic over the world type `W`: L2 stays below the
//! object model (L3) and must not know what a `World` is. The application
//! shell instantiates `Scheduler<oriera_ecs::World>` and owns the world —
//! systems receive `&mut W` each frame. No shared global state crosses
//! layer boundaries (book T3 §3.8.1).

use oriera_common::{FrameIndex, Grants, Timestamp};
use std::collections::VecDeque;

pub mod allocator;

pub use allocator::{AllocatorConfig, BudgetAllocator};
pub use oriera_common::{ComputeGrant, Grants as BudgetGrants};

/// Target frame time in nanoseconds (60fps ≈ 16.6ms).
pub const FRAME_BUDGET_NS: u64 = 16_666_666;

/// How many per-system timings and frame samples are kept for the
/// rolling averages the scheduler reports.
const HISTORY: usize = 60;

/// Context passed to every system each frame. `grants` carries the
/// compute allocation decided for *this* frame by the L2
/// [`BudgetAllocator`] — spend-hungry systems scale their work by
/// `ctx.grants.throttle_of(self.name())`; systems that don't look
/// simply run at full speed (missing entry = unmanaged).
pub struct FrameCtx<'a> {
    pub frame: FrameIndex,
    pub dt: f32,
    pub now: Timestamp,
    pub grants: Grants<'a>,
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
    frame_times: VecDeque<f32>,
    /// Frames in a row that exceeded the budget by >20% (reset on any
    /// in-budget frame).
    consecutive_overruns: u32,
    /// How many times the sustained-overrun warning has fired. Read by
    /// tests and the observability cross-cut (book T15).
    pub budget_warnings: u64,
    /// Frame budget this scheduler is graded against. Defaults to
    /// [`FRAME_BUDGET_NS`]; platform shells pass the L1 profile's
    /// budget (`frame_budget_us * 1000`) via [`Scheduler::with_budget_ns`]
    /// so AR glasses at 90fps are not graded on the 60fps curve.
    budget_ns: u64,
    /// Per-system wall time of the most recent frame, in ms, in run
    /// order. The answer to "which system ate this frame" — for
    /// AI-native apps that is usually cognition or inference. Names
    /// are captured once at `add_system`; ticks only rewrite the ms.
    system_ms: Vec<(String, f32)>,
    /// Externally supplied virtual costs (#103 E4), folded into the
    /// next tick's allocator observation — tick measures real ms by
    /// assignment, so extras must merge in after measurement.
    extra_ms: Vec<(String, f32)>,
    /// Fixed simulation step (issue #66). `Some(dt)` makes every
    /// `FrameCtx::dt` exactly `dt` regardless of wall-clock pacing —
    /// the determinism dial for replay/evaluation workloads. `None`
    /// (default) keeps wall-clock dt for real-time apps.
    fixed_dt: Option<f32>,
    /// Frame-budget arbiter (compute slicing). Regulates on observed
    /// per-system cost; see [`BudgetAllocator`].
    allocator: BudgetAllocator,
    /// Reused grant table — steady-state ticks allocate nothing.
    grant_table: Vec<(String, oriera_common::ComputeGrant)>,
}

/// Sustained overruns that trigger one warning, and the overrun ratio.
const OVERRUN_STREAK: u32 = 60;
const OVERRUN_RATIO: f32 = 1.2;

impl<W> Scheduler<W> {
    pub fn new() -> Self {
        Self::with_budget_ns(FRAME_BUDGET_NS)
    }

    /// Build a scheduler graded against a specific frame budget (ns).
    pub fn with_budget_ns(budget_ns: u64) -> Self {
        Self {
            systems: Vec::new(),
            frame: FrameIndex(0),
            last: Timestamp::now(),
            frame_times: VecDeque::with_capacity(HISTORY + 1),
            consecutive_overruns: 0,
            budget_warnings: 0,
            budget_ns: budget_ns.max(1),
            system_ms: Vec::new(),
            extra_ms: Vec::new(),
            fixed_dt: None,
            allocator: BudgetAllocator::default(),
            grant_table: Vec::new(),
        }
    }

    /// Pin the simulation step (issue #66): every tick hands systems
    /// exactly `dt` in [`FrameCtx`], so physics integration and every
    /// other dt-consumer become independent of wall-clock pacing — two
    /// runs of the same command stream produce bit-identical worlds,
    /// on any machine, under any load. Frame time, budget accounting
    /// and allocator observation still use the real clock: regulation
    /// stays honest while semantics become reproducible
    /// (COMPUTE-BUDGET.md §5).
    ///
    /// Values ≤ 0 clamp to 1e-6 (a zero step would silently freeze the
    /// world; freezing is L6 `TimeScale(0.0)`'s job, visibly).
    pub fn with_fixed_dt(mut self, dt: f32) -> Self {
        self.fixed_dt = Some(dt.max(1e-6));
        self
    }

    /// The pinned step, if any (for observability; systems read
    /// `ctx.dt`, not this).
    pub fn fixed_dt(&self) -> Option<f32> {
        self.fixed_dt
    }

    /// Replace the allocator tuning (e.g. `AllocatorConfig::off()` for
    /// pass-through behavior, or faster rhythms in tests).
    pub fn with_allocator(mut self, config: AllocatorConfig) -> Self {
        self.allocator = BudgetAllocator::new(config);
        self
    }

    /// Read-only access to the budget arbiter (load, throttles, config).
    pub fn allocator(&self) -> &BudgetAllocator {
        &self.allocator
    }

    /// The grant table systems are reading this frame (empty when the
    /// allocator is off — everyone unmanaged, full throttle).
    pub fn grants(&self) -> Grants<'_> {
        Grants::new(&self.grant_table)
    }

    /// Fraction of the frame budget left on the table over the last
    /// 60 frames: `1.0` = everything (or no data yet), `0.0` = spent.
    pub fn headroom(&self) -> f32 {
        if self.frame_times.is_empty() {
            return 1.0;
        }
        (1.0 - self.avg_frame_ms() / self.budget_ms()).clamp(0.0, 1.0)
    }

    pub fn add_system(&mut self, s: Box<dyn System<W>>) {
        self.allocator.register(s.name());
        self.system_ms.push((s.name().to_string(), 0.0));
        self.systems.push(s);
    }

    /// The configured frame budget in ms.
    pub fn budget_ms(&self) -> f32 {
        self.budget_ns as f32 / 1_000_000.0
    }

    /// Per-system wall time of the last frame, in run order:
    /// `[(name, ms), ...]`. Updated by every [`Scheduler::tick`].
    pub fn system_timings(&self) -> &[(String, f32)] {
        &self.system_ms
    }

    /// Run one frame. Returns the measured frame time in ms.
    ///
    /// Order inside the tick: the allocator first derives this frame's
    /// grants from everything observed so far and hands them to the
    /// systems via [`FrameCtx::grants`]; after the systems run, the
    /// fresh per-system timings are folded back into the allocator's
    /// cost model. Regulation leads, observation closes the loop.
    pub fn tick(&mut self, world: &mut W) -> f32 {
        let start = Timestamp::now();
        // Semantics vs regulation (issue #66): with a pinned step the
        // ctx carries exactly that dt; the wall delta is still measured
        // below for frame accounting and the allocator.
        let dt = self.fixed_dt.unwrap_or_else(|| start.elapsed(self.last));
        self.last = start;

        self.allocator
            .allocate(self.budget_ms(), &mut self.grant_table);

        let ctx = FrameCtx {
            frame: self.frame,
            dt,
            now: start,
            grants: Grants::new(&self.grant_table),
        };

        for (s, timing) in self.systems.iter_mut().zip(self.system_ms.iter_mut()) {
            let t0 = Timestamp::now();
            s.run(&ctx, world);
            timing.1 = Timestamp::now().elapsed(t0) * 1000.0;
        }

        // Merge externally supplied virtual costs (#103 E4) AFTER the
        // real measurements so they survive to this frame's observation.
        for (n, e) in &self.extra_ms {
            if let Some((_, ms)) = self.system_ms.iter_mut().find(|(sn, _)| sn == n) {
                *ms += e;
            }
        }
        self.extra_ms.clear();

        self.allocator.observe(&self.system_ms);

        let frame_ms = Timestamp::now().elapsed(start) * 1000.0;
        self.frame.0 += 1;
        self.frame_times.push_back(frame_ms);
        if self.frame_times.len() > HISTORY {
            self.frame_times.pop_front();
        }
        self.record_budget(frame_ms);
        frame_ms
    }

    /// Book T5 `ReportBudget`: a frame >20% over budget extends the
    /// streak; 60 in a row emits one `tracing::warn!` with the mean
    /// frame time, then the streak restarts. Pure function of the
    /// measured frame time so tests can drive it deterministically.
    fn record_budget(&mut self, frame_ms: f32) {
        let budget_ms = self.budget_ms();
        if frame_ms > budget_ms * OVERRUN_RATIO {
            self.consecutive_overruns += 1;
            if self.consecutive_overruns >= OVERRUN_STREAK {
                let worst = self
                    .system_ms
                    .iter()
                    .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                    .map(|(n, _)| n.as_str())
                    .unwrap_or("");
                tracing::warn!(
                    frame = self.frame.0,
                    avg_frame_ms = self.avg_frame_ms(),
                    budget_ms,
                    worst_system = worst,
                    "L2 scheduler over budget for {OVERRUN_STREAK} consecutive frames"
                );
                self.budget_warnings += 1;
                self.consecutive_overruns = 0;
            }
        } else {
            self.consecutive_overruns = 0;
        }
    }

    /// Fold an *externally derived* virtual cost into one system's
    /// observed ms before the next allocator observation (#103, review
    /// E4: wire the B7 breaker state into the EWMA — a circuit that is
    /// open is fail-fast, so without compensation a failing AI channel
    /// would *win* budget, the perverse incentive). The shell computes
    /// the value, e.g. via
    /// [`FallbackProvider::regulator_hint_ms`](oriera_infer)'s
    /// modelled retry/degradation latency. Negative inputs are clamped
    /// to zero: this API can only *add* pressure, never fake headroom.
    pub fn observe_extra_cost(&mut self, system_name: &str, extra_ms: f32) {
        let extra = extra_ms.max(0.0);
        if extra == 0.0 {
            return;
        }
        match self.extra_ms.iter_mut().find(|(n, _)| n == system_name) {
            Some((_, v)) => *v += extra,
            None => self.extra_ms.push((system_name.to_string(), extra)),
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
    fn custom_budget_changes_the_grading_curve() {
        // AR-glasses budget (11.1ms): 14ms frames are overruns here but
        // not against the 60fps default.
        let mut sched: Scheduler<()> = Scheduler::with_budget_ns(11_111_000);
        let over_ms = sched.budget_ms() * 1.5;
        for _ in 0..60 {
            sched.record_budget(over_ms);
        }
        assert_eq!(sched.budget_warnings, 1);

        let mut desktop: Scheduler<()> = Scheduler::new();
        for _ in 0..120 {
            desktop.record_budget(over_ms);
        }
        assert_eq!(desktop.budget_warnings, 0);
        assert!((desktop.budget_ms() - 16.6666).abs() < 0.01);
    }

    #[test]
    fn extra_cost_pressures_the_ewma_and_never_fakes_headroom() {
        // #103 E4: a failing (breaker-open) AI channel must LOSE budget
        // headroom, not win it — negative input is clamped, unknown
        // system names are a no-op.
        let mut sched: Scheduler<u32> = Scheduler::with_budget_ns(16_000_000);
        sched.add_system(Box::new(IncSystem));
        let mut world = 0u32;
        sched.tick(&mut world);
        sched.observe_extra_cost(IncSystem.name(), 100.0);
        sched.observe_extra_cost(IncSystem.name(), -50.0); // clamped away
        sched.observe_extra_cost("no-such-system", 99.0); // no-op
                                                          // The fold lands on the NEXT tick's allocator observation
                                                          // (real measurements are taken by assignment each tick, extras
                                                          // merge in after them): +100ms virtual on a 16ms budget must
                                                          // push the observed load past 1.0 even through EWMA damping.
        sched.tick(&mut world);
        let load1 = sched.allocator().load(sched.budget_ms());
        assert!(load1 > 1.0, "folded cost must register as load: {load1}");
        // Sustained virtual overload must squeeze the line below full
        // speed (EWMA converges, then the overrun streak trips it) —
        // an open breaker LOSES headroom, never wins it.
        for _ in 0..40 {
            sched.observe_extra_cost(IncSystem.name(), 100.0);
            sched.tick(&mut world);
        }
        let throttled = sched.allocator().throttle_of(IncSystem.name());
        assert!(
            throttled < 1.0,
            "an over-budget line must be squeezed: throttle={throttled}"
        );
    }

    #[test]
    fn tick_records_per_system_timings_in_run_order() {
        let mut sched: Scheduler<u32> = Scheduler::new();
        sched.add_system(Box::new(IncSystem));
        sched.add_system(Box::new(IncSystem));
        let mut world = 0u32;
        sched.tick(&mut world);
        let t = sched.system_timings();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].0, "inc");
        assert!(t.iter().all(|(_, ms)| *ms >= 0.0));
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

    #[test]
    fn grants_flow_from_allocator_into_the_tick() {
        let mut sched: Scheduler<()> = Scheduler::new();
        sched.add_system(Box::new(Noop));
        let mut w = ();
        sched.tick(&mut w);
        // Even on frame 0 the table exists (cold start: everyone
        // managed, full throttle — never a zero-work first frame).
        assert!(!sched.grants().is_empty());
        assert!(sched.grants().is_managed("noop"));
        assert_eq!(sched.grants().throttle_of("noop"), 1.0);
    }

    /// Records every ctx.dt it receives (world = the log itself).
    struct DtRecorder;

    impl System<Vec<f32>> for DtRecorder {
        fn name(&self) -> &str {
            "dt-recorder"
        }
        fn run(&mut self, ctx: &FrameCtx, world: &mut Vec<f32>) {
            world.push(ctx.dt);
        }
    }

    #[test]
    fn fixed_dt_pins_the_step_under_wall_clock_noise() {
        let mut sched: Scheduler<Vec<f32>> = Scheduler::new().with_fixed_dt(1.0 / 60.0);
        sched.add_system(Box::new(DtRecorder));
        let mut log = Vec::new();
        for _ in 0..3 {
            sched.tick(&mut log);
            std::thread::sleep(std::time::Duration::from_millis(7)); // jitter
        }
        assert_eq!(log, vec![1.0 / 60.0; 3], "noise must not leak into ctx.dt");
        assert_eq!(sched.fixed_dt(), Some(1.0 / 60.0));
    }

    #[test]
    fn default_keeps_wall_clock_dt() {
        let mut sched: Scheduler<Vec<f32>> = Scheduler::new();
        assert_eq!(sched.fixed_dt(), None);
        sched.add_system(Box::new(DtRecorder));
        let mut log = Vec::new();
        sched.tick(&mut log);
        std::thread::sleep(std::time::Duration::from_millis(6));
        sched.tick(&mut log);
        // First dt is construction-to-first-tick (>0); second covers the
        // sleep — wall clock, and definitely not a pinned 1/60.
        assert!(log.iter().all(|dt| *dt > 0.0));
        assert!(log[1] > 0.005, "expected wall-measured dt, got {}", log[1]);
    }

    #[test]
    fn non_positive_fixed_dt_clamps_instead_of_freezing() {
        let mut sched: Scheduler<Vec<f32>> = Scheduler::new().with_fixed_dt(0.0);
        sched.add_system(Box::new(DtRecorder));
        let mut log = Vec::new();
        sched.tick(&mut log);
        assert_eq!(log[0], 1e-6);
    }

    struct Noop;

    impl System<()> for Noop {
        fn name(&self) -> &str {
            "noop"
        }
        fn run(&mut self, _ctx: &FrameCtx, _world: &mut ()) {}
    }

    #[test]
    fn allocator_off_means_transparent_passthrough() {
        let mut sched: Scheduler<u32> = Scheduler::new().with_allocator(AllocatorConfig::off());
        sched.add_system(Box::new(IncSystem));
        let mut world = 0u32;
        for _ in 0..10 {
            sched.tick(&mut world);
        }
        assert_eq!(world, 10); // behavior identical to v0.3
        assert!(sched.grants().is_empty());
        assert_eq!(sched.allocator().throttle_of("inc"), 1.0);
    }

    #[test]
    fn headroom_is_a_fraction_of_unspent_budget() {
        let sched: Scheduler<()> = Scheduler::new();
        assert_eq!(sched.headroom(), 1.0); // no data yet
    }
}
