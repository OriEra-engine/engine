//! L6 Subsystems — physics, animation, audio, UI, net sync.
//!
//! v0.1 implements the physics subsystem only: fixed-step integration of
//! `Position`/`Velocity` (L3 components) with bounding-box bounce. The
//! world is passed in by the L2 scheduler each frame — subsystems never
//! reach into globals (book T3 §3.8.1).

#[cfg(feature = "rigid")]
pub mod rigid;
#[cfg(feature = "rigid")]
pub use rigid::RigidBodySystem;

#[cfg(feature = "rigid3d")]
pub mod rigid3d;
#[cfg(feature = "rigid3d")]
pub use rigid3d::{
    sim_fingerprint, ArmDescription, JointTargets, JointTelemetry, MultibodySystem, Pose3, Scene3D,
    SimObject, GRASP_RADIUS, JOINT_NAMES, PROP_HALF,
};

use oriera_core::{FrameCtx, System};
use oriera_ecs::{Position, Velocity, World};

/// Singleton resource: downward acceleration in units/s².
/// The inference layer (L7) writes it; physics (L6) reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gravity(pub f32);

/// Singleton resource: simulation pacing multiplier. `1.0` is real
/// time, `0.0` freezes the world, `2.0` is double speed. Physics
/// scales its integration step by this value (clamped to
/// `[0, MAX_TIME_SCALE]` — a hallucinated `1e9` from a model must not
/// explode the world). Written by whoever owns pacing: the app, a
/// cutscene, or an agent ("bullet time" while the AI director frames
/// a shot).
///
/// **Subsystem contract:** consume pacing through
/// [`TimeScale::effective`], never `ts.0` — the clamp must live in
/// exactly one place as animation/audio/net-sync subsystems land.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeScale(pub f32);

impl Default for TimeScale {
    fn default() -> Self {
        Self(1.0)
    }
}

impl TimeScale {
    /// The value every pacing consumer must use: the raw multiplier
    /// clamped to `[0, MAX_TIME_SCALE]` (negative → frozen).
    pub fn effective(self) -> f32 {
        self.0.clamp(0.0, MAX_TIME_SCALE)
    }
}

/// Upper clamp for [`TimeScale`]; see [`TimeScale::effective`].
pub const MAX_TIME_SCALE: f32 = 8.0;

/// Fixed-step particle physics: integrate velocity, apply gravity,
/// bounce off `bounds`. Runs as an L2-scheduled system.
pub struct PhysicsSystem {
    pub bounds: (f32, f32),
    /// Frames whose integration pass was skipped as a provable no-op
    /// (frozen time, or an exactly-still world). The compute-budget
    /// readout line for "work NOT spent" — a world waiting on the AI
    /// pays nothing for physics.
    pub idle_frames: u64,
}

impl PhysicsSystem {
    pub fn new(w: f32, h: f32) -> Self {
        Self {
            bounds: (w, h),
            idle_frames: 0,
        }
    }
}

impl System<World> for PhysicsSystem {
    fn name(&self) -> &str {
        "physics"
    }
    fn run(&mut self, ctx: &FrameCtx, world: &mut World) {
        let gravity = world.resource::<Gravity>().map(|g| g.0).unwrap_or(0.0);
        let scale = world
            .resource::<TimeScale>()
            .map(|t| t.effective())
            .unwrap_or(1.0);
        let dt = ctx.dt * scale;

        // Compute-budget idle skip (v0.4): when integration provably
        // cannot change anything, do not walk the columns at all. The
        // pass is a no-op exactly when both hold —
        // (1) every integrated entity sits inside the bounds, so the
        //     clamp arms cannot fire (integration clamps strays home
        //     even at zero velocity — skipping over them would NOT be
        //     a no-op; review #52);
        // (2) every delta is exactly zero: frozen time (`dt == 0`),
        //     or zero gravity with every velocity exactly 0.0.
        // The scan is read-only over the same pairs the write pass
        // would touch and bails as soon as neither skip condition can
        // hold anymore. Anything that merely *looks* idle (tiny
        // velocities) still integrates — approximation is the L2
        // allocator's business, never the physics pass's.
        if dt == 0.0 || gravity == 0.0 {
            let (bw, bh) = self.bounds;
            let mut all_inside = true;
            let mut all_still = true;
            for (_e, p, v) in world.query2::<Position, Velocity>() {
                let inside = p.x >= 0.0 && p.x <= bw && p.y >= 0.0 && p.y <= bh;
                let still = v.vx == 0.0 && v.vy == 0.0;
                all_inside &= inside;
                all_still &= still;
                if !all_inside && !all_still {
                    break; // neither skip branch can hold anymore
                }
            }
            if all_inside && (dt == 0.0 || all_still) {
                self.idle_frames += 1;
                return;
            }
        }
        let (bw, bh) = self.bounds;
        world.for_each2_mut::<Position, Velocity>(|_e, p, v| {
            p.x += v.vx * dt;
            p.y += v.vy * dt;
            v.vy += gravity * dt;
            if p.x < 0.0 {
                p.x = 0.0;
                v.vx = v.vx.abs();
            }
            if p.x > bw {
                p.x = bw;
                v.vx = -v.vx.abs();
            }
            if p.y < 0.0 {
                p.y = 0.0;
                v.vy = v.vy.abs();
            }
            if p.y > bh {
                p.y = bh;
                v.vy = -v.vy.abs();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_common::FrameIndex;
    use oriera_ecs::Entity;

    fn ctx(dt: f32) -> FrameCtx<'static> {
        FrameCtx {
            frame: FrameIndex(0),
            dt,
            now: oriera_common::Timestamp(0),
            grants: oriera_common::Grants::EMPTY,
        }
    }

    #[test]
    fn integrates_velocity_and_bounces() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 5.0, y: 5.0 });
        world.insert(e, Velocity { vx: 10.0, vy: 0.0 });

        let mut sys = PhysicsSystem::new(100.0, 100.0);
        sys.run(&ctx(1.0), &mut world);
        assert_eq!(world.get::<Position>(e).unwrap().x, 15.0);

        // Drive it into the right wall twice — velocity must flip.
        sys.run(&ctx(20.0), &mut world);
        assert_eq!(world.get::<Position>(e).unwrap().x, 100.0);
        assert!(world.get::<Velocity>(e).unwrap().vx < 0.0);
    }

