//! L2 budget allocator — the compute-slicing micro-model (book T3 §3.5).
//!
//! v0.2's lubricants were per-layer brakes: L5 watched the frame clock,
//! L7 hard-coded a queue cap, L2 only warned *after* the frame was
//! already blown. The allocator upgrades that to **allocate before you
//! spend**: every system's recent cost is tracked as an EWMA, and the
//! frame budget is carved into per-system shares — proportional to
//! observed cost, with a guaranteed floor so cheap systems are never
//! starved. Regulation is driven by one honest signal:
//!
//! - **Over-subscribed frame** (`Σ cost > budget`, sustained): every
//!   spender is handed a `throttle` ≈ `share / cost` — proportional
//!   fairness, i.e. everyone slows to `1/load`, while systems whose
//!   target exceeds 1.0 stay at full speed. The floor keeps a squeezed
//!   system alive (L7 combines it with its `min 1` command guarantee).
//! - **Real headroom** (`Σ cost < budget × headroom`, sustained):
//!   throttles recover slowly, one step at a time — the same
//!   degrade-fast / recover-slow hysteresis the L5 quality governor
//!   uses, applied one layer earlier.
//! - **In between: hold.** A frame inside its budget is not trimmed —
//!   saving when there is nothing to save from is the job of the
//!   *consumers* (L6 idle-skip, L9 cadence, L7 coalescing), not of the
//!   arbiter.
//!
//! Two invariants worth stating out loud:
//!
//! 1. **Unmanaged by default.** A system missing from the grant table
//!    reads full throttle. Plugging a new system (or a new model behind
//!    L7) never requires touching the allocator first — friction stays
//!    at zero (see `docs/COMPUTE-BUDGET.md`).
//! 2. **Regulation, not replay.** Throttles derive from wall-clock
//!    observation, so they pace *work*, not world semantics. Workloads
//!    needing bit-equal re-scheduling can disable the allocator
//!    ([`AllocatorConfig::off`]) and get exact pass-through behavior.

use oriera_common::ComputeGrant;

/// Tuning of the [`BudgetAllocator`]. Defaults mirror the L5 quality
/// governor's rhythm (degrade after 30 bad frames, recover after 120
/// good ones) so the whole engine breathes at the same tempo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AllocatorConfig {
    /// Guaranteed fraction of the frame budget for every system,
    /// regardless of how cheap it looks. Anti-starvation floor.
    pub min_share: f32,
    /// EWMA smoothing factor for observed per-system cost (higher =
    /// react faster, noisier).
    pub ema_alpha: f32,
    /// Load above 1.0 counts as over-subscribed (tolerance band).
    pub over_tolerance: f32,
    /// Sustained over-subscription frames before a squeeze step.
    pub squeeze_after: u32,
    /// Load below `budget × relax_headroom` counts as real headroom.
    pub relax_headroom: f32,
    /// Sustained headroom frames before a recovery step.
    pub relax_after: u32,
    /// Additive throttle recovery per step, capped at 1.0.
    pub relax_step: f32,
    /// Hard floor for the throttle — a squeezed system always keeps a
    /// tenth of its rate.
    pub floor: f32,
    /// `false` = pass-through: the grant table stays empty and every
    /// system reads full throttle.
    pub enabled: bool,
}

impl Default for AllocatorConfig {
    fn default() -> Self {
        Self {
            min_share: 0.05,
            ema_alpha: 0.2,
            over_tolerance: 0.02,
            squeeze_after: 30,
            relax_headroom: 0.6,
            relax_after: 120,
            relax_step: 0.05,
            floor: 0.1,
            enabled: true,
        }
    }
}

impl AllocatorConfig {
    /// Regulation off: grants stay empty, every system unmanaged.
    pub fn off() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

/// Per-system regulation state. Owned by the allocator, never published.
#[derive(Debug, Clone, Copy)]
struct SysState {
    /// Smoothed observed cost in ms (cold-starts at 0 = "unknown, cheap").
    ema_ms: f32,
    throttle: f32,
    over_streak: u32,
    under_streak: u32,
}

impl SysState {
    fn fresh() -> Self {
        Self {
            ema_ms: 0.0,
            throttle: 1.0,
            over_streak: 0,
            under_streak: 0,
        }
    }
}

/// The frame-budget arbiter. One instance lives inside the
/// [`Scheduler`](crate::Scheduler); it is driven by the per-system
/// timings the scheduler already measures, so enabling it costs no new
/// instrumentation. [`BudgetAllocator::allocate`] runs at the top of
/// each tick (regulating on history), [`BudgetAllocator::observe`] at
/// the bottom (folding in the frame that just ran).
pub struct BudgetAllocator {
    config: AllocatorConfig,
    /// Parallel to the scheduler's system list: same order, same length.
    names: Vec<String>,
    states: Vec<SysState>,
}

impl BudgetAllocator {
    pub fn new(config: AllocatorConfig) -> Self {
        Self {
            config,
            names: Vec::new(),
            states: Vec::new(),
        }
    }

