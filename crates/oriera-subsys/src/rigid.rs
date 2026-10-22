//! L6 rigid-body backend (#80, feature `rigid`) — rapier2d.
//!
//! The particle integrator ([`crate::PhysicsSystem`]) stays the default:
//! cheap, bit-exact, made for crowds of dots. This backend swaps in a
//! real physics pipeline — circle colliders, collisions, restitution —
//! behind the same `System<World>` contract, the same `Gravity` /
//! `TimeScale` resources and the same bounds semantics (y grows
//! downward, floor at `y = h`). That swap is the "layer configuration"
//! knob of the verticals strategy made real (VERTICALS §0).
//!
//! **Determinism**: rapier is same-build deterministic; pair this with
//! `Scheduler::with_fixed_dt` (#66) and assert episodes with
//! `World::state_hash` (#67) — the embodied data-collection contract.
//! It is NOT bit-compatible with the particle integrator: collisions
//! and solver iterations add degrees of freedom the integrator does not
//! model. Verticals pick one; they do not mix mid-replay.
//!
//! **Sync protocol** (world ↔ physics, per tick):
//! 1. live entities (Position+Velocity) become dynamic bodies with ball
//!    colliders; dead entities (despawned / `Clear` bumped generations)
//!    have their bodies removed;
//! 2. a world velocity that differs from what we wrote back last tick
//!    is an external write (e.g. L7 `Command::Nudge`, #69) — adopt it
//!    into the body before stepping;
//! 3. step the pipeline with `ctx.dt * TimeScale` and the `Gravity`
//!    resource;
//! 4. write translation/linvel back; remember what we wrote.

use crate::{Gravity, TimeScale};
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Position, Velocity, World};
use rapier2d::prelude::*;
use std::collections::BTreeMap;

/// Entity identity as a plain key: `(index, generation)`. Generation
/// changes on despawn/clear, so stale handles cannot alias live ones.
type EntKey = (u32, u32);

pub struct RigidBodySystem {
    pub bounds: (f32, f32),
    /// Ball collider radius for every dynamic entity.
    pub radius: f32,
    /// Restitution (bounciness) shared by balls and walls.
    pub restitution: f32,
    /// Frames skipped as provably inert (frozen time), the compute-budget
    /// readout mirroring [`crate::PhysicsSystem::idle_frames`].
    pub idle_frames: u64,
    pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad_phase: DefaultBroadPhase,
    narrow_phase: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd: CCDSolver,
    soft_bodies: SoftBodySet,
    map: BTreeMap<EntKey, RigidBodyHandle>,
    /// Velocity we last wrote back, per entity — the external-write
    /// detector of step 2.
    last_vel: BTreeMap<EntKey, (f32, f32)>,
    /// Step-4 scratch table, rebuilt each tick into the same allocation
    /// (`clear()` keeps capacity): `EntKey → (x, y, vx, vy)` clamped
    /// translation/velocity to flush into the columns in ONE pass.
    writeback: BTreeMap<EntKey, (f32, f32, f32, f32)>,
    walls: Vec<RigidBodyHandle>,
}

impl RigidBodySystem {
    pub fn new(w: f32, h: f32) -> Self {
        let mut sys = Self {
            bounds: (w, h),
            radius: 8.0,
            restitution: 0.9,
            idle_frames: 0,
            pipeline: PhysicsPipeline::new(),
            islands: IslandManager::new(),
            broad_phase: DefaultBroadPhase::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd: CCDSolver::new(),
            soft_bodies: SoftBodySet::new(),
            map: BTreeMap::new(),
            last_vel: BTreeMap::new(),
            writeback: BTreeMap::new(),
            walls: Vec::new(),
        };
        sys.build_walls();
        sys
    }

