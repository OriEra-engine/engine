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
//! 链上条目（[`SlotRoute::chain`](crate::router::SlotRoute)）用 preset id
//! （`rule` / `zhipu` / `openai`），而 [`ModelProvider::id`](crate::provider::ModelProvider::id)
//! 报的是**实例 id**（`rule` 行的实例叫 `rule-v1`）。两者不属于同一命名空间。
//! 本模块按"先精确 preset id、再按 preset 的默认 model 名"两级查找闭合它：
//! 后者命中 `rule-v1 → rule` 这类行，且不引入任何硬编码别名表。两级都不中
//! 即 `unmatched`，绝不猜。

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
///   是**健康快照里的原始 id**——`resolve` 要按它与 `SlotRoute::chain`
///   匹配，所以不能替换成 preset id；
/// - `unmatched` 是查不到声明的 id 清单，调用方据此报装配错误或降级。
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
                provider_id: id.to_string(),
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
            cands[0].provider_id, "rule-v1",
            "id stays as health reported it"
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
        assert_eq!(ids, vec!["rule-v1", "zhipu"]);
    }
}
