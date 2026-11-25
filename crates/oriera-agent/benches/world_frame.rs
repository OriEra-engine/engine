//! **整机帧成本** —— 一个完整世界跑一帧到底多少钱。
//!
//! 与 `benches/` 里其余基准的分工：那些量**零件**（`resolve` 29 ns、
//! 检索 1.3 µs/条、GLB 5.2 ns/顶点、物理 1.65 ns/实体）。零件都便宜，
//! 装到一起是否还在 60fps 预算内、超在哪一段，零件基准回答不了。
//! 本文件量**整机油耗**。
//!
//! ## 世界构成（最小但齐全）
//!
//! 一帧里同时发生四件事，对应引擎四条主线：
//!
//! | 段 | 内容 | 层 |
//! |---|---|---|
//! | `physics` | `MultibodySystem` 关节链 + n 个自由刚体 | L6 |
//! | `memory` | `MemoryStore::retrieve` 窗口内检索 | L9 |
//! | `infer` | `InferJobs::submit` + `poll` 一回合推理 | L7 |
//! | `gate` | `validate_asset` 走 GLB 全路径闸口 | L8 |
//!
//! 四段可单独开关，于是同一套夹具既能量整机，也能量"去掉某段省多少"
//! ——**差值才是该段的真实边际成本**，比单独跑该段的孤立读数可信。
//!
//! ## 为什么infer段必须计入提交税
//!
//! `InferJobs` 一 job 一 OS 线程，`submit` 的 ~45 µs 几乎全是线程 spawn
//! 与 channel 往返。孤立基准里它只是个数字；放进帧里才知道它吃掉多少
//! 预算。本基准把 submit + drain 完整计入，不做任何摊销假设。
//!
//! ## 跑法
//!
//! ```sh
//! cargo bench -p oriera-agent --features rigid3d --bench world_frame
//! # 打印各段占比与预算结论（非 criterion，直接读）：
//! cargo bench -p oriera-agent --features rigid3d --bench world_frame -- --nocapture breakdown
//! ```
//!
//! 所有数字均为**内部基线**：未标定硬件与构建形态前不对外，
//! 远程后端未经真实压测，不给 SLA 承诺。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_agent::{Episode, MemoryStore};
use oriera_common::{FrameIndex, Grants, Timestamp};
#[cfg(not(feature = "rigid3d"))]
use oriera_core::FrameCtx;
#[cfg(feature = "rigid3d")]
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use oriera_genai::{validate_asset, AssetFormat, GeneratedAsset, ValidationLimits};
use oriera_infer::{ChatMessage, InferJobs, RuleProvider};
use std::sync::Arc;

/// 60fps 的帧预算（微秒）。所有占比都对着这个数算。
const FRAME_BUDGET_US: f64 = 16_667.0;

// 无 rigid3d 时 physics 段空转，ctx 与 world/sys 字段仅供占位编译。
#[cfg_attr(not(feature = "rigid3d"), allow(dead_code))]
fn ctx(frame: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(frame),
        dt: 1.0 / 60.0,
        now: Timestamp(frame * 16_667),
        grants: Grants::EMPTY,
    }
}

// ── 各段夹具 ───────────────────────────────────────────────────────

#[cfg(feature = "rigid3d")]
fn physics_world(props: usize) -> (World, oriera_subsys::MultibodySystem) {
    use oriera_subsys::{
        ArmDescription, Gravity, JointTargets, JointTelemetry, MultibodySystem, Pose3, Scene3D,
        SimObject, TimeScale,
    };
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
    // prop 落桌面静止高度 0.55，与既有测试同口径，不与臂链接穿。
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
    let mut sys = MultibodySystem::new(ArmDescription::default(), Scene3D::default());
    // 预热：首帧建刚体，稳态才是每 tick 的真实成本。
    sys.run(&ctx(0), &mut world);
    (world, sys)
}

fn memory_store(entries: usize) -> MemoryStore {
    let mut store = MemoryStore::with_capacity(entries.max(1));
    const TOPICS: [&str; 4] = ["forge", "market", "harbor", "gate"];
    for i in 0..entries {
        store.record(
            "npc",
            Episode {
                turn: i as u64,
                day: (i / 24) as u32,
                about: TOPICS[i % TOPICS.len()].to_string(),
                text: format!("observation {} at the {}", i, TOPICS[i % TOPICS.len()]),
                importance: (i % 5) as u8,
            },
        );
    }
    store
}

