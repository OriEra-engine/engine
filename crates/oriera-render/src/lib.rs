//! L5 Render — render graph and frame submission.
//!
//! v0.1 counts visible `Position` components as a stand-in for a real
//! render graph (book T8), plus the `SetQuality` half of the L5
//! contract: an adaptive quality governor that watches frame times and
//! moves batching up or down with hysteresis, so a cognition-heavy
//! frame degrades to fewer draw calls instead of dropping frames. The
//! world comes from the L2 scheduler; render depends only on layers
//! below it (L2 core, L3 object model) — never on L6 subsystems.

use oriera_core::{FrameCtx, System};
use oriera_ecs::{Position, World};

/// Render quality tier. Drives the batching factor applied to visible
/// geometry — the cheapest dial a frame-graph has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    Low,
    Medium,
    High,
}

impl Quality {
    /// How many visible objects one batched draw call covers at this
    /// tier. High = no extra batching; Low = aggressive.
    fn batch_factor(self) -> u32 {
        match self {
            Quality::High => 1,
            Quality::Medium => 4,
            Quality::Low => 16,
        }
    }

    fn step_down(self) -> Option<Quality> {
        match self {
            Quality::High => Some(Quality::Medium),
            Quality::Medium => Some(Quality::Low),
            Quality::Low => None,
        }
    }

    fn step_up(self) -> Option<Quality> {
        match self {
            Quality::Low => Some(Quality::Medium),
            Quality::Medium => Some(Quality::High),
            Quality::High => None,
        }
    }
}

/// Consecutive over/under-budget frames the governor waits before
/// switching tiers. Degrade fast (AI burst), recover slow (no flapping).
const DEGRADE_AFTER: u32 = 30;
const RECOVER_AFTER: u32 = 120;
/// Avg must sit below this fraction of budget before recovering.
const RECOVER_HEADROOM: f32 = 0.6;

/// Frame submission system: cull + count what would be drawn, then
/// batch according to the current [`Quality`].
pub struct RenderSystem {
    pub draw_calls: u32,
    pub visible: u32,
    /// Current tier; starts at High and moves only through
    /// [`RenderSystem::observe_frame`].
    pub quality: Quality,
    over_streak: u32,
    under_streak: u32,
}

impl RenderSystem {
    pub fn new() -> Self {
        Self {
            draw_calls: 0,
            visible: 0,
            quality: Quality::High,
            over_streak: 0,
            under_streak: 0,
        }
    }

    /// Feed the governor one frame's average time (from
    /// `Scheduler::avg_frame_ms`) and the budget it is graded against.
    /// Sustained overruns step quality down; sustained runs with real
    /// headroom step it back up. Everything between resets both
    /// streaks — the middle of the budget is where we want to stay.
    pub fn observe_frame(&mut self, avg_ms: f32, budget_ms: f32) {
        if avg_ms > budget_ms {
            self.over_streak += 1;
            self.under_streak = 0;
            if self.over_streak >= DEGRADE_AFTER {
                if let Some(q) = self.quality.step_down() {
                    tracing::info!(from = ?self.quality, to = ?q, "L5 quality degrade");
                    self.quality = q;
                }
                self.over_streak = 0;
            }
        } else if avg_ms < budget_ms * RECOVER_HEADROOM {
            self.under_streak += 1;
            self.over_streak = 0;
            if self.under_streak >= RECOVER_AFTER {
                if let Some(q) = self.quality.step_up() {
                    tracing::info!(from = ?self.quality, to = ?q, "L5 quality recover");
                    self.quality = q;
                }
                self.under_streak = 0;
            }
        } else {
            self.over_streak = 0;
            self.under_streak = 0;
        }
    }
}

impl System<World> for RenderSystem {
    fn name(&self) -> &str {
        "render"
    }
    fn run(&mut self, _ctx: &FrameCtx, world: &mut World) {
        self.visible = world.query::<Position>().count() as u32;
        let factor = self.quality.batch_factor();
        self.draw_calls = self.visible.div_ceil(factor).max(1);
    }
}

