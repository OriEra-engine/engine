//! RFC 0003 rule 档**双跑 harness** —— #13 步 C 接云链前的硬前置。
//!
//! 与 `router.rs` / `candidates.rs` 的单测分工：那两处各自钉自己那一层的
//! 口径；本文件钉**整条链接起来之后**是否仍然逐位可复现：
//!
//! ```text
//! health_report ──► snapshot_candidates ──► resolve ──► registry ──► observe
//!   (实例 id)         (preset id 翻译)       (纯决策)    (取实例)     (台账)
//! ```
//!
//! 三条纪律对应三组断言：
//!
//! 1. **双跑逐位一致**：同健康快照 + 同 preset 表 + 同槽位表，跑两遍，
//!    `ResolvedPick` 与整本 `RouteLedger` 必须逐条相等。等待微秒在 harness
//!    里喂固定值——真实墙钟不参与确定性比对（`RouteDecision::waited_us`
//!    的既有口径）。
//! 2. **顺序无关**：候选快照乱序不得改变选链结果（`resolve` 按 chain 序
//!    匹配 id，不按候选序）。
//! 3. **fail closed 贯通**：未声明实例、全熔断、未知槽位、空链，一律
//!    `None` 且不落账——不静默兜底、不臆造能力。
//!
//! 本 harness **不发起任何模型调用**：registry 里放的是离线 `RuleProvider`，
//! 只验证"决策出的 id 能取到实例"，不调 `complete`。接真实远程后端是步 C
//! 之后的事，届时本文件是回归基线。

use oriera_infer::{
    snapshot_candidates, BreakerState, CapNeed, ChainCandidate, Exhausted, HalfOpenPolicy, Privacy,
    ProviderRegistry, ResolvedPick, RuleProvider, ServiceTier, SlotBudget, SlotRoute, SlotRouter,
    PROVIDER_PRESETS,
};
use std::sync::Arc;

/// 离线兜底槽位：链首云端行、链尾 rule 行（rule 永不熔断）。
///
/// **不要求 tools**：preset 表里 `rule` 行的 caps 是
/// `ModelCaps::UNREGISTERED`（`tools: false`）——离线后端确实不支持工具
/// 调用。要求 tools 的槽位由 [`tools_route`] 单独覆盖，那条链上 rule
/// 会被能力闸正确拒掉（诚实深度：不假装它有这个能力）。
fn dialogue_route() -> SlotRoute {
    SlotRoute {
        id: "npc_dialog".into(),
        tier: ServiceTier::Realtime,
        chain: vec!["zhipu".into(), "rule".into()],
        require: vec![],
        privacy: Privacy::Either,
        budget: SlotBudget {
            max_inflight: 2,
            max_ms_p95: Some(600),
            max_micro_per_1k_turns: Some(200_000),
        },
    }
}

/// 显式要求工具调用的槽位：rule 行 caps 未登记 tools，能力闸应拒。
fn tools_route() -> SlotRoute {
    SlotRoute {
        id: "tool_agent".into(),
        require: vec![CapNeed::Tools],
        ..dialogue_route()
    }
}

/// 本地隐私槽位：只走本地开源行。
fn local_route() -> SlotRoute {
    SlotRoute {
        id: "companion_recall".into(),
        tier: ServiceTier::Balanced,
        chain: vec!["zhipu".into(), "rule".into()],
        require: vec![],
        privacy: Privacy::Local,
        budget: SlotBudget::default(),
    }
}

/// 装配期注册表：**按 preset id 登记**（与 chain / 候选同一 id 空间），
/// 实例本身的 `id()` 仍是 `rule-v1` —— 两个空间各归各位。
fn registry() -> ProviderRegistry {
    let mut reg = ProviderRegistry::new();
    for preset in PROVIDER_PRESETS
        .iter()
        .filter(|p| p.id == "rule" || p.id == "zhipu")
    {
        assert!(reg.register(
            preset.id,
            Arc::new(RuleProvider::new()),
            preset.local,
            preset.caps,
        ));
    }
    reg
}

