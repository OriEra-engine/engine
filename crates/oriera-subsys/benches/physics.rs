//! L6 物理步微基准 — 粒子积分与 3D 关节链步进。
//!
//! 三组场景：
//! - `physics_particles/n{entities}`：默认 `PhysicsSystem`（纯积分 + 边界弹跳），
//!   实体数梯度 1k / 5k / 20k——ECS 列扫描 + 积分的线性成本；
//! - `physics_particles_idle/n{entities}`：同规模静止世界——`idle_frames`
//!   快路径（扫描一遍即 return）与真实积分的差距；
//! - `multibody_step/props{n}`（feature `rigid3d`）：`MultibodySystem::run`
//!   单步，关节链 + n 个自由刚体 prop（n = 1 / 4 / 16）——具身训练每
//!   tick 真正支付的成本。
//!
//! 确定性说明：物理步本身是纯函数（同状态同输出），基准只量耗时；
//! 与 MuJoCo 对拍的 `|dq|` 偏差是质量门，**不在此基准范围**。
//!
//! 跑法：`cargo bench -p oriera-subsys --features rigid3d`。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_common::{FrameIndex, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Position, Velocity, World};
use oriera_subsys::{Gravity, PhysicsSystem, TimeScale};

fn ctx(frame: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(frame),
        dt: 1.0 / 60.0,
        now: Timestamp(frame),
        grants: oriera_common::Grants::EMPTY,
    }
}

fn particle_world(n: usize, moving: bool) -> World {
    let mut world = World::new();
    world.insert_resource(Gravity(9.8));
    world.insert_resource(TimeScale(1.0));
    for i in 0..n {
        let e = world.spawn();
        world.insert(
            e,
            Position {
                x: (i as f32 % 100.0) + 0.5,
                y: (i as f32 / 100.0 % 100.0) + 0.5,
            },
        );
        world.insert(
            e,
            Velocity {
                vx: if moving { 0.3 } else { 0.0 },
                vy: if moving { -0.2 } else { 0.0 },
            },
        );
    }
    world
}

fn bench_particles(c: &mut Criterion) {
    let mut group = c.benchmark_group("physics_particles");

    // ── 运动世界：真实积分 ──
    for n in [1_000usize, 5_000, 20_000] {
        group.bench_with_input(BenchmarkId::new("n", n), &n, |b, &n| {
            let mut world = particle_world(n, true);
            let mut sys = PhysicsSystem::new(100.0, 100.0);
            b.iter(|| {
                sys.run(&ctx(1), &mut world);
                black_box(sys.idle_frames)
            })
        });
    }

    // ── 静止世界：idle 快路径（扫描后 return，不写列） ──
    for n in [1_000usize, 5_000, 20_000] {
        group.bench_with_input(BenchmarkId::new("idle", n), &n, |b, &n| {
            let mut world = particle_world(n, false);
            let mut sys = PhysicsSystem::new(100.0, 100.0);
            b.iter(|| {
                sys.run(&ctx(1), &mut world);
                black_box(sys.idle_frames)
            })
        });
    }

    group.finish();
}

/// `MultibodySystem::run` 单步：4-DoF 臂 + n 个 prop。
#[cfg(feature = "rigid3d")]
fn bench_multibody(c: &mut Criterion) {
    use oriera_subsys::rigid3d::{
        ArmDescription, JointTargets, JointTelemetry, MultibodySystem, Pose3, Scene3D, SimObject,
    };

    fn world_with_props(n: usize) -> (World, MultibodySystem) {
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        world.insert_resource(TimeScale(1.0));
        let arm = world.spawn();
        world.insert(
            arm,
            JointTargets {
                angles: vec![0.0; 4],
                gripper: 0.0,
            },
        );
        world.insert(arm, JointTelemetry::default());
        // prop 落在桌面静止高度 0.55，与既有测试同口径——不与臂链接穿。
        for i in 0..n {
            let obj = world.spawn();
            world.insert(
                obj,
                Pose3 {
                    pos: [
                        0.20 + 0.08 * (i as f32 % 5.0),
                        0.55,
                        0.10 * (i as f32 / 5.0),
                    ],
                    ..Default::default()
                },
            );
            world.insert(obj, SimObject);
        }
        let sys = MultibodySystem::new(ArmDescription::default(), Scene3D::default());
        (world, sys)
    }

    let mut group = c.benchmark_group("multibody_step");
    for n in [1usize, 4, 16] {
        group.bench_with_input(BenchmarkId::new("props", n), &n, |b, &n| {
            let (mut world, mut sys) = world_with_props(n);
            // 预热：首帧会创建刚体，稳态才是每 tick 的真实成本。
            sys.run(&ctx(0), &mut world);
            b.iter(|| {
                sys.run(&ctx(1), &mut world);
                black_box(sys.gap())
            })
        });
    }
    group.finish();
}

#[cfg(not(feature = "rigid3d"))]
fn bench_multibody(_c: &mut Criterion) {}

criterion_group!(benches, bench_particles, bench_multibody);
criterion_main!(benches);