impl Default for RenderSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_common::FrameIndex;

    #[test]
    fn counts_visible_positions() {
        let mut world = World::new();
        for i in 0..5 {
            let e = world.spawn();
            world.insert(
                e,
                Position {
                    x: i as f32,
                    y: 0.0,
                },
            );
        }
        let ctx = FrameCtx {
            frame: FrameIndex(0),
            dt: 0.0,
            now: oriera_common::Timestamp(0),
            grants: oriera_common::Grants::EMPTY,
        };
        let mut sys = RenderSystem::new();
        sys.run(&ctx, &mut world);
        assert_eq!(sys.visible, 5);
        assert_eq!(sys.draw_calls, 5); // High tier: factor 1
    }

    #[test]
    fn batching_reduces_draw_calls_at_lower_tiers() {
        let mut world = World::new();
        for i in 0..32 {
            let e = world.spawn();
            world.insert(
                e,
                Position {
                    x: i as f32,
                    y: 0.0,
                },
            );
        }
        let ctx = FrameCtx {
            frame: FrameIndex(0),
            dt: 0.0,
            now: oriera_common::Timestamp(0),
            grants: oriera_common::Grants::EMPTY,
        };
        let mut sys = RenderSystem::new();

        sys.quality = Quality::Medium;
        sys.run(&ctx, &mut world);
        assert_eq!(sys.visible, 32);
        assert_eq!(sys.draw_calls, 8); // 32 / 4

        sys.quality = Quality::Low;
        sys.run(&ctx, &mut world);
        assert_eq!(sys.draw_calls, 2); // 32 / 16
    }

    #[test]
    fn sustained_overruns_degrade_quality() {
        let mut sys = RenderSystem::new();
        for _ in 0..DEGRADE_AFTER {
            sys.observe_frame(20.0, 16.7);
        }
        assert_eq!(sys.quality, Quality::Medium);
        for _ in 0..DEGRADE_AFTER {
            sys.observe_frame(20.0, 16.7);
        }
        assert_eq!(sys.quality, Quality::Low);
        // Already at Low: stays, no panic, streak keeps cycling.
        for _ in 0..DEGRADE_AFTER {
            sys.observe_frame(20.0, 16.7);
        }
        assert_eq!(sys.quality, Quality::Low);
    }

    #[test]
    fn in_budget_frames_reset_the_degrade_streak() {
        let mut sys = RenderSystem::new();
        for _ in 0..(DEGRADE_AFTER - 1) {
            sys.observe_frame(20.0, 16.7);
        }
        sys.observe_frame(10.0, 16.7); // inside budget, above headroom
        for _ in 0..(DEGRADE_AFTER - 1) {
            sys.observe_frame(20.0, 16.7);
        }
        assert_eq!(sys.quality, Quality::High); // never accumulated a full streak
    }

    #[test]
    fn recovery_needs_headroom_and_patience() {
        let mut sys = RenderSystem::new();
        sys.quality = Quality::Low;
        // Fast but above headroom: no recovery.
        for _ in 0..RECOVER_AFTER {
            sys.observe_frame(11.0, 16.7);
        }
        assert_eq!(sys.quality, Quality::Low);
        // Real headroom for long enough: step up once, not straight to High.
        for _ in 0..RECOVER_AFTER {
            sys.observe_frame(5.0, 16.7);
        }
        assert_eq!(sys.quality, Quality::Medium);
        // Over-budget frames immediately reset the recovery streak.
        for _ in 0..(RECOVER_AFTER - 1) {
            sys.observe_frame(5.0, 16.7);
        }
        sys.observe_frame(20.0, 16.7);
        for _ in 0..(RECOVER_AFTER - 1) {
            sys.observe_frame(5.0, 16.7);
        }
        assert_eq!(sys.quality, Quality::Medium); // never reached the threshold
    }
}