/// 一次完整决策 + 回填。返回 (pick, 台账克隆)。
///
/// `waited_us` 由调用方喂固定值：harness 比的是决策与台账结构，
/// 不是墙钟。
fn decide_and_record(
    slot: &str,
    health: &[(&str, BreakerState, u32)],
    policy: HalfOpenPolicy,
    waited_us: u64,
) -> (Option<ResolvedPick>, oriera_infer::RouteLedger) {
    let mut router = SlotRouter::new(vec![dialogue_route(), local_route()]);
    let (candidates, unmatched) = snapshot_candidates(health, PROVIDER_PRESETS, policy);
    assert!(
        unmatched.is_empty(),
        "harness fixtures must all carry a preset declaration; got {unmatched:?}"
    );

    let pick = router.resolve(slot, &candidates);
    if let Some(p) = &pick {
        // 包装层将来要做的回填：实例 id 进台账，preset id 只用于选链。
        let reg = registry();
        let instance = reg
            .get(&p.provider_id)
            .expect("resolved preset id must exist in the assembly registry");
        router.observe(
            slot,
            instance.id(),
            p.chain_index,
            waited_us,
            p.degraded_from.clone(),
            None,
            0,
            42,
        );
    }
    (pick, router.ledger().clone())
}

// ── 1. 双跑逐位一致 ────────────────────────────────────────────────

#[test]
fn healthy_chain_double_run_is_bit_identical() {
    let health = [
        ("zhipu", BreakerState::Closed, 0u32),
        ("rule-v1", BreakerState::Closed, 0),
    ];
    let a = decide_and_record("npc_dialog", &health, HalfOpenPolicy::default(), 5_000);
    let b = decide_and_record("npc_dialog", &health, HalfOpenPolicy::default(), 5_000);
    assert_eq!(a.0, b.0, "ResolvedPick must replay identically");
    assert!(a.1.matches(&b.1), "the whole ledger must replay entrywise");
    assert_eq!(a.1.len(), 1);
}

#[test]
fn degraded_chain_double_run_is_bit_identical() {
    // 云端行熔断 → 降级到 rule；degraded_from 记首项。
    let health = [
        ("zhipu", BreakerState::Open, 3u32),
        ("rule-v1", BreakerState::Closed, 0),
    ];
    let a = decide_and_record("npc_dialog", &health, HalfOpenPolicy::default(), 9_000);
    let b = decide_and_record("npc_dialog", &health, HalfOpenPolicy::default(), 9_000);
    assert_eq!(a.0, b.0);
    assert!(a.1.matches(&b.1));

    let pick = a.0.expect("rule tail must save the chain");
    assert_eq!(pick.provider_id, "rule");
    assert_eq!(pick.chain_index, 1);
    assert_eq!(pick.degraded_from.as_deref(), Some("zhipu"));
}

#[test]
fn multi_slot_decision_stream_replays_entrywise() {
    let health = [
        ("zhipu", BreakerState::Open, 2u32),
        ("rule-v1", BreakerState::Closed, 0),
    ];
    let run = || {
        let mut router = SlotRouter::new(vec![dialogue_route(), local_route()]);
        let (cands, _) = snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
        for (slot, waited) in [("npc_dialog", 1_000u64), ("companion_recall", 2_000)] {
            if let Some(p) = router.resolve(slot, &cands) {
                router.observe(
                    slot,
                    &p.provider_id,
                    p.chain_index,
                    waited,
                    p.degraded_from,
                    None,
                    0,
                    7,
                );
            }
        }
        router.ledger().clone()
    };
    let a = run();
    let b = run();
    assert!(a.matches(&b), "multi-slot stream must replay entrywise");
    assert_eq!(a.len(), 2, "both slots resolve to the local rule tail");
}

// ── 2. 顺序无关 ────────────────────────────────────────────────────

#[test]
fn candidate_order_does_not_change_the_pick() {
    let forward = [
        ("zhipu", BreakerState::Closed, 0u32),
        ("rule-v1", BreakerState::Closed, 0),
    ];
    let reversed = [
        ("rule-v1", BreakerState::Closed, 0u32),
        ("zhipu", BreakerState::Closed, 0),
    ];
    let router = SlotRouter::new(vec![dialogue_route()]);
    let (f, _) = snapshot_candidates(&forward, PROVIDER_PRESETS, HalfOpenPolicy::default());
    let (r, _) = snapshot_candidates(&reversed, PROVIDER_PRESETS, HalfOpenPolicy::default());
    assert_ne!(
        f.iter().map(|c| &c.provider_id).collect::<Vec<_>>(),
        r.iter().map(|c| &c.provider_id).collect::<Vec<_>>(),
        "snapshots follow health order, so these two differ"
    );
    assert_eq!(
        router.resolve("npc_dialog", &f),
        router.resolve("npc_dialog", &r),
        "chain order decides, not candidate order"
    );
}