    /// Register a system slot (the scheduler does this from
    /// `add_system`). Late registrations start unthrottled.
    pub fn register(&mut self, name: &str) {
        self.names.push(name.to_string());
        self.states.push(SysState::fresh());
    }

    pub fn config(&self) -> &AllocatorConfig {
        &self.config
    }

    /// Fold one frame of measured per-system timings into the EWMA.
    /// Runs positionally over `system_ms`, which the scheduler keeps in
    /// registration order — the two lists stay in lockstep.
    pub fn observe(&mut self, system_ms: &[(String, f32)]) {
        if !self.config.enabled {
            return;
        }
        while self.states.len() < system_ms.len() {
            // A system added since the last observe: fresh slot.
            self.register("");
        }
        let alpha = self.config.ema_alpha;
        for (state, (_, ms)) in self.states.iter_mut().zip(system_ms.iter()) {
            state.ema_ms = if state.ema_ms == 0.0 {
                *ms // cold start: seed with the first sample
            } else {
                state.ema_ms + alpha * (*ms - state.ema_ms)
            };
        }
    }

    /// Derive this frame's grant table and advance the regulation
    /// streaks. Called at the top of the tick, so this frame's grants
    /// are decided from everything observed so far. Fills `out` in
    /// registration order; `out` is reused frame to frame so
    /// steady-state ticks allocate nothing.
    pub fn allocate(&mut self, budget_ms: f32, out: &mut Vec<(String, ComputeGrant)>) {
        // Steady-state zero allocation (#103, review E12): keep the
        // caller's table sized to the registry and refresh numbers in
        // place -- the per-frame name.clone() in publish is gone.
        // Growth (and its one clone per row) happens only when a new
        // system registers.
        out.truncate(self.names.len());
        for name in self.names[out.len()..].iter() {
            out.push((name.clone(), ComputeGrant::default()));
        }
        if !self.config.enabled || self.states.is_empty() {
            // The "unmanaged = empty table" contract stays: an off
            // allocator publishes nothing (AllocatorConfig::off keeps
            // v0.3 bit-identical passthrough).
            out.clear();
            return;
        }
        debug_assert_eq!(out.len(), self.names.len());
        let total: f32 = self.states.iter().map(|s| s.ema_ms.max(0.0)).sum();
        let load = if budget_ms > 0.0 {
            total / budget_ms
        } else {
            0.0
        };

        if load > 1.0 + self.config.over_tolerance {
            self.over_streak_tick(out, budget_ms, total);
        } else if total < budget_ms * self.config.relax_headroom {
            self.under_streak_tick(out, budget_ms, total);
        } else {
            // In budget, no real headroom: hold, reset both streaks —
            // the middle of the budget is where we want to stay.
            for s in &mut self.states {
                s.over_streak = 0;
                s.under_streak = 0;
            }
            self.publish(budget_ms, total, out);
        }
    }

    /// Sustained over-subscription: step every spender toward its
    /// proportional-fairness target `share / cost` (= 1/load for
    /// spenders above their floor). Idempotent — re-deriving the same
    /// target while the condition persists is stable, not compounding.
    fn over_streak_tick(&mut self, out: &mut [(String, ComputeGrant)], budget_ms: f32, total: f32) {
        let mut over = 0;
        for s in &mut self.states {
            s.over_streak += 1;
            s.under_streak = 0;
            over = over.max(s.over_streak);
        }
        if over >= self.config.squeeze_after {
            let min_share = self.config.min_share;
            let floor = self.config.floor;
            // Two passes, no intermediate Vec (#103): share_of is pure,
            // recomputing it beats allocating on the rare squeeze step.
            for s in &mut self.states {
                let share = Self::share_of(s.ema_ms, budget_ms, total, min_share);
                let target = if s.ema_ms > f32::EPSILON {
                    share / s.ema_ms
                } else {
                    1.0
                };
                s.throttle = target.clamp(floor, 1.0);
                s.over_streak = 0;
            }
        }
        self.publish(budget_ms, total, out);
    }

