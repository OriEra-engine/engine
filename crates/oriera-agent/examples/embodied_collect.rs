//! Embodied data collection (#80) — the world-as-benchmark loop.
//!
//! Spawns a robot among targets in a rigid-body world, drives it with
//! a deterministic seek policy through the L7 action channel
//! (`Command::Nudge`, #69), steps the rapier2d backend (L6 `rigid`)
//! under a pinned dt (#66), and records every transition into a
//! LeRobotDataset-v3-aligned JSONL dataset whose episodes carry world
//! fingerprints (#67).
//!
//! Runs the whole collection TWICE and asserts both runs are
//! bit-identical — the embodied acceptance of the determinism contract.
//!
//! Run: `cargo run -p oriera-agent --example embodied_collect --features rigid,json`
//! Output: `out/embodied/{data/chunk-000.jsonl, meta/info.json}`

use oriera_agent::embodied::{EpisodeRecorder, Observation, Transition};
use oriera_common::FrameIndex;
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, Position, Velocity, World};
use oriera_infer::{Command, InferSystem};
use oriera_subsys::{Gravity, RigidBodySystem, TimeScale};

const EPISODES: u32 = 8;
const STEPS: u64 = 240;
const DT: f32 = 1.0 / 60.0;
const BOUNDS: (f32, f32) = (640.0, 480.0);
const ROBOT_SPAWN: (f32, f32) = (80.0, 80.0);
const TARGETS: usize = 5;
const THRUST: f32 = 60.0;
const CATCH_RADIUS: f32 = 18.0;

fn ctx(step: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(step),
        dt: DT,
        now: oriera_common::Timestamp(0),
        grants: oriera_common::Grants::EMPTY,
    }
}

/// Deterministic LCG for target placement — same seed, same world.
struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self, lo: f32, hi: f32) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let f = (self.0 >> 33) as f32 / (u32::MAX >> 1) as f32;
        lo + f * (hi - lo)
    }
}

/// The robot's eyes.
fn observe(world: &World, robot: Entity) -> Observation {
    EpisodeRecorder::observe(world, robot)
}

/// Deterministic seek policy: full thrust toward the nearest target,
/// quantized to a coarse action grid (a policy with opinions, not
/// noise — the dataset must replay).
fn policy(obs: &Observation) -> [f32; 2] {
    let [dx, dy, d, _spd] = obs.nearest;
    if d < 1e-3 {
        return [0.0, 0.0];
    }
    let q = |v: f32| (v * 4.0).round() / 4.0; // ±0.25 grid
    [q(dx / d * THRUST), q(dy / d * THRUST)]
}

/// Dense shaping reward: progress toward the nearest target this step,
/// plus a success bonus on capture.
fn reward(prev: &Observation, now: &Observation, done: bool) -> f32 {
    let progress = prev.nearest[2] - now.nearest[2];
    progress + if done { 10.0 } else { 0.0 }
}

/// One full collection run. Returns (jsonl, info_json, per-episode
/// hashes, successes) — everything the determinism assert needs.
fn collect() -> (String, String, Vec<u64>, u32) {
    let mut rec = EpisodeRecorder::new();
    let mut rng = Lcg(0x5EED_0000_0080_0001);

    for ep in 0..EPISODES {
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        world.insert_resource(TimeScale(1.0));
        let robot = world.spawn();
        world.insert(
            robot,
            Position {
                x: ROBOT_SPAWN.0,
                y: ROBOT_SPAWN.1,
            },
        );
        world.insert(robot, Velocity { vx: 0.0, vy: 0.0 });
        for _ in 0..TARGETS {
            let t = world.spawn();
            world.insert(
                t,
                Position {
                    x: rng.next_f32(120.0, BOUNDS.0 - 40.0),
                    y: rng.next_f32(120.0, BOUNDS.1 - 40.0),
                },
            );
            world.insert(t, Velocity { vx: 0.0, vy: 0.0 });
        }

        let mut infer = InferSystem::new();
        let mut rigid = RigidBodySystem::new(BOUNDS.0, BOUNDS.1);

        let mut done = false;
        let mut step: u64 = 0;
        while !done && step < STEPS {
            let obs = observe(&world, robot);
            let action = policy(&obs);
            infer.submit(Command::Nudge {
                entity: robot,
                fx: action[0],
                fy: action[1],
            });
            infer.run(&ctx(step), &mut world); // L7 action channel
            rigid.run(&ctx(step), &mut world); // L6 rigid step
            let next = observe(&world, robot);

            let caught = next.nearest[2] < CATCH_RADIUS;
            done = caught || step + 1 == STEPS;
            rec.record(Transition {
                episode: ep,
                step,
                obs,
                action,
                reward: reward(&obs, &next, caught),
                next,
                done,
            });
            step += 1;
        }
        let hash = world.state_hash();
        rec.finish_episode(observe(&world, robot).nearest[2] < CATCH_RADIUS, hash);
    }

    let jsonl = rec.to_jsonl();
    let info = rec.info_json(
        concat!("oriera-engine/", env!("CARGO_PKG_VERSION")),
        "rigid-rapier2d",
        DT,
    );
    (jsonl, info, rec.episode_hashes.clone(), rec.successes)
}

fn write_out(rel: &str, content: &str) -> std::io::Result<()> {
    let path = std::path::Path::new(rel);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)
}

fn main() {
    println!("╔══════════════════════════════════════════════════════╗");
    println!("║  OriEra L6×L9 · 具身数据采集（rapier2d × LeRobot 对齐） ║");
    println!("╚══════════════════════════════════════════════════════╝");
    println!(
        "  世界 {}×{} · {} 集 × ≤{} 步 · dt={:.4} · 目标 {} 个",
        BOUNDS.0, BOUNDS.1, EPISODES, STEPS, DT, TARGETS
    );

    // The determinism receipt: collect twice, demand bit equality.
    let (jsonl1, info1, hashes1, ok1) = collect();
    let (jsonl2, _info2, hashes2, ok2) = collect();
    assert_eq!(hashes1, hashes2, "#66+#67: episode fingerprints must match");
    assert_eq!(jsonl1, jsonl2, "dataset must be bit-identical across runs");
    assert_eq!(ok1, ok2);

    let steps = jsonl1.lines().count();
    println!(
        "  ✓ 双跑逐位一致：{} 集 · {} transitions · 成功 {} 集",
        EPISODES, steps, ok1
    );
    for (i, h) in hashes1.iter().enumerate() {
        println!("    episode {i:02} · fingerprint {h:016x}");
    }

    write_out("out/embodied/data/chunk-000.jsonl", &jsonl1).unwrap();
    write_out("out/embodied/meta/info.json", &info1).unwrap();
    println!("  ✓ 已写出 out/embodied/{{data/chunk-000.jsonl, meta/info.json}}");
    println!("  下一步（社区位）：JSONL→parquet 转换器 + LeRobotDataset v3 直读（见 docs/EMBODIED-PIPELINES.md）");
}