fn jobs() -> InferJobs<RuleProvider> {
    InferJobs::new(Arc::new(RuleProvider::new()), 8)
}

fn prompt() -> Vec<ChatMessage> {
    vec![ChatMessage::user("what do you remember about the forge?")]
}

/// 一份合法 GLB：8 顶点立方体三角网格，走完整解码 + 三道核对。
fn glb_asset() -> GeneratedAsset {
    let positions: [[f32; 3]; 8] = [
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, -0.5, 0.5],
        [0.5, -0.5, 0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
    ];
    let indices: [u32; 36] = [
        0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7, 4,
        1, 5, 6, 1, 6, 2,
    ];
    let pos_bytes = positions.len() * 12;
    let idx_bytes = indices.len() * 4;
    let doc = serde_json::json!({
        "asset": {"version": "2.0"},
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 8, "type": "VEC3"},
            {"bufferView": 1, "componentType": 5125, "count": 36, "type": "SCALAR"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": pos_bytes},
            {"buffer": 0, "byteOffset": pos_bytes, "byteLength": idx_bytes}
        ],
        "buffers": [{"byteLength": pos_bytes + idx_bytes}]
    });
    let mut bin = Vec::new();
    for p in &positions {
        for c in p {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    for i in &indices {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    let json_raw = serde_json::to_vec(&doc).unwrap();
    let json_pad = json_raw.len().div_ceil(4) * 4;
    let bin_pad = bin.len().div_ceil(4) * 4;
    let total = 12 + 8 + json_pad + 8 + bin_pad;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_pad as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_raw);
    out.extend(std::iter::repeat_n(b' ', json_pad - json_raw.len()));
    out.extend_from_slice(&(bin_pad as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out.extend(std::iter::repeat_n(0, bin_pad - bin.len()));

    GeneratedAsset {
        format: AssetFormat::Glb,
        payload: out,
        provider_id: "bench".into(),
        model: "fixture".into(),
        usage: None,
    }
}

// ── 一帧的四种开关组合 ─────────────────────────────────────────────

/// 一帧里跑哪些段。用差值法量各段的边际成本。
#[derive(Clone, Copy)]
struct Load {
    physics: bool,
    memory: bool,
    infer: bool,
    gate: bool,
}

impl Load {
    const NONE: Self = Self {
        physics: false,
        memory: false,
        infer: false,
        gate: false,
    };
    const FULL: Self = Self {
        physics: true,
        memory: true,
        infer: true,
        gate: true,
    };
}

/// 跑一帧，返回各段耗时（µs）。未启用的段记 0。
///
/// 计时用 `Instant`：criterion 负责统计稳定性，这里要的是**段间占比**，
/// 必须逐段实测而不是估算。
impl Rig {
    /// 跑一帧，返回各段耗时（µs）。未启用的段记 0。
    ///
    /// 计时用 `Instant`：criterion 负责统计稳定性，这里要的是**段间占比**，
    /// 必须逐段实测而不是估算。
    fn run_frame(&mut self, load: Load) -> [f64; 4] {
        let frame = self.frame;
        let mut us = [0.0f64; 4];

        if load.physics {
            let t = std::time::Instant::now();
            #[cfg(feature = "rigid3d")]
            self.sys.run(&ctx(frame), &mut self.world);
            us[0] = t.elapsed().as_secs_f64() * 1e6;
        }
        if load.memory {
            let t = std::time::Instant::now();
            let hits = self
                .store
                .retrieve("npc", "forge", "observation forge", frame, 8);
            black_box(hits.len());
            us[1] = t.elapsed().as_secs_f64() * 1e6;
        }
        if load.infer {
            let t = std::time::Instant::now();
            if self.jobs.submit(prompt()).is_ok() {
                // drain：worker 先 send 再减 inflight，退出后须再 poll 一次收尾。
                while self.jobs.inflight() > 0 {
                    black_box(self.jobs.poll().len());
                    std::hint::spin_loop();
                }
                black_box(self.jobs.poll().len());
            }
            us[2] = t.elapsed().as_secs_f64() * 1e6;
        }
        if load.gate {
            let t = std::time::Instant::now();
            black_box(validate_asset(&self.asset, &self.limits).is_ok());
            us[3] = t.elapsed().as_secs_f64() * 1e6;
        }
        us
    }
}

struct Rig {
    #[cfg_attr(not(feature = "rigid3d"), allow(dead_code))]
    world: World,
    #[cfg(feature = "rigid3d")]
    sys: oriera_subsys::MultibodySystem,
    #[cfg(not(feature = "rigid3d"))]
    #[allow(dead_code)]
    sys: (),
    store: MemoryStore,
    jobs: InferJobs<RuleProvider>,
    asset: GeneratedAsset,
    limits: ValidationLimits,
    frame: u64,
}

fn rig(props: usize, memories: usize) -> Rig {
    #[cfg(feature = "rigid3d")]
    let (world, sys) = physics_world(props);
    #[cfg(not(feature = "rigid3d"))]
    let (world, sys) = (World::new(), ());
    let _ = props;
    Rig {
        world,
        sys,
        store: memory_store(memories),
        jobs: jobs(),
        asset: glb_asset(),
        limits: ValidationLimits::default(),
        frame: 1,
    }
}

impl Rig {
    fn tick(&mut self, load: Load) -> [f64; 4] {
        self.frame += 1;
        self.run_frame(load)
    }
}

// ── criterion：整机与逐段边际 ──────────────────────────────────────

fn bench_world_frame(c: &mut Criterion) {
    let mut g = c.benchmark_group("world_frame");
    // 整机采样成本高（含线程 spawn），降低采样数守住 CI 时间。
    g.sample_size(20);

    // 规模档：props / 记忆窗口。具身训练的典型小型场景起步。
    for &(props, mem) in &[(4usize, 60usize), (16, 120), (16, 240)] {
        g.bench_with_input(
            BenchmarkId::new("full", format!("p{props}_m{mem}")),
            &(props, mem),
            |b, &(props, mem)| {
                let mut r = rig(props, mem);
                b.iter(|| black_box(r.tick(Load::FULL)))
            },
        );
    }

    // 差值法：整机 vs 去掉某一段。同规模 p16/m120。
    let variants: [(&str, Load); 5] = [
        ("full", Load::FULL),
        (
            "no_physics",
            Load {
                physics: false,
                ..Load::FULL
            },
        ),
        (
            "no_memory",
            Load {
                memory: false,
                ..Load::FULL
            },
        ),
        (
            "no_infer",
            Load {
                infer: false,
                ..Load::FULL
            },
        ),
        (
            "no_gate",
            Load {
                gate: false,
                ..Load::FULL
            },
        ),
    ];
    for (name, load) in variants {
        g.bench_function(format!("marginal_p16_m120/{name}"), |b| {
            let mut r = rig(16, 120);
            b.iter(|| black_box(r.tick(load)))
        });
    }

    // 空转基线：四段全关，量夹具与计时本身的开销。
    g.bench_function("idle_baseline", |b| {
        let mut r = rig(16, 120);
        b.iter(|| black_box(r.tick(Load::NONE)))
    });

    g.finish();
}

// ── 诊断：各段占比与预算结论 ───────────────────────────────────────

/// 跑 N 帧取中位数，打印各段占比。
///
/// 用 `--nocapture breakdown` 触发；不带该参数时静默跳过，
/// 不干扰 criterion 的统计输出。
fn bench_breakdown(c: &mut Criterion) {
    if !std::env::args().any(|a| a == "breakdown") {
        return;
    }
    const FRAMES: usize = 300;

    println!("\n══════ 整机帧成本分解（中位数，µs）══════");
    println!("帧预算 60fps = {FRAME_BUDGET_US:.0} µs\n");
    println!(
        "{:<14} {:>10} {:>10} {:>10} {:>10} {:>10} {:>8}",
        "规模", "physics", "memory", "infer", "gate", "合计", "占预算"
    );

    for &(props, mem) in &[(4usize, 60usize), (16, 120), (16, 240)] {
        let mut r = rig(props, mem);
        let mut samples: Vec<[f64; 4]> = Vec::with_capacity(FRAMES);
        // 预热 30 帧，避开首帧建刚体与线程池冷启动。
        for _ in 0..30 {
            r.tick(Load::FULL);
        }
        for _ in 0..FRAMES {
            samples.push(r.tick(Load::FULL));
        }
        let med = |i: usize| {
            let mut v: Vec<f64> = samples.iter().map(|s| s[i]).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        let seg = [med(0), med(1), med(2), med(3)];
        let total: f64 = seg.iter().sum();
        println!(
            "{:<14} {:>10.1} {:>10.1} {:>10.1} {:>10.1} {:>10.1} {:>7.1}%",
            format!("p{props}_m{mem}"),
            seg[0],
            seg[1],
            seg[2],
            seg[3],
            total,
            total / FRAME_BUDGET_US * 100.0
        );
    }

    // infer 段的并发放大：一帧内提交 n 个 job。
    println!("\n────── infer 段：一帧内并发提交 n 个 job（µs）──────");
    for n in [1usize, 4, 8, 16] {
        let mut j = InferJobs::new(Arc::new(RuleProvider::new()), 16);
        let mut samples = Vec::new();
        for _ in 0..60 {
            let t = std::time::Instant::now();
            for _ in 0..n {
                let _ = j.submit(prompt());
            }
            while j.inflight() > 0 {
                black_box(j.poll().len());
                std::hint::spin_loop();
            }
            black_box(j.poll().len());
            samples.push(t.elapsed().as_secs_f64() * 1e6);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let m = samples[samples.len() / 2];
        println!(
            "n={n:<3} 中位 {:>8.1} µs   每 job {:>7.1} µs   占预算 {:>5.1}%",
            m,
            m / n as f64,
            m / FRAME_BUDGET_US * 100.0
        );
    }
    println!("\n注：内部基线，未标定硬件与构建形态前不对外。\n");

    // ── 对照：复用池 vs 每次新建池 ──
    // 孤立的 infer_jobs 基准用 iter_batched 每轮新建 InferJobs；整机帧里
    // 池是长期复用的。两者若差异显著，说明"每 job 45 µs"那个读数里
    // 混进了冷启动成分，不能直接当稳态帧成本用。
    println!("────── 对照：池复用 vs 每轮新建（n=1，中位 µs）──────");
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let drain = |j: &mut InferJobs<RuleProvider>| {
        while j.inflight() > 0 {
            black_box(j.poll().len());
            std::hint::spin_loop();
        }
        black_box(j.poll().len());
    };

    let mut reused = InferJobs::new(Arc::new(RuleProvider::new()), 8);
    for _ in 0..50 {
        let _ = reused.submit(prompt());
        drain(&mut reused);
    }
    let mut warm = Vec::new();
    for _ in 0..200 {
        let t = std::time::Instant::now();
        let _ = reused.submit(prompt());
        drain(&mut reused);
        warm.push(t.elapsed().as_secs_f64() * 1e6);
    }
    println!("复用池（预热 50 轮后）      {:>8.1}", median(warm));

    let mut cold = Vec::new();
    for _ in 0..200 {
        let mut fresh = InferJobs::new(Arc::new(RuleProvider::new()), 8);
        let t = std::time::Instant::now();
        let _ = fresh.submit(prompt());
        drain(&mut fresh);
        cold.push(t.elapsed().as_secs_f64() * 1e6);
    }
    println!("每轮新建池（仅计 submit+drain）{:>8.1}", median(cold));
    println!();

    // ── 规模外推：记忆窗口放大到什么时候真的撑不住 ──
    // 检索是全量扫描，成本线性于窗口。小窗口下它便宜，但具身训练 /
    // 长期陪伴的记忆规模远不止 240 条。这里找出跨越预算的量级。
    println!("────── 记忆窗口外推（retrieve 中位 µs，k=8）──────");
    for n in [240usize, 1_000, 5_000, 20_000, 50_000] {
        let store = memory_store(n);
        let mut v = Vec::new();
        for f in 0..40 {
            let t = std::time::Instant::now();
            black_box(
                store
                    .retrieve("npc", "forge", "observation forge", 1_000 + f, 8)
                    .len(),
            );
            v.push(t.elapsed().as_secs_f64() * 1e6);
        }
        let m = median(v);
        println!(
            "n={n:<6} {:>9.1} µs   占预算 {:>6.2}%   {}",
            m,
            m / FRAME_BUDGET_US * 100.0,
            if m > FRAME_BUDGET_US {
                "← 单检索已超整帧预算"
            } else {
                ""
            }
        );
    }
    println!();

    // 占位 group，保持 criterion_main 结构完整。
    let mut g = c.benchmark_group("breakdown_noop");
    g.sample_size(10);
    g.bench_function("noop", |b| b.iter(|| black_box(1u64)));
    g.finish();
}

criterion_group!(benches, bench_world_frame, bench_breakdown);
criterion_main!(benches);