// ── 3. fail closed 贯通 ────────────────────────────────────────────

#[test]
fn all_breakers_open_resolves_none_and_records_nothing() {
    let router = SlotRouter::new(vec![dialogue_route()]);
    let health = [
        ("zhipu", BreakerState::Open, 3u32),
        ("rule-v1", BreakerState::Open, 3),
    ];
    let (cands, _) = snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
    assert!(router.resolve("npc_dialog", &cands).is_none());
    assert_eq!(router.ledger().len(), 0, "no decision, no ledger row");
}

#[test]
fn undeclared_instance_is_dropped_and_reported_not_guessed() {
    let health = [
        ("ghost-provider", BreakerState::Closed, 0u32),
        ("rule-v1", BreakerState::Closed, 0),
    ];
    let (cands, unmatched) =
        snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
    assert_eq!(unmatched, vec!["ghost-provider".to_string()]);
    assert_eq!(
        cands.len(),
        1,
        "undeclared instance never enters the candidate view"
    );

    // 链上没有 ghost，所以它的缺席不改变结果；rule 仍然兜住。
    let router = SlotRouter::new(vec![dialogue_route()]);
    let pick = router
        .resolve("npc_dialog", &cands)
        .expect("rule tail serves");
    assert_eq!(pick.provider_id, "rule");
}

#[test]
fn unknown_slot_and_empty_chain_resolve_none() {
    let health = [("rule-v1", BreakerState::Closed, 0u32)];
    let (cands, _) = snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());

    let router = SlotRouter::new(vec![dialogue_route()]);
    assert!(router.resolve("no_such_slot", &cands).is_none());

    let empty = SlotRouter::new(vec![SlotRoute {
        id: "empty".into(),
        chain: vec![],
        ..dialogue_route()
    }]);
    assert!(empty.resolve("empty", &cands).is_none());
}

#[test]
fn privacy_gate_survives_the_snapshot_translation() {
    // companion_recall 只走本地：云端行即使健康也必须被跳过。
    let health = [
        ("zhipu", BreakerState::Closed, 0u32),
        ("rule-v1", BreakerState::Closed, 0),
    ];
    let (cands, _) = snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
    let router = SlotRouter::new(vec![dialogue_route(), local_route()]);
    let pick = router
        .resolve("companion_recall", &cands)
        .expect("local rule row serves the privacy slot");
    assert_eq!(pick.provider_id, "rule");
    assert_eq!(pick.degraded_from.as_deref(), Some("zhipu"));
}

// ── 4. id 空间：本次接线的关键回归 ─────────────────────────────────

#[test]
fn instance_id_translates_to_preset_id_so_the_chain_actually_matches() {
    // 这条是 harness 存在的理由：`health_report` 报实例 id `rule-v1`，
    // chain 写 preset id `rule`。若候选视图直接沿用实例 id，resolve 会
    // 找不到任何匹配、静默返回 None —— 单测各自全绿也发现不了。
    let health = [("rule-v1", BreakerState::Closed, 0u32)];
    let (cands, unmatched) =
        snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
    assert!(unmatched.is_empty(), "declaration lookup must succeed");
    assert_eq!(
        cands[0].provider_id, "rule",
        "must be translated to the chain space"
    );

    let router = SlotRouter::new(vec![SlotRoute {
        id: "only_rule".into(),
        chain: vec!["rule".into()],
        require: vec![],
        ..dialogue_route()
    }]);
    let pick = router
        .resolve("only_rule", &cands)
        .expect("preset-id candidate must match the preset-id chain entry");
    assert_eq!(pick.provider_id, "rule");
    assert_eq!(pick.chain_index, 0);
    assert!(pick.degraded_from.is_none());
}

