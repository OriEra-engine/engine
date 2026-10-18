//! L7 Inference Runtime — model boundary + command queue.
//!
//! Two services live here (book T10):
//!
//! 1. [`ModelProvider`] — the ONLY way any layer talks to a model.
//!    Applications never hold keys or URLs; providers are configured
//!    once at boot (`RuleProvider` offline, HTTP providers in platform
//!    shells / wasm kernels).
//! 2. [`InferSystem`] — the command queue executed against the L3 world
//!    each frame, with processed/rejected metrics.
//!
//! Natural-language parsing does NOT live here — that is the L9 agent's
//! job (`oriera-agent`). This layer only executes what it is given.

pub mod provider;

pub use oriera_common::TokenUsage;
pub use provider::{
    ChatMessage, InferError, ModelProvider, ProviderPreset, Role, RuleProvider, PROVIDER_PRESETS,
};

use oriera_core::{FrameCtx, System};
use oriera_ecs::{Position, Velocity, World};
use oriera_subsys::Gravity;

/// A structured engine command. Produced by the L9 agent, executed here.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Spawn `n` entities with randomized position/velocity.
    Spawn(usize),
    /// Despawn every entity.
    Clear,
    /// Set world gravity (writes the `Gravity` resource read by L6).
    SetGravity(f32),
    /// Shift every entity's position by `(dx, dy)`.
    Teleport(f32, f32),
    /// Count live entities into the [`Stats`] resource; world untouched.
    Count,
    /// Scale every velocity by `k` ("speed 2x").
    SetSpeed(f32),
    /// Unrecognized intent — counted by `rejected`, never executed.
    Unknown(String),
}

/// Singleton resource written by `Command::Count`; the app/UI layer
/// reads it to answer "how many entities are alive?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub entities: usize,
}

/// Singleton resource that accumulates model usage across every
/// cognition/director call in the world. L9 systems record into it
/// (`provider.last_usage()`); apps read one meter to render cost and
/// budget UI. Absent by default — nothing meters until someone reads
/// or seeds it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageMeter {
    pub total: TokenUsage,
    pub calls: u64,
}

impl UsageMeter {
    pub fn record(&mut self, usage: TokenUsage) {
        self.total += usage;
        self.calls += 1;
    }
}

/// How many commands one frame may execute when nothing narrower is
/// configured. Batches an AI burst across frames instead of letting a
/// flooded queue blow the frame budget in one tick.
pub const DEFAULT_FRAME_BUDGET: usize = 32;

/// Inference runtime: drains a command queue each frame.
///
/// Compute-budget aware: the effective per-frame command budget is the
/// configured `frame_budget` scaled by the L2 allocator's throttle
/// (`ctx.grants`), so an over-subscribed frame automatically executes
/// fewer commands instead of blowing the budget — never fewer than one,
/// the queue can slow but never starve.
pub struct InferSystem {
    queue: Vec<Command>,
    pub processed: u64,
    pub rejected: u64,
    /// Deferral events: how many times a command had to wait out a
    /// frame-budget overrun. A command that survives 3 budget-capped
    /// frames counts 3 here — it is "how long did the queue stall",
    /// not "how many commands waited". For queue depth see
    /// [`InferSystem::pending`] and [`InferSystem::max_pending`].
    pub deferred: u64,
    /// Commands merged away by [`InferSystem::submit`] coalescing
    /// (e.g. two adjacent spawns becoming one). Each merge is
    /// provably world- and RNG-identical — this counter is the
    /// "waste avoided" line in the metrics readout.
    pub coalesced: u64,
    /// High-water mark of queue depth, in commands: the peak size the
    /// queue ever reached (after any submit burst, after any drain).
    /// The one number to answer "how bad was the flood".
    pub max_pending: usize,
    /// Max commands executed per frame (before throttle scaling).
    frame_budget: usize,
    /// Deterministic spawn RNG state (LCG) — replayable (book T5).
    seed: u64,
}

