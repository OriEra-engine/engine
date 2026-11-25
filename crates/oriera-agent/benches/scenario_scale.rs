//! **多环境并行缩放** —— 三个候选场景的规模定标底座。
//!
//! `world_frame` 量的是**一个世界**一帧多少钱。但具身训练要的是
//! **N 个世界同时步进**，这是完全不同的一根轴：单环境便宜不代表
//! N 环境线性可加，因为 `InferJobs` 一 job 一 OS 线程——N 环境各提交
//! 一次推理，就是每步 N 次线程 spawn。
//!
//! 本基准量三档负载在环境数梯度下的总成本与每环境摊销：
//!
//! | 档 | 每环境每步做什么 | 对应场景 |
//! |---|---|---|
//! | `phys` | 物理步 | 纯仿真 / 回放 |
//! | `phys_mem` | 物理步 + 记忆检索 | 带状态的环境 |
//! | `phys_mem_infer` | 再加一次推理提交 + 收割 | 训练 / 智能体闭环 |
//!
//! 三档之间的差值直接回答："线程模型在哪一档、哪个环境数上开始咬人"。
//!
//! 跑法：
//! ```sh
//! cargo bench -p oriera-agent --features rigid3d --bench scenario_scale -- --nocapture scale
//! ```
//!
//! 内部基线，未标定硬件与构建形态前不对外。

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use oriera_agent::{Episode, MemoryStore};
use oriera_common::{FrameIndex, Grants, Timestamp};
#[cfg(not(feature = "rigid3d"))]
use oriera_core::FrameCtx;
#[cfg(feature = "rigid3d")]
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use oriera_infer::{ChatMessage, InferJobs, RuleProvider};
use std::sync::Arc;

// 无 rigid3d 时物理段空转，ctx 与 world 字段仅供占位编译。
#[cfg_attr(not(feature = "rigid3d"), allow(dead_code))]
fn ctx(frame: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(frame),
        dt: 1.0 / 60.0,
        now: Timestamp(frame * 16_667),
        grants: Grants::EMPTY,
    }
}

/// 一个环境：物理世界 + 自己的记忆 + 自己的推理池。
struct Env {
    #[cfg_attr(not(feature = "rigid3d"), allow(dead_code))]
    world: World,
    #[cfg(feature = "rigid3d")]
    sys: oriera_subsys::MultibodySystem,
    store: MemoryStore,
    jobs: InferJobs<RuleProvider>,
}