    /// Sustained real headroom: everyone recovers one small step.
    fn under_streak_tick(
        &mut self,
        out: &mut [(String, ComputeGrant)],
        budget_ms: f32,
        total: f32,
    ) {
        let mut under = 0;
        for s in &mut self.states {
            s.under_streak += 1;
            s.over_streak = 0;
            under = under.max(s.under_streak);
        }
        if under >= self.config.relax_after {
            for s in &mut self.states {
                s.throttle = (s.throttle + self.config.relax_step).min(1.0);
                s.under_streak = 0;
            }
        }
        self.publish(budget_ms, total, out);
    }

    /// A system's budget slice: proportional to observed cost, floored
    /// at `min_share` so nobody is starved below a survival ration.
    /// Free function so callers can use it while mutating `states`.
    fn share_of(ema_ms: f32, budget_ms: f32, total: f32, min_share: f32) -> f32 {
        let weight = if total > f32::EPSILON {
            ema_ms.max(0.0) / total
        } else {
            0.0
        };
        (budget_ms * weight).max(budget_ms * min_share)
    }

    fn publish(&self, budget_ms: f32, total: f32, out: &mut [(String, ComputeGrant)]) {
        debug_assert_eq!(out.len(), self.names.len());
        for (slot, state) in out.iter_mut().zip(self.states.iter()) {
            slot.1.share_ms = Self::share_of(state.ema_ms, budget_ms, total, self.config.min_share);
            slot.1.throttle = state.throttle.clamp(self.config.floor, 1.0);
        }
    }

    /// Snapshot of the *next* grants for introspection (shells, PC
    /// overlay, tests). Convenience wrapper around
    /// [`BudgetAllocator::allocate`] that does not advance streaks.
    pub fn peek(&self, budget_ms: f32) -> Vec<(String, ComputeGrant)> {
        let total: f32 = self.states.iter().map(|s| s.ema_ms.max(0.0)).sum();
        self.names
            .iter()
            .zip(self.states.iter())
            .map(|(name, state)| {
                (
                    name.clone(),
                    ComputeGrant {
                        share_ms: Self::share_of(
                            state.ema_ms,
                            budget_ms,
                            total,
                            self.config.min_share,
                        ),
                        throttle: state.throttle.clamp(self.config.floor, 1.0),
                    },
                )
            })
            .collect()
    }

    /// The throttle dial a system would read right now (1.0 when
    /// unmanaged). Shells use this for overlay readouts.
    pub fn throttle_of(&self, name: &str) -> f32 {
        self.names
            .iter()
            .position(|n| n == name)
            .and_then(|i| self.states.get(i))
            .map(|s| s.throttle.clamp(self.config.floor, 1.0))
            .unwrap_or(1.0)
    }

