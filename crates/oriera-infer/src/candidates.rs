//! RFC 0003 v1-b-2 步 B — 装配期候选快照构建（纯函数）。
//!
//! 职责：把运行期的**熔断健康快照**（[`FallbackProvider::health_report`] 的
//! 输出）与**preset 声明表**（[`PROVIDER_PRESETS`]）合成 `resolve` 要吃的
//! [`ChainCandidate`] 列表。它是决策层与执行层之间唯一的"视图拼装"点。
//!
//! 三条边界（评审卡这三条）：
//!
//! 1. **纯函数**：无 IO、无 `Instant`、不读熔断状态机内部、不发起调用。
//!    同健康快照 + 同 preset 表 → 逐位同一结果，可直接进双跑 harness。
//! 2. **不判健康**：`breaker_open` 只做 `BreakerState` → `bool` 的映射，
//!    开合逻辑仍归 [`oriera_infer::health`]。HalfOpen 的口径由调用方
//!    显式传入 [`HalfOpenPolicy`]（见 P1-2）。
//! 3. **缺行 fail closed**：实例 id 在 preset 表里查不到，**不进候选**
//!    （不臆造 `caps`/`local`），但**进 `unmatched` 清单**——装配层必须
//!    看得见"这个实例没有声明"，而不是被静默丢弃。
//!
//! ## id 空间不闭合这件事
//!
//! 三个命名空间必须分清，混用会让整条链静默判死：
//!
//! | 空间 | 形态 | 谁在用 |
//! |---|---|---|
//! | preset id | `rule` / `zhipu` | [`SlotRoute::chain`](crate::router::SlotRoute)、
//!   [`ChainCandidate::provider_id`](crate::router::ChainCandidate) |
//! | 实例 id | `rule-v1`（`rule` 行的默认 model 名） | [`ModelProvider::id`](crate::provider::ModelProvider::id)、
//!   [`FallbackProvider::health_report`](crate::health::FallbackProvider::health_report)、
//!   [`RouteDecision::provider_id`](crate::router::RouteDecision)（台账） |
//!
//! `health_report` 报的是**实例 id**，而 `resolve` 按 **preset id** 匹配
//! chain。本模块负责这次翻译：按"精确 preset id、再按 preset 默认 model 名"
//! 两级查找定位声明行（后者命中 `rule-v1 → rule`），**产出的
//! `provider_id` 一律是 preset id**——这是 `ChainCandidate` 的既有契约，
//! 不是可选项。实例 id 不进候选视图；包装层回填台账时再从实例上取
//! `id()`，两个空间各归各位。
//!
//! 两级都不中即 `unmatched`，绝不猜、绝不引入硬编码别名表。
//!
//! 同一 preset 的多个实例（如两个网关都声明 `zhipu`）会产出同名候选，
//! `resolve` 取链序首个；需要区分时应给它们不同的 preset 行，而不是
//! 指望候选视图替装配层裁决。

use crate::health::BreakerState;
use crate::provider::ProviderPreset;
use crate::router::ChainCandidate;

/// HalfOpen 状态的映射口径（评审 **P1-2** 的落点）。
///
/// 决策层不判状态，只读布尔视图；"半开该不该放行"是装配层的策略选择，
/// 所以做成显式参数而不是写死分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HalfOpenPolicy {
    /// 方案甲（**默认**）：HalfOpen 视作可用，`breaker_open = false`。
    ///
    /// 与 [`ProviderHealth::admits`](crate::health::ProviderHealth::admits)
    /// 的现状一致——半开本就放行一次试探。默认取它，意味着本模块落地
    /// **不改变任何既有行为**。
    #[default]
    ProbeAsAvailable,
    /// 方案乙：HalfOpen 视作不可用，`breaker_open = true`，跳过去。
    ///
    /// 保守选择：连试探流量也不走。适合"降级风暴比漏一次探活更贵"的场景。
    TreatAsOpen,
}

impl HalfOpenPolicy {
    /// 该状态在这一口径下是否应被跳过。
    fn skips(self, state: BreakerState) -> bool {
        match state {
            BreakerState::Closed => false,
            BreakerState::Open => true,
            BreakerState::HalfOpen => matches!(self, HalfOpenPolicy::TreatAsOpen),
        }
    }
}

/// 按 preset id 或 preset 默认 model 名查找声明行。
///
/// 两级查找是闭合 id 空间的手段：精确 id 优先（装配层显式声明过），
/// 其次按 model（`rule-v1 → rule`）。同 model 多行时取表内首个——表是
/// 装配期常量，顺序即声明顺序，不做额外裁决。
fn lookup<'a>(presets: &'a [ProviderPreset], id: &str) -> Option<&'a ProviderPreset> {
    if let Some(p) = presets.iter().find(|p| p.id == id) {
        return Some(p);
    }
    presets.iter().find(|p| p.model == id)
}