impl InferSystem {
    pub fn new() -> Self {
        Self {
            queue: Vec::new(),
            processed: 0,
            rejected: 0,
            deferred: 0,
            coalesced: 0,
            max_pending: 0,
            frame_budget: DEFAULT_FRAME_BUDGET,
            seed: 0x9E3779B97F4A7C15,
        }
    }

    /// Cap per-frame command execution (e.g. `4` on a slow device, or
    /// `1` when each command kicks off model work). The runtime budget
    /// may still shrink below this under allocator pressure.
    pub fn with_frame_budget(n: usize) -> Self {
        Self {
            frame_budget: n.max(1),
            ..Self::new()
        }
    }

    /// Submit a command for execution on the next frame.
    ///
    /// Coalesces against the queue tail when the merge is *provably*
    /// identical in world outcome and RNG stream — cheap flood control
    /// that runs before the queue ever reaches a frame:
    ///
    /// - `Spawn(m)` + `Spawn(n)` → `Spawn(m + n)`: one loop of `m + n`
    ///   entities draws the same LCG sequence as two loops of `m`, `n`.
    /// - `SetGravity(a)` + `SetGravity(b)` → `SetGravity(b)`: a plain
    ///   resource overwrite; the earlier value is unobservable.
    /// - `Count` + `Count` → `Count`: both read the world at execution
    ///   time, which is identical either way.
    ///
    /// Deliberately NOT merged: `Teleport`/`SetSpeed` pairs — floating
    /// point addition and multiplication are not associative, so
    /// `(x + dx1) + dx2` can differ from `x + (dx1 + dx2)` in the last
    /// bit, and anything that changes observable floats is not waste
    /// removal, it is a behavior change.
    pub fn submit(&mut self, cmd: Command) {
        if let Some(tail) = self.queue.last_mut() {
            let merged = match (tail, &cmd) {
                (Command::Spawn(m), Command::Spawn(n)) => {
                    *m += *n;
                    Some(())
                }
                (Command::SetGravity(prev), Command::SetGravity(new)) => {
                    *prev = *new;
                    Some(())
                }
                (Command::Count, Command::Count) => Some(()),
                _ => None,
            };
            if merged.is_some() {
                self.coalesced += 1;
                return;
            }
        }
        self.queue.push(cmd);
        self.max_pending = self.max_pending.max(self.queue.len());
    }

    /// Commands still waiting in the queue right now.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Simple deterministic LCG so replays reproduce identical spawns.
    fn rand(&mut self) -> u64 {
        self.seed = self
            .seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.seed
    }
}

impl System<World> for InferSystem {
    fn name(&self) -> &str {
        "infer"
    }
    fn run(&mut self, ctx: &FrameCtx, world: &mut World) {
        // The allocator's dial for this frame: budget × throttle, never
        // below one command. Unmanaged (allocator off / not registered)
        // reads throttle 1.0 and behaves exactly like the static cap.
        let limit = ctx
            .grants
            .get(self.name())
            .scale_usize(self.frame_budget, 1)
            .min(self.queue.len());
        self.deferred += (self.queue.len() - limit) as u64;
        let mut batch: Vec<Command> = self.queue.drain(..limit).collect();
        for cmd in batch.drain(..) {
            match cmd {
                Command::Spawn(n) => {
                    for _ in 0..n {
                        let e = world.spawn();
                        let r1 = self.rand();
                        let r2 = self.rand();
                        world.insert(
                            e,
                            Position {
                                x: (r1 % 800) as f32,
                                y: (r2 % 600) as f32,
                            },
                        );
                        world.insert(
                            e,
                            Velocity {
                                vx: ((r1 % 200) as f32 - 100.0) * 0.01,
                                vy: ((r2 % 200) as f32 - 100.0) * 0.01,
                            },
                        );
                    }
                    self.processed += 1;
                }
                Command::Clear => {
                    world.clear();
                    self.processed += 1;
                }
                Command::SetGravity(g) => {
                    world.insert_resource(Gravity(g));
                    self.processed += 1;
                }
                Command::Teleport(dx, dy) => {
                    world.for_each_mut::<Position>(|_e, p| {
                        p.x += dx;
                        p.y += dy;
                    });
                    self.processed += 1;
                }
                Command::Count => {
                    let entities = world.len();
                    world.insert_resource(Stats { entities });
                    self.processed += 1;
                }
                Command::SetSpeed(k) => {
                    world.for_each_mut::<Velocity>(|_e, v| {
                        v.vx *= k;
                        v.vy *= k;
                    });
                    self.processed += 1;
                }
                Command::Unknown(_) => {
                    self.rejected += 1;
                }
            }
        }
    }
}