    /// Observed total cost over budget: 1.0 = exactly subscribed,
    /// above = over-subscribed, below = headroom left.
    pub fn load(&self, budget_ms: f32) -> f32 {
        let total: f32 = self.states.iter().map(|s| s.ema_ms.max(0.0)).sum();
        if budget_ms > 0.0 {
            total / budget_ms
        } else {
            0.0
        }
    }
}

impl Default for BudgetAllocator {
    fn default() -> Self {
        Self::new(AllocatorConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_common::Grants;

    fn timings(pairs: &[(&str, f32)]) -> Vec<(String, f32)> {
        pairs.iter().map(|(n, ms)| (n.to_string(), *ms)).collect()
    }

    /// Fast-rhythm config so tests do not loop 30×120 times.
    fn quick() -> AllocatorConfig {
        AllocatorConfig {
            squeeze_after: 3,
            relax_after: 5,
            ..AllocatorConfig::default()
        }
    }

    #[test]
    fn disabled_allocator_publishes_nothing() {
        let mut a = BudgetAllocator::new(AllocatorConfig::off());
        a.register("infer");
        a.observe(&timings(&[("infer", 5.0)]));
        let mut out = Vec::new();
        a.allocate(16.7, &mut out);
        assert!(out.is_empty()); // every system reads full throttle
        assert_eq!(a.throttle_of("infer"), 1.0);
    }

    #[test]
    fn shares_are_proportional_with_a_floor() {
        let mut a = BudgetAllocator::new(quick());
        a.register("physics");
        a.register("render");
        a.observe(&timings(&[("physics", 12.0), ("render", 3.0)]));
        let g = a.peek(16.7);
        // 12:3 split of 16.7 → ~13.4 / ~3.3, both above the 5% floor.
        assert!((g[0].1.share_ms - 16.7 * 0.8).abs() < 0.01);
        assert!((g[1].1.share_ms - 16.7 * 0.2).abs() < 0.01);
        assert!(g.iter().all(|(_, x)| x.throttle == 1.0)); // in budget: nobody touched

        // A near-free system still gets at least min_share.
        let mut b = BudgetAllocator::new(quick());
        b.register("infer");
        b.register("tiny");
        b.observe(&timings(&[("infer", 16.0), ("tiny", 0.001)]));
        let g = b.peek(16.7);
        assert!(g[1].1.share_ms >= 16.7 * 0.05 - 1e-6);
    }

    #[test]
    fn sustained_overload_squeezes_spenders_proportionally() {
        let mut a = BudgetAllocator::new(quick());
        a.register("hog");
        a.register("hog2");
        // Total 16 + 16 = 32ms against a 16ms budget: load = 2.
        for _ in 0..10 {
            a.observe(&timings(&[("hog", 16.0), ("hog2", 16.0)]));
            a.allocate(16.0, &mut Vec::new());
        }
        let g = a.peek(16.0);
        // Proportional fairness: both throttles ≈ share/cost = 8/16 = 0.5.
        assert!(
            (g[0].1.throttle - 0.5).abs() < 0.02,
            "got {}",
            g[0].1.throttle
        );
        assert!(
            (g[1].1.throttle - 0.5).abs() < 0.02,
            "got {}",
            g[1].1.throttle
        );
    }

    #[test]
    fn cheap_systems_keep_full_speed_under_load() {
        let mut a = BudgetAllocator::new(quick());
        a.register("hog");
        a.register("tiny");
        // 16 + 0.5 against a 16ms budget: tiny's fair target is above
        // 1.0 → clamped to full speed; the hog carries the squeeze.
        for _ in 0..10 {
            a.observe(&timings(&[("hog", 16.0), ("tiny", 0.5)]));
            a.allocate(16.0, &mut Vec::new());
        }
        let g = a.peek(16.0);
        assert!(
            (g[1].1.throttle - 1.0).abs() < 1e-6,
            "tiny should run full speed"
        );
        assert!(g[0].1.throttle < 1.0, "hog should be squeezed");
    }

    #[test]
    fn throttle_respects_the_floor() {
        let mut a = BudgetAllocator::new(quick());
        a.register("hog");
        // 200ms against a 16ms budget: the fair target 16/200 = 0.08
        // sits below the floor — the clamp must hold the line at 0.1.
        a.observe(&timings(&[("hog", 200.0)]));
        for _ in 0..10 {
            a.observe(&timings(&[("hog", 200.0)]));
            a.allocate(16.0, &mut Vec::new());
        }
        assert!((a.throttle_of("hog") - 0.1).abs() < 1e-6);
    }

    #[test]
    fn in_budget_frames_hold_and_reset_streaks() {
        let mut a = BudgetAllocator::new(AllocatorConfig {
            squeeze_after: 4,
            ..AllocatorConfig::default()
        });
        a.register("a");
        a.register("b");
        // Alternate over-subscription with in-budget cost: the
        // over-streak never reaches 4 in a row, so nothing is squeezed.
        for _ in 0..12 {
            a.observe(&timings(&[("a", 20.0), ("b", 0.0)])); // load 1.25
            a.allocate(16.0, &mut Vec::new());
            a.observe(&timings(&[("a", 5.0), ("b", 0.0)])); // mid band
            a.allocate(16.0, &mut Vec::new());
        }
        assert_eq!(a.throttle_of("a"), 1.0);
    }

    #[test]
    fn real_headroom_recovers_slowly() {
        let mut a = BudgetAllocator::new(quick());
        a.register("hog");
        // Squeeze first (load 1.25 sustained).
        for _ in 0..10 {
            a.observe(&timings(&[("hog", 20.0)]));
            a.allocate(16.0, &mut Vec::new());
        }
        let squeezed = a.throttle_of("hog");
        assert!(squeezed < 1.0);
        // Now go idle: the EMA needs a few frames to decay into the
        // headroom band, then recovery advances one small step at a
        // time — never a jump straight back to full speed.
        for _ in 0..12 {
            a.observe(&timings(&[("hog", 0.1)]));
            a.allocate(16.0, &mut Vec::new());
        }
        let recovered = a.throttle_of("hog");
        assert!(recovered > squeezed, "should be recovering");
        assert!(
            recovered - squeezed <= 0.05 + 1e-6,
            "recovery must be gradual, jumped to {recovered}"
        );
        assert!(recovered < 1.0, "one step, not all the way back");
    }

    #[test]
    fn empty_allocator_is_transparent() {
        let mut a = BudgetAllocator::default();
        let mut out = Vec::new();
        a.allocate(16.7, &mut out);
        assert!(out.is_empty());
        assert_eq!(Grants::new(&out).throttle_of("infer"), 1.0);
        assert_eq!(a.load(16.7), 0.0);
    }
}
