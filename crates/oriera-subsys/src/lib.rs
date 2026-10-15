//! L6 Subsystems — physics, animation, audio, UI, net sync.
//!
//! v0.1 implements the physics subsystem only: fixed-step integration of
//! `Position`/`Velocity` (L3 components) with bounding-box bounce. The
//! world is passed in by the L2 scheduler each frame — subsystems never
//! reach into globals (book T3 §3.8.1).

use oriera_core::{FrameCtx, System};
use oriera_ecs::{Position, Velocity, World};

/// Singleton resource: downward acceleration in units/s².
/// The inference layer (L7) writes it; physics (L6) reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gravity(pub f32);

/// Fixed-step particle physics: integrate velocity, apply gravity,
/// bounce off `bounds`. Runs as an L2-scheduled system.
pub struct PhysicsSystem {
    pub bounds: (f32, f32),
}

impl PhysicsSystem {
    pub fn new(w: f32, h: f32) -> Self {
        Self { bounds: (w, h) }
    }
}

impl System<World> for PhysicsSystem {
    fn name(&self) -> &str {
        "physics"
    }
    fn run(&mut self, ctx: &FrameCtx, world: &mut World) {
        let gravity = world.resource::<Gravity>().map(|g| g.0).unwrap_or(0.0);
        let (bw, bh) = self.bounds;
        world.for_each2_mut::<Position, Velocity>(|_e, p, v| {
            p.x += v.vx * ctx.dt;
            p.y += v.vy * ctx.dt;
            v.vy += gravity * ctx.dt;
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

    fn ctx(dt: f32) -> FrameCtx {
        FrameCtx {
            frame: FrameIndex(0),
            dt,
            now: oriera_common::Timestamp(0),
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
}