/// 构建候选快照。
///
/// 输入是 [`FallbackProvider::health_report`](crate::health::FallbackProvider::health_report)
/// 的原始形态 `(provider id, state, streak)`（streak 本函数不用，保留形态
/// 以免调用方多一次转换）与 preset 表。
///
/// 返回 `(candidates, unmatched)`：
/// - `candidates` 顺序与 `health` 一致（即装配链序），每项 `provider_id`
///   是**声明行的 preset id**——`ChainCandidate` 的契约要求它与
///   `SlotRoute::chain` 对齐，`resolve` 正是按它匹配。实例 id 不进这里；
/// - `unmatched` 是查不到声明的**实例 id** 清单，调用方据此报装配错误或降级。
pub fn snapshot_candidates(
    health: &[(&str, BreakerState, u32)],
    presets: &[ProviderPreset],
    policy: HalfOpenPolicy,
) -> (Vec<ChainCandidate>, Vec<String>) {
    let mut candidates = Vec::with_capacity(health.len());
    let mut unmatched = Vec::new();

    for &(id, state, _streak) in health {
        match lookup(presets, id) {
            Some(preset) => candidates.push(ChainCandidate {
                // preset id，不是实例 id：与 SlotRoute::chain 同一空间。
                provider_id: preset.id.to_string(),
                caps: preset.caps,
                local: preset.local,
                breaker_open: policy.skips(state),
            }),
            // fail closed：查不到声明就不进候选，但必须留下痕迹。
            None => unmatched.push(id.to_string()),
        }
    }

    (candidates, unmatched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::PROVIDER_PRESETS;

    fn rule_preset() -> ProviderPreset {
        *PROVIDER_PRESETS
            .iter()
            .find(|p| p.id == "rule")
            .expect("preset table carries the offline rule row")
    }

    #[test]
    fn closed_state_yields_available_candidate() {
        let (cands, unmatched) = snapshot_candidates(
            &[("rule-v1", BreakerState::Closed, 0)],
            PROVIDER_PRESETS,
            HalfOpenPolicy::default(),
        );
        assert!(
            unmatched.is_empty(),
            "rule-v1 must resolve via model-name match"
        );
        assert_eq!(cands.len(), 1);
        assert_eq!(
            cands[0].provider_id, "rule",
            "candidate id must be the PRESET id (chain space), not the instance id"
        );
        assert!(!cands[0].breaker_open);
        let preset = rule_preset();
        assert_eq!(cands[0].local, preset.local);
        assert_eq!(cands[0].caps, preset.caps);
    }

    #[test]
    fn open_state_marks_breaker_open() {
        let (cands, _) = snapshot_candidates(
            &[("rule-v1", BreakerState::Open, 3)],
            PROVIDER_PRESETS,
            HalfOpenPolicy::ProbeAsAvailable,
        );
        assert!(cands[0].breaker_open, "Open is skipped under either policy");
    }

    #[test]
    fn half_open_obeys_the_two_policies() {
        // 方案甲（默认）：放行试探 → 不算 open。
        let (probe, _) = snapshot_candidates(
            &[("rule-v1", BreakerState::HalfOpen, 1)],
            PROVIDER_PRESETS,
            HalfOpenPolicy::ProbeAsAvailable,
        );
        assert!(
            !probe[0].breaker_open,
            "ProbeAsAvailable matches admits(): half-open still gets a trial"
        );

        // 方案乙：保守跳过 → 算 open。
        let (skip, _) = snapshot_candidates(
            &[("rule-v1", BreakerState::HalfOpen, 1)],
            PROVIDER_PRESETS,
            HalfOpenPolicy::TreatAsOpen,
        );
        assert!(skip[0].breaker_open, "TreatAsOpen skips the trial entirely");
    }

    #[test]
    fn exact_preset_id_beats_model_match() {
        // `zhipu` 既是某个 preset 的 id，也可能是别的行的 model —— 精确 id 优先。
        let (cands, unmatched) = snapshot_candidates(
            &[("zhipu", BreakerState::Closed, 0)],
            PROVIDER_PRESETS,
            HalfOpenPolicy::default(),
        );
        assert!(unmatched.is_empty());
        assert_eq!(cands[0].provider_id, "zhipu");
        let preset = PROVIDER_PRESETS.iter().find(|p| p.id == "zhipu").unwrap();
        assert_eq!(cands[0].caps, preset.caps);
    }

    #[test]
    fn unknown_id_is_fail_closed_and_reported() {
        let (cands, unmatched) = snapshot_candidates(
            &[
                ("rule-v1", BreakerState::Closed, 0),
                ("no-such-provider", BreakerState::Closed, 0),
            ],
            PROVIDER_PRESETS,
            HalfOpenPolicy::default(),
        );
        assert_eq!(
            cands.len(),
            1,
            "unmatched id must NOT enter the candidate list"
        );
        assert_eq!(unmatched, vec!["no-such-provider".to_string()]);
        // 未臆造任何 caps：清单之外没有任何候选。
        assert!(cands.iter().all(|c| c.provider_id != "no-such-provider"));
    }

    #[test]
    fn empty_health_yields_empty_snapshot() {
        let (cands, unmatched) =
            snapshot_candidates(&[], PROVIDER_PRESETS, HalfOpenPolicy::default());
        assert!(cands.is_empty());
        assert!(unmatched.is_empty());
    }

    #[test]
    fn snapshot_is_deterministic_for_double_run() {
        let health = [
            ("zhipu", BreakerState::Open, 2),
            ("rule-v1", BreakerState::Closed, 0),
            ("ghost", BreakerState::HalfOpen, 1),
        ];
        let a = snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
        let b = snapshot_candidates(&health, PROVIDER_PRESETS, HalfOpenPolicy::default());
        assert_eq!(a, b, "same inputs must yield bit-identical snapshots");
    }

    #[test]
    fn order_follows_health_report_not_preset_table() {
        // 候选顺序 = 装配链序（health 顺序），不是 preset 表顺序。
        let (cands, _) = snapshot_candidates(
            &[
                ("rule-v1", BreakerState::Closed, 0),
                ("zhipu", BreakerState::Closed, 0),
            ],
            PROVIDER_PRESETS,
            HalfOpenPolicy::default(),
        );
        let ids: Vec<&str> = cands.iter().map(|c| c.provider_id.as_str()).collect();
        assert_eq!(ids, vec!["rule", "zhipu"]);
    }
}