    #[test]
    fn gravity_resource_drives_acceleration() {
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        let e: Entity = world.spawn();
        world.insert(e, Position { x: 50.0, y: 90.0 });
        world.insert(e, Velocity { vx: 0.0, vy: 0.0 });

        let mut sys = PhysicsSystem::new(100.0, 100.0);
        sys.run(&ctx(1.0), &mut world);
        // Integration order: position uses the pre-update velocity.
        assert!((world.get::<Velocity>(e).unwrap().vy - 9.8).abs() < 1e-6);
        assert_eq!(world.get::<Position>(e).unwrap().y, 90.0);
        sys.run(&ctx(1.0), &mut world);
        assert!(world.get::<Position>(e).unwrap().y > 90.0);

        // No resource → zero gravity.
        let mut world2 = World::new();
        let e2 = world2.spawn();
        world2.insert(e2, Position { x: 50.0, y: 90.0 });
        world2.insert(e2, Velocity { vx: 0.0, vy: 0.0 });
        sys.run(&ctx(1.0), &mut world2);
        assert_eq!(world2.get::<Velocity>(e2).unwrap().vy, 0.0);
    }

    #[test]
    fn time_scale_paces_the_simulation() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 50.0, y: 50.0 });
        world.insert(e, Velocity { vx: 10.0, vy: 0.0 });

        let mut sys = PhysicsSystem::new(100.0, 100.0);
        sys.run(&ctx(1.0), &mut world);
        assert_eq!(world.get::<Position>(e).unwrap().x, 60.0); // default 1.0x

        world.insert_resource(TimeScale(2.0));
        sys.run(&ctx(1.0), &mut world);
        assert_eq!(world.get::<Position>(e).unwrap().x, 80.0); // double step

        world.insert_resource(TimeScale(0.0));
        sys.run(&ctx(1.0), &mut world);
        assert_eq!(world.get::<Position>(e).unwrap().x, 80.0); // frozen
    }

    #[test]
    fn absurd_time_scale_is_clamped_not_fatal() {
        let mut world = World::new();
        world.insert_resource(TimeScale(1.0e9)); // a hallucinated value
        let e = world.spawn();
        world.insert(e, Position { x: 0.0, y: 0.0 });
        world.insert(e, Velocity { vx: 1.0, vy: 0.0 });

        let mut sys = PhysicsSystem::new(100.0, 100.0);
        sys.run(&ctx(1.0), &mut world);
        // 1 * 8 = capped step, entity clamped at bounds — no NaN, no panic.
        let p = world.get::<Position>(e).unwrap();
        assert!(p.x.is_finite() && p.x <= 100.0);
    }

    #[test]
    fn effective_is_the_single_clamp_entry_point() {
        assert_eq!(TimeScale(2.0).effective(), 2.0);
        assert_eq!(TimeScale(0.0).effective(), 0.0);
        assert_eq!(TimeScale(-3.0).effective(), 0.0); // negative → frozen
        assert_eq!(TimeScale(1.0e9).effective(), MAX_TIME_SCALE);
        assert_eq!(TimeScale::default().effective(), 1.0);
    }

    #[test]
    fn frozen_time_skips_the_pass_bit_identically() {
        let build = || {
            let mut w = World::new();
            w.insert_resource(Gravity(9.8));
            for i in 0..10 {
                let e = w.spawn();
                w.insert(
                    e,
                    Position {
                        x: i as f32 * 7.0,
                        y: 13.0,
                    },
                );
                w.insert(e, Velocity { vx: 1.5, vy: -0.25 });
            }
            w
        };
        // Frozen world: dt·scale = 0 → integration is an exact no-op, so
        // five skipped frames must equal a world that never integrated
        // at all, bit for bit.
        let mut frozen = build();
        frozen.insert_resource(TimeScale(0.0));
        let mut sys = PhysicsSystem::new(100.0, 100.0);
        for _ in 0..5 {
            sys.run(&ctx(1.0), &mut frozen);
        }
        assert_eq!(sys.idle_frames, 5);
        let fresh = build();
        let a: Vec<(u32, u32)> = frozen
            .query::<Position>()
            .map(|(_e, p)| (p.x.to_bits(), p.y.to_bits()))
            .collect();
        let b: Vec<(u32, u32)> = fresh
            .query::<Position>()
            .map(|(_e, p)| (p.x.to_bits(), p.y.to_bits()))
            .collect();
        assert_eq!(a, b);

        // The same provable no-op via a zero-dt context (e.g. the very
        // first frame before the clock has a predecessor).
        let mut zero_dt = build();
        let mut sys2 = PhysicsSystem::new(100.0, 100.0);
        sys2.run(&ctx(0.0), &mut zero_dt);
        assert_eq!(sys2.idle_frames, 1);
    }

    #[test]
    fn out_of_bounds_entities_are_clamped_even_when_still() {
        // REVIEW #52: the skip predicate must not fire while anything
        // sits outside the bounds — integration clamps those home even
        // with zero velocity, so skipping would NOT be a no-op.
        let mut w = World::new();
        let outside = w.spawn();
        w.insert(outside, Position { x: -5.0, y: 200.0 });
        w.insert(outside, Velocity { vx: 0.0, vy: 0.0 });
        let inside = w.spawn();
        w.insert(inside, Position { x: 50.0, y: 50.0 });
        w.insert(inside, Velocity { vx: 0.0, vy: 0.0 });

        let mut sys = PhysicsSystem::new(100.0, 100.0);
        sys.run(&ctx(1.0), &mut w);
        // Integration (no skip): clamp both coordinates of the outside
        // entity into the bounds; the inside entity is untouched.
        let p = w.get::<Position>(outside).unwrap();
        assert_eq!(p.x, 0.0);
        assert_eq!(p.y, 100.0);
        assert_eq!(w.get::<Position>(inside).unwrap().x, 50.0);
        assert_eq!(
            sys.idle_frames, 0,
            "still world, but NOT skippable while clamps have work"
        );
    }

    #[test]
    fn perfectly_still_world_skips_but_gravity_wakes_it() {
        let mut w = World::new();
        for i in 0..4 {
            let e = w.spawn();
            w.insert(
                e,
                Position {
                    x: i as f32,
                    y: 50.0,
                },
            );
            w.insert(e, Velocity { vx: 0.0, vy: 0.0 });
        }
        let mut sys = PhysicsSystem::new(100.0, 100.0);

        // No gravity resource → zero gravity: still world, skip.
        sys.run(&ctx(1.0), &mut w);
        assert_eq!(sys.idle_frames, 1);
        let before: Vec<(u32, u32)> = w
            .query::<Position>()
            .map(|(_e, p)| (p.x.to_bits(), p.y.to_bits()))
            .collect();

        // Gravity appears: zero velocities would still accelerate —
        // must NOT skip (the skip only holds when the pass is a no-op).
        // Integration order moves position by the *pre-update*
        // velocity, so the first gravity frame changes velocities, not
        // positions yet.
        w.insert_resource(Gravity(9.8));
        sys.run(&ctx(1.0), &mut w);
        assert_eq!(sys.idle_frames, 1); // unchanged: it ran
        let vels: Vec<f32> = w.query::<Velocity>().map(|(_e, v)| v.vy).collect();
        assert!(vels.iter().all(|vy| *vy > 0.0)); // falling now
        sys.run(&ctx(1.0), &mut w);
        let after: Vec<(u32, u32)> = w
            .query::<Position>()
            .map(|(_e, p)| (p.x.to_bits(), p.y.to_bits()))
            .collect();
        assert_ne!(before, after); // and by frame two, positions moved

        // A single moving entity anywhere cancels the still-world skip.
        let mut w2 = World::new();
        for i in 0..4 {
            let e = w2.spawn();
            w2.insert(
                e,
                Position {
                    x: i as f32,
                    y: 50.0,
                },
            );
            w2.insert(e, Velocity { vx: 0.0, vy: 0.0 });
        }
        let mover = w2.spawn();
        w2.insert(mover, Position { x: 0.0, y: 0.0 });
        w2.insert(mover, Velocity { vx: 5.0, vy: 0.0 });
        let mut sys2 = PhysicsSystem::new(100.0, 100.0);
        sys2.run(&ctx(1.0), &mut w2);
        assert_eq!(sys2.idle_frames, 0);
        assert_eq!(w2.get::<Position>(mover).unwrap().x, 5.0);
    }
}