fn make_env(props: usize, memories: usize) -> Env {
    #[cfg_attr(not(feature = "rigid3d"), allow(unused_mut))]
    let mut world = World::new();
    #[cfg(feature = "rigid3d")]
    let sys = {
        use oriera_subsys::{
            ArmDescription, Gravity, JointTargets, JointTelemetry, MultibodySystem, Pose3, Scene3D,
            SimObject, TimeScale,
        };
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
        for i in 0..props {
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
        let mut s = MultibodySystem::new(ArmDescription::default(), Scene3D::default());
        s.run(&ctx(0), &mut world);
        s
    };
    #[cfg(not(feature = "rigid3d"))]
    let _ = props;

    let mut store = MemoryStore::with_capacity(memories.max(1));
    for i in 0..memories {
        store.record(
            "npc",
            Episode {
                turn: i as u64,
                day: 0,
                about: "forge".into(),
                text: format!("observation {i}"),
                importance: (i % 5) as u8,
            },
        );
    }
    Env {
        world,
        #[cfg(feature = "rigid3d")]
        sys,
        store,
        jobs: InferJobs::new(Arc::new(RuleProvider::new()), 4),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tier {
    Phys,
    PhysMem,
    PhysMemInfer,
}

/// 所有环境同步步进一次，返回总耗时 µs。
fn step_all(envs: &mut [Env], tier: Tier, frame: u64) -> f64 {
    let t = std::time::Instant::now();
    for e in envs.iter_mut() {
        #[cfg(feature = "rigid3d")]
        e.sys.run(&ctx(frame), &mut e.world);
        if tier != Tier::Phys {
            black_box(
                e.store
                    .retrieve("npc", "forge", "observation", frame, 8)
                    .len(),
            );
        }
        if tier == Tier::PhysMemInfer && e.jobs.submit(vec![ChatMessage::user("act")]).is_ok() {
            while e.jobs.inflight() > 0 {
                black_box(e.jobs.poll().len());
                std::hint::spin_loop();
            }
            black_box(e.jobs.poll().len());
        }
    }
    t.elapsed().as_secs_f64() * 1e6
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn bench_scale(c: &mut Criterion) {
    if !std::env::args().any(|a| a == "scale") {
        let mut g = c.benchmark_group("scenario_scale_noop");
        g.sample_size(10);
        g.bench_function("noop", |b| b.iter(|| black_box(1u64)));
        g.finish();
        return;
    }

    println!("\n══════ 多环境并行缩放（每环境 4 props / 60 条记忆）══════");
    println!(
        "{:>6} {:>14} {:>14} {:>16} {:>14}",
        "envs", "phys", "phys+mem", "phys+mem+infer", "infer 边际"
    );
    println!(
        "{:>6} {:>14} {:>14} {:>16} {:>14}",
        "", "µs/步", "µs/步", "µs/步", "µs"
    );

    for n in [1usize, 8, 32, 64, 128] {
        let mut row = [0.0f64; 3];
        for (ti, tier) in [Tier::Phys, Tier::PhysMem, Tier::PhysMemInfer]
            .into_iter()
            .enumerate()
        {
            let mut envs: Vec<Env> = (0..n).map(|_| make_env(4, 60)).collect();
            for f in 0..20 {
                step_all(&mut envs, tier, f + 1);
            }
            let samples: Vec<f64> = (0..60)
                .map(|f| step_all(&mut envs, tier, f + 100))
                .collect();
            row[ti] = median(samples);
        }
        println!(
            "{:>6} {:>14.1} {:>14.1} {:>16.1} {:>14.1}",
            n,
            row[0],
            row[1],
            row[2],
            row[2] - row[1]
        );
    }

    println!("\n────── 每环境摊销（µs/环境/步）──────");
    println!("{:>6} {:>14} {:>16}", "envs", "phys+mem", "phys+mem+infer");
    for n in [1usize, 8, 32, 64, 128] {
        let mut envs: Vec<Env> = (0..n).map(|_| make_env(4, 60)).collect();
        for f in 0..20 {
            step_all(&mut envs, Tier::PhysMemInfer, f + 1);
        }
        let a = median(
            (0..60)
                .map(|f| step_all(&mut envs, Tier::PhysMem, f + 100))
                .collect(),
        );
        let b = median(
            (0..60)
                .map(|f| step_all(&mut envs, Tier::PhysMemInfer, f + 200))
                .collect(),
        );
        println!("{:>6} {:>14.2} {:>16.2}", n, a / n as f64, b / n as f64);
    }

    println!("\n────── 训练吞吐视角：N 环境能跑多少步/秒 ──────");
    println!("（含推理闭环；步频 = 1e6 / 每步总 µs）");
    for n in [1usize, 8, 32, 64, 128] {
        let mut envs: Vec<Env> = (0..n).map(|_| make_env(4, 60)).collect();
        for f in 0..20 {
            step_all(&mut envs, Tier::PhysMemInfer, f + 1);
        }
        let m = median(
            (0..60)
                .map(|f| step_all(&mut envs, Tier::PhysMemInfer, f + 100))
                .collect(),
        );
        println!(
            "envs={:<4} {:>9.1} µs/步   {:>8.1} 步/秒   环境步/秒 {:>9.0}",
            n,
            m,
            1e6 / m,
            1e6 / m * n as f64
        );
    }
    // ── 并发提交：N 个环境同时提交，N 条线程同时存活 ──
    // 上面的梯度是**逐个**步进（提交→收割→下一个），同一时刻只有 1 条
    // worker 线程。真正的线程爆炸发生在"全部提交完再统一收割"——
    // 这才是训练循环批推理的形态，两者必须分开量。
    println!("\n────── 串行 vs 并发提交（N 环境，µs/步）──────");
    println!(
        "{:>6} {:>14} {:>14} {:>10}",
        "envs", "串行提交收割", "全提交后收割", "并发/串行"
    );
    for n in [1usize, 8, 32, 64, 128] {
        let mut envs: Vec<Env> = (0..n).map(|_| make_env(4, 60)).collect();
        let serial = {
            for f in 0..20 {
                step_all(&mut envs, Tier::PhysMemInfer, f + 1);
            }
            median(
                (0..60)
                    .map(|f| step_all(&mut envs, Tier::PhysMemInfer, f + 100))
                    .collect(),
            )
        };
        let concurrent = {
            let mut v = Vec::new();
            for f in 0..20 {
                for e in envs.iter_mut() {
                    #[cfg(feature = "rigid3d")]
                    e.sys.run(&ctx(f + 1), &mut e.world);
                    black_box(
                        e.store
                            .retrieve("npc", "forge", "observation", f + 1, 8)
                            .len(),
                    );
                    let _ = e.jobs.submit(vec![ChatMessage::user("act")]);
                }
                for e in envs.iter_mut() {
                    while e.jobs.inflight() > 0 {
                        black_box(e.jobs.poll().len());
                        std::hint::spin_loop();
                    }
                    black_box(e.jobs.poll().len());
                }
            }
            for f in 0..60 {
                let t = std::time::Instant::now();
                for e in envs.iter_mut() {
                    #[cfg(feature = "rigid3d")]
                    e.sys.run(&ctx(f + 100), &mut e.world);
                    black_box(
                        e.store
                            .retrieve("npc", "forge", "observation", f + 100, 8)
                            .len(),
                    );
                    let _ = e.jobs.submit(vec![ChatMessage::user("act")]);
                }
                for e in envs.iter_mut() {
                    while e.jobs.inflight() > 0 {
                        black_box(e.jobs.poll().len());
                        std::hint::spin_loop();
                    }
                    black_box(e.jobs.poll().len());
                }
                v.push(t.elapsed().as_secs_f64() * 1e6);
            }
            median(v)
        };
        println!(
            "{:>6} {:>14.1} {:>14.1} {:>9.2}x",
            n,
            serial,
            concurrent,
            concurrent / serial
        );
    }

    println!("\n注：内部基线；RuleProvider 离线后端，不含真实模型延迟。\n");

    let mut g = c.benchmark_group("scenario_scale_noop");
    g.sample_size(10);
    g.bench_function("noop", |b| b.iter(|| black_box(1u64)));
    g.finish();
}

criterion_group!(benches, bench_scale);
criterion_main!(benches);