    /// Four static cuboids just outside the bounds, matching the
    /// integrator's containment semantics (nothing escapes `0..=w`,
    /// `0..=h`). Thickness is generous so fast bodies do not tunnel.
    fn build_walls(&mut self) {
        let (w, h) = self.bounds;
        let t = 100.0; // wall thickness
        let rects = [
            (w / 2.0, -t / 2.0, w + 2.0 * t, t),    // top    (y = 0)
            (w / 2.0, h + t / 2.0, w + 2.0 * t, t), // bottom (y = h)
            (-t / 2.0, h / 2.0, t, h + 2.0 * t),    // left   (x = 0)
            (w + t / 2.0, h / 2.0, t, h + 2.0 * t), // right  (x = w)
        ];
        for (x, y, hw, hh) in rects {
            let body = RigidBodyBuilder::fixed()
                .translation(Vec2::new(x, y))
                .build();
            let h = self.bodies.insert(body);
            let col = ColliderBuilder::cuboid(hw / 2.0, hh / 2.0)
                .restitution(self.restitution)
                .build();
            self.colliders.insert_with_parent(col, h, &mut self.bodies);
            self.walls.push(h);
        }
    }

    fn spawn_body(&mut self, key: EntKey, p: &Position, v: &Velocity) {
        let body = RigidBodyBuilder::dynamic()
            .translation(Vec2::new(p.x, p.y))
            .linvel(Vec2::new(v.vx, v.vy))
            .build();
        let h = self.bodies.insert(body);
        let col = ColliderBuilder::ball(self.radius)
            .restitution(self.restitution)
            .friction(0.0)
            .build();
        self.colliders.insert_with_parent(col, h, &mut self.bodies);
        self.map.insert(key, h);
    }
}

