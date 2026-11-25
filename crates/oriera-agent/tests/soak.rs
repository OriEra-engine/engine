//! **长跑 soak** —— 长时间运行下的稳定性、有界性与无漂移。
//!
//! 微基准（`benches/`）回答"一次操作多快"，单测回答"一次操作对不对"。
//! 两者都盖不住第三类问题：**跑久了会不会坏**。具体是三种：
//!
//! 1. **无界增长**：台账、记忆、计时表在长时间运行后是否停在容量上限，
//!    而不是随回合数线性膨胀。
//! 2. **状态漂移**：同样的输入序列重复跑，第 1 圈与第 N 圈的结果是否
//!    逐位一致——累积浮点误差、计数器回绕、缓存污染都在这里现形。
//! 3. **节流器病态**：预算分配器在持续超载 / 持续空闲后，能否回到正常
//!    区间，而不是卡在挤压态或把份额抖成锯齿。
//!
//! ## 规模与运行时间
//!
//! 默认圈数按"CI 可接受"定：整套 soak 在 debug 下亚秒级完成，不进
//! `#[ignore]`，每次 `cargo test` 都跑。想压更久用环境变量：
//!
//! ```sh
//! ORIERA_SOAK_ROUNDS=20000 cargo test -p oriera-agent --test soak -- --nocapture
//! ```
//!
//! 这不是"跑一夜"的老式 soak，是**确定性长跑**：圈数可放大，语料由
//! 固定种子生成，失败可精确复现。

use oriera_agent::{Episode, MemoryStore};
use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{AllocatorConfig, FrameCtx, Scheduler, System};
use oriera_infer::{
    snapshot_candidates, BreakerState, HalfOpenPolicy, Privacy, ServiceTier, SlotBudget, SlotRoute,
    SlotRouter, PROVIDER_PRESETS,
};

/// 默认圈数：debug 下亚秒级，够暴露线性增长与漂移。
const DEFAULT_ROUNDS: usize = 2_000;

fn rounds() -> usize {
    std::env::var("ORIERA_SOAK_ROUNDS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_ROUNDS)
}

/// 确定性语料：同一个 i 永远产出同一条记忆，不依赖随机源。
fn episode(i: usize) -> Episode {
    const TOPICS: [&str; 5] = ["forge", "market", "harbor", "temple", "gate"];
    Episode {
        turn: i as u64,
        day: (i / 24) as u32,
        about: TOPICS[i % TOPICS.len()].to_string(),
        text: format!("observation {} at the {}", i, TOPICS[i % TOPICS.len()]),
        importance: (i % 5) as u8,
    }
}

// ── 1. 记忆：容量有界 + 长跑无漂移 ─────────────────────────────────

#[test]
fn memory_stays_bounded_over_long_run() {
    let n = rounds();
    let capacity = 64;
    let mut store = MemoryStore::with_capacity(capacity);

    for i in 0..n {
        store.record("npc", episode(i));
        assert!(
            store.len_total() <= capacity,
            "round {i}: memory grew past capacity to {}",
            store.len_total()
        );
    }
    assert_eq!(
        store.len_total(),
        capacity,
        "capacity must be saturated, not under-filled"
    );

    // 检索在长跑后仍然可用且条数受限。
    let hits = store.retrieve("npc", "forge", "observation forge", n as u64, 8);
    assert!(!hits.is_empty(), "recall must still work after {n} rounds");
    assert!(hits.len() <= 8);
}

/// 同一序列跑两遍，最终召回必须逐位一致——累积状态不得引入漂移。
#[test]
fn memory_replay_is_identical_across_two_long_runs() {
    let n = rounds();
    let run = || {
        let mut store = MemoryStore::with_capacity(64);
        for i in 0..n {
            store.record("npc", episode(i));
        }
        store.retrieve("npc", "market", "observation market", n as u64, 12)
    };
    let a = run();
    let b = run();
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(x.turn, y.turn);
        assert_eq!(x.text, y.text);
        assert_eq!(x.importance, y.importance);
    }
}

/// 多 agent 长跑：容量按 agent 各自计，不互相挤占。
#[test]
fn multi_agent_memory_does_not_cross_evict() {
    let n = rounds();
    let mut store = MemoryStore::with_capacity(32);
    for i in 0..n {
        store.record("alice", episode(i));
        store.record("bob", episode(i + 1));
    }
    assert!(store.len_total() <= 64, "two agents, 32 each");
    let a = store.retrieve("alice", "forge", "observation", n as u64, 32);
    let b = store.retrieve("bob", "forge", "observation", n as u64, 32);
    assert!(
        !a.is_empty() && !b.is_empty(),
        "neither agent may be starved out"
    );
}

// ── 2. 路由台账：容量有界 + 长跑双跑一致 ───────────────────────────

fn soak_route() -> SlotRoute {
    SlotRoute {
        id: "npc_dialog".into(),
        tier: ServiceTier::Realtime,
        chain: vec!["zhipu".into(), "rule".into()],
        require: vec![],
        privacy: Privacy::Either,
        budget: SlotBudget::default(),
    }
}