#[test]
fn registry_lookup_uses_preset_id_while_ledger_keeps_instance_id() {
    let health = [("rule-v1", BreakerState::Closed, 0u32)];
    let (pick, ledger) = decide_and_record("npc_dialog", &health, HalfOpenPolicy::default(), 3_000);
    let pick = pick.expect("rule tail serves");

    // 注册表按 preset id 取得到实例。
    let reg = registry();
    let instance = reg
        .get(&pick.provider_id)
        .expect("preset id is the registry key");
    // 台账记的是实例 id —— 两个空间不混用。
    assert_eq!(instance.id(), "rule-v1");
    let row = &ledger.entries()[0];
    assert_eq!(row.provider_id, "rule-v1", "ledger carries the instance id");
    assert_eq!(row.chain_index, 1);
}

// ── 5. HalfOpen 策略在整条链上的可观测差异 ─────────────────────────

#[test]
fn half_open_policy_changes_the_pick_observably() {
    let health = [
        ("zhipu", BreakerState::HalfOpen, 1u32),
        ("rule-v1", BreakerState::Closed, 0),
    ];
    let router = SlotRouter::new(vec![dialogue_route()]);

    // 方案甲（默认，与 admits() 一致）：半开放行 → 云端行仍被选中。
    let (probe, _) =
        snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::ProbeAsAvailable);
    let a = router
        .resolve("npc_dialog", &probe)
        .expect("probe admits half-open");
    assert_eq!(a.provider_id, "zhipu");
    assert_eq!(a.chain_index, 0);

    // 方案乙：半开跳过 → 降级到 rule。
    let (skip, _) = snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::TreatAsOpen);
    let b = router
        .resolve("npc_dialog", &skip)
        .expect("rule tail serves");
    assert_eq!(b.provider_id, "rule");
    assert_eq!(b.degraded_from.as_deref(), Some("zhipu"));
}

// ── 6. 台账边界：越界 chain_index 不落账 ───────────────────────────

#[test]
fn out_of_range_chain_index_is_refused_by_observe() {
    let mut router = SlotRouter::new(vec![dialogue_route()]);
    // dialogue_route 链长 2；index 2 越界。
    assert!(router
        .observe(
            "npc_dialog",
            "rule-v1",
            2,
            1_000,
            None,
            Some(Exhausted::Deadline),
            0,
            1
        )
        .is_none());
    assert_eq!(router.ledger().len(), 0);
}

#[test]
fn snapshot_of_empty_health_yields_no_candidates() {
    let (cands, unmatched) = snapshot_candidates(&[], PROVIDER_PRESETS, HalfOpenPolicy::default());
    assert!(cands.is_empty());
    assert!(unmatched.is_empty());
    let router = SlotRouter::new(vec![dialogue_route()]);
    assert!(router.resolve("npc_dialog", &cands).is_none());
}

/// 候选视图的 caps 必须来自 preset 表，而不是被 harness 臆造。
#[test]
fn candidate_caps_come_from_the_preset_table() {
    let health = [("rule-v1", BreakerState::Closed, 0u32)];
    let (cands, _) = snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
    let preset = PROVIDER_PRESETS.iter().find(|p| p.id == "rule").unwrap();
    let c: &ChainCandidate = &cands[0];
    assert_eq!(c.caps, preset.caps);
    assert_eq!(c.local, preset.local);
}

/// 能力闸贯通：`rule` 行 caps 未登记 tools，要求 tools 的槽位必须
/// 跳过它 —— 诚实深度，不假装离线后端支持工具调用。
#[test]
fn tools_requirement_rejects_the_unregistered_rule_row() {
    let health = [
        ("zhipu", BreakerState::Open, 3u32),
        ("rule-v1", BreakerState::Closed, 0),
    ];
    let (cands, _) = snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
    let router = SlotRouter::new(vec![dialogue_route(), tools_route()]);

    // 云端行熔断、rule 行能力不足 → 整链耗尽。
    assert!(
        router.resolve("tool_agent", &cands).is_none(),
        "rule row declares no tools; the gate must not pretend otherwise"
    );
    // 同一份候选在不要求 tools 的槽位上仍能兜底。
    assert_eq!(
        router.resolve("npc_dialog", &cands).unwrap().provider_id,
        "rule"
    );
}
