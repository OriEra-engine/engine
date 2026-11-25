//! RFC 0003 选链决策微基准 — `SlotRouter::resolve` 纯函数开销。
//!
//! 三类场景，与 `router.rs` 单测同口径：
//! - `resolve/chain_len_{n}`：链长梯度（2/4/8），全部可用，首项即中；
//! - `resolve/degraded_k{n}`：链长 8，前 n 项熔断/缺能力，决策需下探到第 n+1 项；
//! - `resolve/all_unavailable`：链长 8，全链不可用 → `None`（fail closed 路径）。
//!
//! `resolve` 不持 provider 实例、不记台账、不发起调用，所以这里量到的就是
//! 决策本身（查表 + 三闸判定 + 分配 `ResolvedPick`）的成本，可直接作
//! 包装层接线（#13 步 C）前的基线。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_infer::{
    ChainCandidate, ModelCaps, Privacy, ServiceTier, SlotBudget, SlotRoute, SlotRouter,
};

fn candidate(id: &str, tools: bool, breaker_open: bool) -> ChainCandidate {
    ChainCandidate {
        provider_id: id.to_string(),
        caps: ModelCaps {
            context_window: Some(8192),
            structured_output: false,
            tools,
            image_input: false,
            prompt_cache: false,
        },
        local: false,
        breaker_open,
    }
}

fn route_with_chain(n: usize) -> SlotRoute {
    SlotRoute {
        id: "npc_dialog".into(),
        tier: ServiceTier::Realtime,
        chain: (0..n).map(|i| format!("p{i}")).collect(),
        require: vec![oriera_infer::CapNeed::Tools],
        privacy: Privacy::Either,
        budget: SlotBudget {
            max_inflight: 2,
            max_ms_p95: Some(600),
            max_micro_per_1k_turns: Some(200_000),
        },
    }
}

fn candidates_for(n: usize, dead_prefix: usize) -> Vec<ChainCandidate> {
    (0..n)
        .map(|i| candidate(&format!("p{i}"), true, i < dead_prefix))
        .collect()
}

fn bench_resolve(c: &mut Criterion) {
    let mut group = c.benchmark_group("router_resolve");

    // ── 链长梯度：全可用，首项即中 ──
    for n in [2usize, 4, 8] {
        let router = SlotRouter::new(vec![route_with_chain(n)]);
        let cands = candidates_for(n, 0);
        group.bench_with_input(BenchmarkId::new("chain_len", n), &n, |b, _| {
            b.iter(|| {
                let pick = router.resolve(black_box("npc_dialog"), black_box(&cands));
                black_box(pick)
            })
        });
    }

    // ── 降级下探：前 k 项不可用，决策停在第 k+1 项 ──
    let router8 = SlotRouter::new(vec![route_with_chain(8)]);
    for dead in [1usize, 3, 7] {
        let cands = candidates_for(8, dead);
        group.bench_with_input(BenchmarkId::new("degraded_k", dead), &dead, |b, _| {
            b.iter(|| {
                let pick = router8.resolve(black_box("npc_dialog"), black_box(&cands));
                black_box(pick)
            })
        });
    }

    // ── 全链不可用：fail closed → None ──
    let all_dead = candidates_for(8, 8);
    group.bench_function("all_unavailable", |b| {
        b.iter(|| {
            let pick = router8.resolve(black_box("npc_dialog"), black_box(&all_dead));
            black_box(pick)
        })
    });

    group.finish();
}

criterion_group!(benches, bench_resolve);
criterion_main!(benches);