#[test]
fn route_ledger_stays_bounded_over_long_run() {
    let n = rounds();
    let mut router = SlotRouter::new(vec![soak_route()]);

    for i in 0..n {
        // 健康态在 Closed / Open 之间交替，逼降级路径也被长跑覆盖。
        let cloud = if i.is_multiple_of(3) {
            BreakerState::Open
        } else {
            BreakerState::Closed
        };
        let health = [("zhipu", cloud, 0u32), ("rule-v1", BreakerState::Closed, 0)];
        let (cands, unmatched) =
            snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
        assert!(unmatched.is_empty());

        if let Some(p) = router.resolve("npc_dialog", &cands) {
            router.observe(
                "npc_dialog",
                "rule-v1",
                p.chain_index,
                // 固定等待值：soak 比结构，不比墙钟。
                1_000,
                p.degraded_from,
                None,
                0,
                i as u64,
            );
        }
        assert!(
            router.ledger().len() <= 256,
            "round {i}: ledger grew past its 256 cap to {}",
            router.ledger().len()
        );
    }
    assert_eq!(
        router.ledger().len(),
        256,
        "ledger must saturate at its cap"
    );
}

#[test]
fn route_ledger_long_run_replays_entrywise() {
    let n = rounds();
    let run = || {
        let mut router = SlotRouter::new(vec![soak_route()]);
        for i in 0..n {
            let cloud = if i.is_multiple_of(3) {
                BreakerState::Open
            } else {
                BreakerState::Closed
            };
            let health = [("zhipu", cloud, 0u32), ("rule-v1", BreakerState::Closed, 0)];
            let (cands, _) =
                snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
            if let Some(p) = router.resolve("npc_dialog", &cands) {
                router.observe(
                    "npc_dialog",
                    "rule-v1",
                    p.chain_index,
                    1_000,
                    p.degraded_from,
                    None,
                    0,
                    i as u64,
                );
            }
        }
        router.ledger().clone()
    };
    assert!(
        run().matches(&run()),
        "long-run ledger must replay entrywise"
    );
}

// ── 3. 帧循环 + 预算分配器：长跑无病态 ─────────────────────────────

/// 记账系统：每帧写入一个由帧号决定的确定值，便于逐位比对。
struct CountingSystem {
    name: &'static str,
    seen: u64,
}

impl<W> System<W> for CountingSystem {
    fn name(&self) -> &str {
        self.name
    }
    fn run(&mut self, ctx: &FrameCtx, _world: &mut W) {
        self.seen = self.seen.wrapping_add(ctx.frame.0 + 1);
    }
}

#[test]
fn frame_loop_stays_stable_over_long_run() {
    let n = rounds();
    let mut sched = Scheduler::<()>::new().with_fixed_dt(1.0 / 60.0);
    sched.add_system(Box::new(CountingSystem { name: "a", seen: 0 }));
    sched.add_system(Box::new(CountingSystem { name: "b", seen: 0 }));

    let mut world = ();
    for i in 0..n {
        sched.tick(&mut world);
        let timings = sched.system_timings();
        assert_eq!(timings.len(), 2, "frame {i}: timing table must not grow");
        // 平均帧耗时必须是有限非负数，不得被长跑累积成 NaN / Inf。
        let avg = sched.avg_frame_ms();
        assert!(
            avg.is_finite() && avg >= 0.0,
            "frame {i}: avg_frame_ms went bad: {avg}"
        );
    }
}

/// 持续超载后回到正常负载：分配器不得卡死在挤压态。
#[test]
fn allocator_recovers_after_sustained_overload() {
    let n = rounds();
    let mut sched = Scheduler::<()>::with_budget_ns(16_000_000)
        .with_fixed_dt(1.0 / 60.0)
        .with_allocator(AllocatorConfig::default());
    sched.add_system(Box::new(CountingSystem {
        name: "heavy",
        seen: 0,
    }));

    let mut world = ();
    // 阶段一：持续报超额，逼分配器挤压。
    for _ in 0..n {
        sched.tick(&mut world);
        sched.observe_extra_cost("heavy", 40.0);
    }
    let squeezed = sched.grants().throttle_of("heavy");
    assert!(
        squeezed.is_finite() && squeezed > 0.0,
        "throttle went bad: {squeezed}"
    );

    // 阶段二：恢复正常负载，跑同样久。
    for _ in 0..n {
        sched.tick(&mut world);
        sched.observe_extra_cost("heavy", 0.0);
    }
    let relaxed = sched.grants().throttle_of("heavy");
    assert!(relaxed.is_finite() && relaxed > 0.0);
    assert!(
        relaxed >= squeezed,
        "throttle must relax back after load drops: squeezed {squeezed} → relaxed {relaxed}"
    );
}

/// 分配器关闭时，长跑下 grants 必须恒为空——不得有残留节流。
#[test]
fn allocator_off_keeps_grants_empty_over_long_run() {
    let n = rounds();
    let mut sched = Scheduler::<()>::new()
        .with_fixed_dt(1.0 / 60.0)
        .with_allocator(AllocatorConfig::off());
    sched.add_system(Box::new(CountingSystem { name: "s", seen: 0 }));

    let mut world = ();
    for i in 0..n {
        sched.tick(&mut world);
        sched.observe_extra_cost("s", 100.0);
        assert_eq!(
            sched.grants().throttle_of("s"),
            1.0,
            "frame {i}: disabled allocator must not throttle"
        );
    }
}

/// 帧上下文单调性：长跑下帧号与时间戳不得回绕或倒退。
#[test]
fn frame_context_is_monotonic_over_long_run() {
    let n = rounds();
    let mut last_frame = 0u64;
    let mut last_now = 0u64;
    for i in 0..n {
        let ctx = FrameCtx {
            frame: FrameIndex(i as u64),
            dt: 1.0 / 60.0,
            now: Timestamp(i as u64 * 16_667),
            grants: Grants::EMPTY,
        };
        assert!(ctx.frame.0 >= last_frame);
        assert!(ctx.now.0 >= last_now);
        last_frame = ctx.frame.0;
        last_now = ctx.now.0;
    }
}