impl System<World> for RigidBodySystem {
    fn name(&self) -> &str {
        "rigid"
    }
    fn run(&mut self, ctx: &FrameCtx, world: &mut World) {
        let scale = world
            .resource::<TimeScale>()
            .map(|t| t.effective())
            .unwrap_or(1.0);
        let dt = ctx.dt * scale;
        if dt <= 0.0 {
            self.idle_frames += 1;
            return;
        }
        let g = world.resource::<Gravity>().map(|r| r.0).unwrap_or(0.0);
        let gravity = Vec2::new(0.0, g); // +y is down, matching the integrator

        // 1. Reconcile the body set with the live entity set.
        let live: Vec<(EntKey, Position, Velocity)> = world
            .query2::<Position, Velocity>()
            .map(|(e, p, v)| ((e.index, e.generation), *p, *v))
            .collect();
        let live_keys: std::collections::BTreeSet<EntKey> =
            live.iter().map(|(k, _, _)| *k).collect();
        let stale: Vec<EntKey> = self
            .map
            .keys()
            .filter(|k| !live_keys.contains(k))
            .copied()
            .collect();
        for k in stale {
            if let Some(h) = self.map.remove(&k) {
                self.bodies.remove(
                    h,
                    &mut self.islands,
                    &mut self.colliders,
                    &mut self.impulse_joints,
                    &mut self.multibody_joints,
                    &mut self.soft_bodies,
                    true,
                );
            }
            self.last_vel.remove(&k);
        }
        for (k, p, v) in &live {
            if !self.map.contains_key(k) {
                self.spawn_body(*k, p, v);
                self.last_vel.insert(*k, (v.vx, v.vy));
            }
        }

        // 2. External velocity writes (Nudge etc.) — adopt into bodies.
        for (k, _, v) in &live {
            let Some(&h) = self.map.get(k) else { continue };
            let written = self.last_vel.get(k).copied();
            if written != Some((v.vx, v.vy)) {
                if let Some(b) = self.bodies.get_mut(h) {
                    b.set_linvel(Vec2::new(v.vx, v.vy), true);
                }
            }
        }

        // 3. Step.
        let params = IntegrationParameters {
            dt,
            ..IntegrationParameters::default()
        };
        self.pipeline.step(
            gravity,
            &params,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.soft_bodies,
            &mut self.ccd,
            &(),
            &(),
        );

        // 4. Write back — one column pass with a key lookup (#100).
        // The per-entity `for_each2_mut` scan here was O(n²): a full
        // column walk to find each entity's own row. Build a scratch
        // table (same allocation every tick), then flush all bodies in
        // a single pass. Per-entity writes depend only on that entity's
        // body state, so batching preserves the write order semantics —
        // and `two_runs_are_bit_identical` pins the bit-level contract.
        self.writeback.clear();
        for (k, _, _) in &live {
            let Some(&h) = self.map.get(k) else { continue };
            let Some(b) = self.bodies.get(h) else {
                continue;
            };
            let t = b.translation();
            let lv = b.linvel();
            // Keep the engine's containment contract even against solver
            // drift: clamp into bounds like the integrator would.
            let (bw, bh) = self.bounds;
            let x = t.x.clamp(0.0, bw);
            let y = t.y.clamp(0.0, bh);
            self.writeback.insert(*k, (x, y, lv.x, lv.y));
            self.last_vel.insert(*k, (lv.x, lv.y));
        }
        if !self.writeback.is_empty() {
            let wb = &self.writeback;
            world.for_each2_mut::<Position, Velocity>(|e, p, v| {
                if let Some(&(x, y, vx, vy)) = wb.get(&(e.index, e.generation)) {
                    p.x = x;
                    p.y = y;
                    v.vx = vx;
                    v.vy = vy;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_common::FrameIndex;

    fn ctx(dt: f32) -> FrameCtx<'static> {
        FrameCtx {
            frame: FrameIndex(0),
            dt,
            now: oriera_common::Timestamp(0),
            grants: oriera_common::Grants::EMPTY,
        }
    }

    #[test]
    fn gravity_falls_and_rests_on_the_floor() {
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        let e = world.spawn();
        world.insert(e, Position { x: 50.0, y: 10.0 });
        world.insert(e, Velocity { vx: 0.0, vy: 0.0 });

        let mut sys = RigidBodySystem::new(100.0, 100.0);
        // restitution 0.9 keeps the ball bouncing for a while — give the
        // decay its physically honest time, then it must have settled.
        for _ in 0..6000 {
            sys.run(&ctx(1.0 / 60.0), &mut world);
        }
        let p = world.get::<Position>(e).unwrap();
        let r = sys.radius;
        assert!(p.y > 100.0 - r * 2.5, "rest on floor: y={}", p.y);
        assert!(p.y <= 100.0, "never past the floor");
        let v = world.get::<Velocity>(e).unwrap();
        assert!(v.vy.abs() < 5.0, "settled, vy={}", v.vy);
    }

    #[test]
    fn walls_contain_every_trajectory() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 50.0, y: 50.0 });
        world.insert(
            e,
            Velocity {
                vx: 500.0,
                vy: 300.0,
            },
        );

        let mut sys = RigidBodySystem::new(100.0, 100.0);
        for _ in 0..300 {
            sys.run(&ctx(1.0 / 60.0), &mut world);
            let p = world.get::<Position>(e).unwrap();
            assert!((0.0..=100.0).contains(&p.x), "x escaped: {}", p.x);
            assert!((0.0..=100.0).contains(&p.y), "y escaped: {}", p.y);
        }
    }

    #[test]
    fn two_bodies_collide_and_do_not_overlap() {
        let mut world = World::new();
        let a = world.spawn();
        world.insert(a, Position { x: 30.0, y: 80.0 });
        world.insert(a, Velocity { vx: 40.0, vy: 0.0 });
        let b = world.spawn();
        world.insert(b, Position { x: 70.0, y: 80.0 });
        world.insert(b, Velocity { vx: -40.0, vy: 0.0 });

        let mut sys = RigidBodySystem::new(100.0, 100.0);
        let r = sys.radius;
        for _ in 0..180 {
            sys.run(&ctx(1.0 / 60.0), &mut world);
        }
        let pa = *world.get::<Position>(a).unwrap();
        let pb = *world.get::<Position>(b).unwrap();
        let d = ((pa.x - pb.x).powi(2) + (pa.y - pb.y).powi(2)).sqrt();
        assert!(
            d >= r * 1.9,
            "balls overlap: d={d} (r={r}) — a={pa:?} b={pb:?}"
        );
    }

    #[test]
    fn frozen_time_is_an_exact_no_op() {
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        world.insert_resource(TimeScale(0.0));
        let e = world.spawn();
        world.insert(e, Position { x: 50.0, y: 50.0 });
        world.insert(e, Velocity { vx: 1.0, vy: 1.0 });

        let mut sys = RigidBodySystem::new(100.0, 100.0);
        for _ in 0..5 {
            sys.run(&ctx(1.0), &mut world);
        }
        assert_eq!(sys.idle_frames, 5);
        let p = *world.get::<Position>(e).unwrap();
        assert_eq!((p.x, p.y), (50.0, 50.0));
    }

    #[test]
    fn two_runs_are_bit_identical_under_fixed_dt() {
        let run = || {
            let mut world = World::new();
            world.insert_resource(Gravity(9.8));
            let mut bodies = Vec::new();
            for i in 0..10 {
                let e = world.spawn();
                world.insert(
                    e,
                    Position {
                        x: 10.0 + i as f32 * 8.0,
                        y: 15.0 + (i % 3) as f32 * 4.0,
                    },
                );
                world.insert(
                    e,
                    Velocity {
                        vx: 3.0 - (i as f32) * 0.5,
                        vy: 0.5,
                    },
                );
                bodies.push(e);
            }
            // An external nudge mid-run, like Command::Nudge would do.
            let mut sys = RigidBodySystem::new(200.0, 150.0);
            for step in 0..120 {
                if step == 30 {
                    let v = world.get_mut::<Velocity>(bodies[2]).unwrap();
                    v.vx += 12.0;
                }
                sys.run(&ctx(1.0 / 60.0), &mut world);
            }
            (
                world.state_hash(),
                bodies
                    .iter()
                    .map(|&e| {
                        let p = world.get::<Position>(e).unwrap();
                        (p.x.to_bits(), p.y.to_bits())
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let (h1, b1) = run();
        let (h2, b2) = run();
        assert_eq!(h1, h2, "state_hash differs across runs");
        assert_eq!(b1, b2, "per-entity bits differ across runs");
    }

    /// #100 regression: the batched single-pass write-back must reach
    /// EVERY body. Linear motion, no contacts (spacing > 2·radius), so
    /// each row must advance exactly `vx·dt` — a missed write leaves
    /// that body at dx = 0.
    #[test]
    fn thousand_bodies_all_receive_their_writeback() {
        let mut world = World::new();
        let mut bodies = Vec::new();
        for i in 0..1000 {
            let e = world.spawn();
            let x0 = 5.0 + (i % 25) as f32 * 17.0;
            let y0 = 5.0 + (i / 25) as f32 * 17.0;
            world.insert(e, Position { x: x0, y: y0 });
            world.insert(e, Velocity { vx: 10.0, vy: 0.0 });
            bodies.push((e, x0));
        }
        let mut sys = RigidBodySystem::new(430.0, 700.0);
        sys.run(&ctx(1.0 / 60.0), &mut world);
        let dt = 1.0f32 / 60.0;
        for &(e, x0) in &bodies {
            let p = world.get::<Position>(e).unwrap();
            assert!(
                (p.x - x0 - 10.0 * dt).abs() < 1e-3,
                "body missed its writeback: expected x≈{}, got {}",
                x0 + 10.0 * dt,
                p.x
            );
        }
    }

    #[test]
    fn despawned_entities_leave_no_ghost_bodies() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 50.0, y: 50.0 });
        world.insert(e, Velocity { vx: 0.0, vy: 0.0 });

        let mut sys = RigidBodySystem::new(100.0, 100.0);
        sys.run(&ctx(1.0 / 60.0), &mut world);
        assert_eq!(sys.map.len(), 1);

        world.clear(); // generations bump: the old handle must not alias
        let e2 = world.spawn();
        world.insert(e2, Position { x: 50.0, y: 50.0 });
        world.insert(e2, Velocity { vx: 0.0, vy: 0.0 });
        sys.run(&ctx(1.0 / 60.0), &mut world);
        assert_eq!(sys.map.len(), 1, "old body removed, new one lives");
        assert_eq!(sys.bodies.len(), 1 + sys.walls.len());
    }
}