impl Default for InferSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_common::{ComputeGrant, FrameIndex, Grants, Timestamp};

    fn ctx() -> FrameCtx<'static> {
        FrameCtx {
            frame: FrameIndex(0),
            dt: 0.0,
            now: Timestamp(0),
            grants: Grants::EMPTY,
        }
    }

    /// A context where `infer` is managed at the given throttle. The
    /// one-entry table is leaked: test-only, bounded by the number of
    /// distinct throttles exercised (a handful).
    fn throttled(t: f32) -> FrameCtx<'static> {
        let entries = Box::leak(
            vec![(
                "infer".to_string(),
                ComputeGrant {
                    share_ms: 1.0,
                    throttle: t,
                },
            )]
            .into_boxed_slice(),
        );
        FrameCtx {
            frame: FrameIndex(0),
            dt: 0.0,
            now: Timestamp(0),
            grants: Grants::new(entries),
        }
    }

    #[test]
    fn spawn_command_creates_entities() {
        let mut world = World::new();
        let mut sys = InferSystem::new();
        sys.submit(Command::Spawn(50));
        sys.run(&ctx(), &mut world);
        assert_eq!(world.len(), 50);
        assert_eq!(world.query::<Position>().count(), 50);
        assert_eq!(sys.processed, 1);
        assert!(sys.queue.is_empty());
    }

    #[test]
    fn clear_and_gravity_commands_touch_the_world() {
        let mut world = World::new();
        let mut sys = InferSystem::new();
        sys.submit(Command::Spawn(10));
        sys.run(&ctx(), &mut world);
        assert_eq!(world.len(), 10);

        sys.submit(Command::Clear);
        sys.submit(Command::SetGravity(9.8));
        sys.run(&ctx(), &mut world);
        assert!(world.is_empty());
        assert_eq!(world.resource::<Gravity>().map(|g| g.0), Some(9.8));
        assert_eq!(sys.processed, 3);
    }

    #[test]
    fn unknown_commands_are_rejected_not_executed() {
        let mut world = World::new();
        let mut sys = InferSystem::new();
        sys.submit(Command::Unknown("make me a sandwich".into()));
        sys.run(&ctx(), &mut world);
        assert_eq!(world.len(), 0);
        assert_eq!(sys.processed, 0);
        assert_eq!(sys.rejected, 1);
    }

    #[test]
    fn spawns_are_deterministic() {
        let run = || {
            let mut world = World::new();
            let mut sys = InferSystem::new();
            sys.submit(Command::Spawn(5));
            sys.run(&ctx(), &mut world);
            world
                .query::<Position>()
                .map(|(_e, p)| (p.x, p.y))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn teleport_shifts_every_position() {
        let mut world = World::new();
        let e1 = world.spawn();
        world.insert(e1, Position { x: 10.0, y: 20.0 });
        let e2 = world.spawn();
        world.insert(e2, Position { x: 0.0, y: 0.0 });

        let mut sys = InferSystem::new();
        sys.submit(Command::Teleport(5.0, -2.0));
        sys.run(&ctx(), &mut world);

        assert_eq!(world.get::<Position>(e1).unwrap().x, 15.0);
        assert_eq!(world.get::<Position>(e1).unwrap().y, 18.0);
        assert_eq!(world.get::<Position>(e2).unwrap().x, 5.0);
    }

    #[test]
    fn count_writes_stats_without_touching_entities() {
        let mut world = World::new();
        let mut sys = InferSystem::new();
        sys.submit(Command::Spawn(7));
        sys.run(&ctx(), &mut world);

        sys.submit(Command::Count);
        sys.run(&ctx(), &mut world);
        assert_eq!(world.resource::<Stats>().map(|s| s.entities), Some(7));
        assert_eq!(world.len(), 7); // unchanged
    }

    #[test]
    fn set_speed_scales_velocities() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Velocity { vx: 1.0, vy: -3.0 });

        let mut sys = InferSystem::new();
        sys.submit(Command::SetSpeed(2.0));
        sys.run(&ctx(), &mut world);

        let v = world.get::<Velocity>(e).unwrap();
        assert_eq!(v.vx, 2.0);
        assert_eq!(v.vy, -6.0);
    }

    #[test]
    fn frame_budget_spreads_a_command_flood() {
        let mut world = World::new();
        let mut sys = InferSystem::with_frame_budget(8);
        // Alternate kinds so nothing coalesces — this test measures the
        // frame budget in isolation (coalescing has its own tests).
        for i in 0..10 {
            sys.submit(Command::Spawn(1));
            sys.submit(Command::SetGravity(1.0 + i as f32));
        }
        assert_eq!(sys.pending(), 20);
        assert_eq!(sys.max_pending, 20); // high-water mark of the burst
        assert_eq!(sys.coalesced, 0);

        sys.run(&ctx(), &mut world);
        assert_eq!(sys.processed, 8);
        assert_eq!(sys.deferred, 12); // 12 deferral events this frame
        assert_eq!(world.len(), 4); // 4 spawns in the first 8 commands
        assert_eq!(sys.pending(), 12);

        // Subsequent frames drain the rest.
        sys.run(&ctx(), &mut world);
        sys.run(&ctx(), &mut world);
        assert_eq!(sys.processed, 20);
        assert_eq!(sys.pending(), 0);
        assert_eq!(world.len(), 10);
        // Frame 2 deferred 4 more (12 leftovers, budget 8): 12+4 events.
        assert_eq!(sys.deferred, 16);
        // High-water mark survives the drain — it is a max, not a level.
        assert_eq!(sys.max_pending, 20);
    }

    #[test]
    fn default_budget_covers_typical_bursts() {
        let mut world = World::new();
        let mut sys = InferSystem::new();
        // Mixed kinds: a same-kind burst would coalesce (by design) and
        // this test would measure the merge, not the budget.
        for i in 0..DEFAULT_FRAME_BUDGET {
            sys.submit(if i % 2 == 0 {
                Command::Count
            } else {
                Command::SetGravity(i as f32)
            });
        }
        sys.run(&ctx(), &mut world);
        assert_eq!(sys.processed, DEFAULT_FRAME_BUDGET as u64);
        assert_eq!(sys.deferred, 0);
        assert_eq!(sys.coalesced, 0);
    }

    #[test]
    fn usage_meter_accumulates_across_calls() {
        let mut meter = UsageMeter::default();
        let p = RuleProvider::new();
        p.complete(&[ChatMessage::user("聊聊咖啡豆")]).unwrap();
        meter.record(p.last_usage());
        p.complete(&[ChatMessage::user("引擎跑通了吗")]).unwrap();
        meter.record(p.last_usage());
        assert_eq!(meter.calls, 2);
        assert!(meter.total.total() > 0);
    }

    #[test]
    fn allocator_throttle_shrinks_the_runtime_budget() {
        let mut world = World::new();
        let mut sys = InferSystem::with_frame_budget(8);
        for i in 0..4 {
            sys.submit(Command::Spawn(1));
            sys.submit(Command::SetGravity(1.0 + i as f32));
        }
        // Half throttle: 8 submitted × 0.5 → 4 run this frame.
        sys.run(&throttled(0.5), &mut world);
        assert_eq!(sys.processed, 4);
        assert_eq!(sys.pending(), 4);

        // Full throttle: back to the configured cap.
        for i in 0..4 {
            sys.submit(Command::Spawn(1));
            sys.submit(Command::SetGravity(10.0 + i as f32));
        }
        sys.run(&ctx(), &mut world);
        assert_eq!(sys.processed, 12); // 4 + 8
    }

    #[test]
    fn throttled_queue_never_starves_below_one_command() {
        let mut world = World::new();
        let mut sys = InferSystem::with_frame_budget(8);
        for i in 0..5 {
            sys.submit(if i % 2 == 0 {
                Command::Count
            } else {
                Command::SetGravity(i as f32)
            });
        }
        // Even a near-zero throttle keeps the queue moving.
        sys.run(&throttled(0.0), &mut world);
        assert_eq!(sys.processed, 1);
        assert_eq!(sys.pending(), 4);
    }

    #[test]
    fn adjacent_spawns_coalesce_bit_identically() {
        // World outcome AND the LCG stream must be identical to running
        // the commands unmerged — proven by direct comparison against
        // a system that never coalesces (commands interleaved).
        let run = |coalesced: bool| {
            let mut world = World::new();
            let mut sys = InferSystem::new();
            sys.submit(Command::Spawn(5));
            if !coalesced {
                sys.run(&ctx(), &mut world); // force execution between submits
            }
            sys.submit(Command::Spawn(3));
            sys.run(&ctx(), &mut world);
            if coalesced {
                sys.run(&ctx(), &mut world);
            }
            (
                world
                    .query::<Position>()
                    .map(|(_e, p)| (p.x.to_bits(), p.y.to_bits()))
                    .collect::<Vec<_>>(),
                sys.processed,
            )
        };
        let (merged_world, _) = run(true);
        let (plain_world, _) = run(false);
        assert_eq!(merged_world.len(), 8);
        assert_eq!(merged_world, plain_world); // bit-identical entities
    }

    #[test]
    fn duplicate_gravity_and_count_coalesce() {
        let mut world = World::new();
        let mut sys = InferSystem::new();
        sys.submit(Command::SetGravity(9.8));
        sys.submit(Command::SetGravity(1.6)); // overwrites before execution
        sys.submit(Command::Count);
        sys.submit(Command::Count); // second count is pure waste
        assert_eq!(sys.pending(), 2); // 4 submissions → 2 commands
        assert_eq!(sys.coalesced, 2);
        sys.run(&ctx(), &mut world);
        assert_eq!(world.resource::<Gravity>().map(|g| g.0), Some(1.6));
        assert!(world.resource::<Stats>().is_some());
    }

    #[test]
    fn float_sensitive_commands_are_never_merged() {
        let mut sys = InferSystem::new();
        sys.submit(Command::Teleport(0.1, 0.0));
        sys.submit(Command::Teleport(0.2, 0.0));
        sys.submit(Command::SetSpeed(2.0));
        sys.submit(Command::SetSpeed(2.0)); // *4, not idempotent
        assert_eq!(sys.pending(), 4);
        assert_eq!(sys.coalesced, 0);
    }

    #[test]
    fn different_command_kinds_do_not_coalesce_across_the_tail() {
        let mut sys = InferSystem::new();
        sys.submit(Command::Spawn(5));
        sys.submit(Command::Count); // must run AFTER the spawn lands
        sys.submit(Command::Spawn(2)); // tail is Count, not Spawn
        assert_eq!(sys.pending(), 3);
        assert_eq!(sys.coalesced, 0);
    }
}
