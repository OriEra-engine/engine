//! RFC 0003 v0/v1-b-2 — 槽位路由表 + 服务等级 + 决策台账 + 纯函数选链。
//!
//! 本模块是路由的**数据层与决策层**：槽位表怎么声明、等级怎么表达、
//! 一次决策留下什么记录，以及给定候选行快照后**选链上哪一项**。
//! 它仍不包 `FallbackProvider`、不改 `ModelProvider` 调用路径、不接帧
//! 预算——熔断状态机、超时截断、实例解析与装配注册是包装层 / v2 的事。
//!
//! 三条边界（评审时按这三条卡）：
//!
//! 1. **Tier 只约束最长等待 / 重试次数 / 降级顺序**，不约束选哪个模型。
//!    模型选择是槽位表（数据）的事，引擎不替用户做内容决策。
//! 2. **决策是观察记录**：台账不进 `state_hash`，不注册路由就不存在，
//!    与 `InferenceLedger` / 召回台账同一套纪律。
//! 3. **成本是估算**：`est_micro_cost` 由调用方按 `ModelPrice` 牌价表
//!    估出，字段名带 `est_`，不参与任何验收门。
//!
//! 选链（v1-b-2）是**纯函数**：路由器不持有 provider 实例，只消费装配期
//! 喂入的 [`ChainCandidate`] 只读快照（能力 / 本地性 / 熔断可见性），按
//! `chain` 顺序返回首个过闸项 [`ResolvedPick`]。熔断开合、超时取消、真正
//! 发起调用仍在 `FallbackProvider` 一侧；rule 档双跑时同表同快照必须
//! 逐位返回同一 `ResolvedPick`。

use crate::provider::ModelCaps;

/// 服务等级。语义只覆盖三件事：最长等待、重试次数、降级顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ServiceTier {
    /// 成本优先：可以等，受成本闸约束。
    Batch,
    /// 平衡档：允许一次重试，deadline 放宽。
    #[default]
    Balanced,
    /// 实时档：带 deadline，超时即降级，绝不阻塞帧。
    Realtime,
}

impl ServiceTier {
    /// Wire 形态（配置文件 / 报告里出现的字符串）。
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceTier::Realtime => "realtime",
            ServiceTier::Balanced => "balanced",
            ServiceTier::Batch => "batch",
        }
    }

    /// 反解 wire 形态；未知串返回 `None`（不猜档）。
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "realtime" => Some(ServiceTier::Realtime),
            "balanced" => Some(ServiceTier::Balanced),
            "batch" => Some(ServiceTier::Batch),
            _ => None,
        }
    }

    /// 该档默认允许的失败后重试次数（不含首调）。
    ///
    /// 纯策略默认值，可被装配层按槽位预算覆盖；这里只给档位口径：
    /// - `Realtime`：deadline 内不重试，超时即降级到链下一项；
    /// - `Balanced`：允许一次重试；
    /// - `Batch`：成本优先，失败不重跑（重跑加价），直接降级。
    pub fn default_retries(self) -> u32 {
        match self {
            ServiceTier::Realtime => 0,
            ServiceTier::Balanced => 1,
            ServiceTier::Batch => 0,
        }
    }
}

/// 隐私口径。合规属性，独立于性能档——云/本地不是快慢问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Privacy {
    /// 云或本地均可。
    #[default]
    Either,
    /// 只走本地开源服务（无出口、无 Key）。
    Local,
    /// 只走云端服务。
    Cloud,
}

impl Privacy {
    pub fn as_str(self) -> &'static str {
        match self {
            Privacy::Either => "either",
            Privacy::Local => "local",
            Privacy::Cloud => "cloud",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "either" => Some(Privacy::Either),
            "local" => Some(Privacy::Local),
            "cloud" => Some(Privacy::Cloud),
            _ => None,
        }
    }
}

/// 槽位预算（估算口径；`None` = 该维度不设闸）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotBudget {
    /// 该槽位在飞调用上限（并发信号量的静态值）。
    pub max_inflight: usize,
    /// P95 等待上限（毫秒，含网络）；Realtime 档的守门值。
    pub max_ms_p95: Option<u32>,
    /// 每千回合成本上限（µ-currency，按牌价表估算）。
    pub max_micro_per_1k_turns: Option<u64>,
}

/// 能力前置条件 token（`require` 列表里出现的字符串）。
///
/// 未知 token **fail closed**：不认识的能力要求按"不满足"处理，
/// 宁可漏选也不硬塞一个不支持工具调用的行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapNeed {
    Tools,
    StructuredOutput,
    ImageInput,
    PromptCache,
}

impl CapNeed {
    pub fn as_str(self) -> &'static str {
        match self {
            CapNeed::Tools => "tools",
            CapNeed::StructuredOutput => "structured_output",
            CapNeed::ImageInput => "image_input",
            CapNeed::PromptCache => "prompt_cache",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "tools" => Some(CapNeed::Tools),
            "structured_output" => Some(CapNeed::StructuredOutput),
            "image_input" => Some(CapNeed::ImageInput),
            "prompt_cache" => Some(CapNeed::PromptCache),
            _ => None,
        }
    }

    /// 该行能力是否满足这条前置。
    fn met_by(self, caps: &ModelCaps) -> bool {
        match self {
            CapNeed::Tools => caps.tools,
            CapNeed::StructuredOutput => caps.structured_output,
            CapNeed::ImageInput => caps.image_input,
            CapNeed::PromptCache => caps.prompt_cache,
        }
    }
}

/// 一条槽位路由。表是数据：宿主（Studio / 平台壳）在装配期构造，
/// 引擎只按 `id` 解析，不硬编码任何槽位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRoute {
    /// 稳定槽位 id（`npc_dialog` / `director` / `codegen` / `reviewer`…）。
    pub id: String,
    pub tier: ServiceTier,
    /// 降级链：preset id 顺序，`rule` 永远兜底。
    pub chain: Vec<String>,
    /// 能力前置；任一不满足即跳过该行。
    pub require: Vec<CapNeed>,
    pub privacy: Privacy,
    pub budget: SlotBudget,
}

impl SlotRoute {
    /// 该行能力是否满足本槽位的全部前置。
    pub fn meets_caps(&self, caps: &ModelCaps) -> bool {
        self.require.iter().all(|need| need.met_by(caps))
    }

    /// 候选行是否过本槽位的闸：熔断未开、隐私相符、能力满足。
    /// 纯判定，不查注册表、不发起调用。
    pub fn candidate_ok(&self, cand: &ChainCandidate) -> bool {
        !cand.breaker_open
            && privacy_allows(self.privacy, cand.local)
            && self.meets_caps(&cand.caps)
    }
}

/// 隐私闸：槽位口径与行的本地性是否相符。
fn privacy_allows(privacy: Privacy, local: bool) -> bool {
    match privacy {
        Privacy::Either => true,
        Privacy::Local => local,
        Privacy::Cloud => !local,
    }
}

/// 装配期喂给路由器的链上候选行**只读快照**。
///
/// 路由器不持有 provider 实例，也不跑熔断状态机；这张视图由包装层在
/// 装配 / 每次决策前从 preset 表与 `FallbackProvider::health_report()`
/// 读入。`breaker_open` 只是"该行此刻被跳过"的可见性标记，开合逻辑仍在
/// `health.rs` 一侧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainCandidate {
    /// preset id（与 [`SlotRoute::chain`] 里的项对齐）。
    pub provider_id: String,
    /// 该行声明能力。
    pub caps: ModelCaps,
    /// 本地开源行（无出口、无 Key）；隐私闸据此判 Local / Cloud。
    pub local: bool,
    /// 熔断已开（或半开被装配层判为不可用）：跳过，不碰线。
    pub breaker_open: bool,
}

/// 一次纯决策的选链结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPick {
    /// 命中 [`SlotRoute::chain`] 的第几项（0 起）。
    pub chain_index: usize,
    pub provider_id: String,
    /// 从哪一项降级下来；首项可服务时为 `None`。
    pub degraded_from: Option<String>,
}

/// 哪一道闸先耗尽（观察标记，不触发任何内部动作）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exhausted {
    /// 成本预算耗尽。
    Cost,
    /// 等待 deadline 耗尽。
    Deadline,
}

/// 一次被记录的路由决策。选链决策由 [`SlotRouter::resolve`] 给出 id；
/// 本结构是事后落账的审计行，本身不参与选链。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    /// 台账内单调序号（从 1 起，台账发号，调用方不可伪造）。
    pub seq: u64,
    pub slot: String,
    pub tier: ServiceTier,
    /// 实际服务的 provider 标签（`ModelProvider::id()` 形态）。
    pub provider_id: String,
    /// 命中降级链的第几项（0 起）。
    pub chain_index: usize,
    /// 本次等待耗时（微秒，进程侧；不参与确定性比对）。
    pub waited_us: u64,
    /// 从哪一条降级下来的（首项服务时为 `None`）。
    pub degraded_from: Option<String>,
    /// 哪道闸耗尽；未耗尽为 `None`。
    pub exhausted: Option<Exhausted>,
    /// 按牌价表估算的本次成本（µ-currency；estimate，不参与验收门）。
    pub est_micro_cost: u64,
    /// 提示词指纹（复用 `prompt_fingerprint` 那把哈希族）。
    pub prompt_fingerprint: u64,
}

/// 决策台账容量（环形：满了丢最旧，序号不重置）。
pub const ROUTE_LEDGER_CAP: usize = 256;

/// 有界决策台账。与 `InferenceLedger` 同纪律：环形裁剪、逐条可比对、
/// 纯观察数据。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteLedger {
    entries: Vec<RouteDecision>,
}

impl RouteLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一条决策；`seq` 在此发号。
    pub fn record(&mut self, mut decision: RouteDecision) {
        // 序号跨窗口单调：用最后一条的 seq 续号，而不是按当前长度。
        let seq = self
            .entries
            .last()
            .map(|e| e.seq)
            .unwrap_or(0)
            .saturating_add(1);
        decision.seq = seq;
        self.entries.push(decision);
        let len = self.entries.len();
        if len > ROUTE_LEDGER_CAP {
            self.entries.drain(0..len - ROUTE_LEDGER_CAP);
        }
    }

    pub fn entries(&self) -> &[RouteDecision] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 两本台账讲同一段决策故事（逐条相等；`Strict` 双胞胎必须过，
    /// `Recorded` 双胞胎拿它做漂移取证）。
    pub fn matches(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

/// 路由器：持有槽位表 + 决策台账 + 纯函数选链。**不发起任何调用、不持
/// provider 实例**——它回答"这个槽位怎么配 / 链上哪一项过闸"，并把已经
/// 发生的决策记下来。熔断开合状态机、实例解析、deadline 截断是包装层
/// （包 `FallbackProvider`）接线时的事。
#[derive(Debug, Clone, Default)]
pub struct SlotRouter {
    routes: Vec<SlotRoute>,
    ledger: RouteLedger,
}

impl SlotRouter {
    pub fn new(routes: Vec<SlotRoute>) -> Self {
        Self {
            routes,
            ledger: RouteLedger::new(),
        }
    }

    /// 按 id 解析槽位；未注册槽位返回 `None`（不猜默认档）。
    pub fn route(&self, slot_id: &str) -> Option<&SlotRoute> {
        self.routes.iter().find(|r| r.id == slot_id)
    }

    /// 纯函数选链：按 `chain` 顺序返回首个过闸项。
    ///
    /// - 未注册槽位 → `None`（不猜默认档）。
    /// - 链上某 id 在 `candidates` 里查不到 → 该行 fail closed 跳过，
    ///   不臆造能力；全部不可用 → `None`（调用方兜底，rule 行应在链尾
    ///   且永不 `breaker_open`）。
    /// - 以 `chain` 顺序为准，`candidates` 顺序无关，按 id 匹配。
    ///
    /// 本方法**不记台账、不发起调用**；选中后由包装层调 [`observe`] 落账。
    /// rule 档双跑：同表同快照必须逐位返回同一 [`ResolvedPick`]。
    pub fn resolve(&self, slot_id: &str, candidates: &[ChainCandidate]) -> Option<ResolvedPick> {
        let route = self.route(slot_id)?;
        let first_id = route.chain.first().cloned();
        for (i, pid) in route.chain.iter().enumerate() {
            let cand = match candidates.iter().find(|c| &c.provider_id == pid) {
                Some(c) => c,
                None => continue,
            };
            if !route.candidate_ok(cand) {
                continue;
            }
            return Some(ResolvedPick {
                chain_index: i,
                provider_id: pid.clone(),
                degraded_from: if i > 0 { first_id.clone() } else { None },
            });
        }
        None
    }

    pub fn routes(&self) -> &[SlotRoute] {
        &self.routes
    }

    pub fn ledger(&self) -> &RouteLedger {
        &self.ledger
    }

    /// 记下一次决策。槽位未注册时**不编造 tier**：不入台账，返回 `None`。
    ///
    /// 除 `slot_id` 外的参数由调用方填——`resolve` 只给出候选 id，实际
    /// 谁服务了本次由包装层如实回填 `provider_id` / `chain_index` /
    /// `degraded_from`。
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        slot_id: &str,
        provider_id: &str,
        chain_index: usize,
        waited_us: u64,
        degraded_from: Option<String>,
        exhausted: Option<Exhausted>,
        est_micro_cost: u64,
        prompt_fingerprint: u64,
    ) -> Option<&RouteDecision> {
        let route = self.route(slot_id)?;
        // 链上位置越界 = 记账方给了 malformed 决策：fail closed，不落账。
        if chain_index >= route.chain.len() {
            return None;
        }
        let decision = RouteDecision {
            seq: 0,
            slot: slot_id.to_string(),
            tier: route.tier,
            provider_id: provider_id.to_string(),
            chain_index,
            waited_us,
            degraded_from,
            exhausted,
            est_micro_cost,
            prompt_fingerprint,
        };
        self.ledger.record(decision);
        self.ledger.entries().last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_route() -> SlotRoute {
        SlotRoute {
            id: "npc_dialog".into(),
            tier: ServiceTier::Realtime,
            chain: vec!["zhipu-flash".into(), "rule:rule-v1".into()],
            require: vec![CapNeed::Tools],
            privacy: Privacy::Either,
            budget: SlotBudget {
                max_inflight: 2,
                max_ms_p95: Some(600),
                max_micro_per_1k_turns: Some(200_000),
            },
        }
    }

    fn cand(id: &str, local: bool, breaker_open: bool, tools: bool) -> ChainCandidate {
        ChainCandidate {
            provider_id: id.into(),
            caps: ModelCaps {
                context_window: Some(8192),
                structured_output: false,
                tools,
                image_input: false,
                prompt_cache: false,
            },
            local,
            breaker_open,
        }
    }

    #[test]
    fn route_lookup_resolves_registered_slot_and_keeps_chain_order() {
        let router = SlotRouter::new(vec![sample_route()]);
        let route = router.route("npc_dialog").expect("registered slot");
        assert_eq!(route.tier, ServiceTier::Realtime);
        assert_eq!(route.chain, vec!["zhipu-flash", "rule:rule-v1"]);
        assert!(
            router.route("director").is_none(),
            "unknown slot → None, no default tier"
        );
    }

    #[test]
    fn tier_and_privacy_wire_roundtrip() {
        for t in [
            ServiceTier::Realtime,
            ServiceTier::Balanced,
            ServiceTier::Batch,
        ] {
            assert_eq!(ServiceTier::from_wire(t.as_str()), Some(t));
        }
        for p in [Privacy::Either, Privacy::Local, Privacy::Cloud] {
            assert_eq!(Privacy::from_wire(p.as_str()), Some(p));
        }
        assert!(ServiceTier::from_wire("turbo").is_none());
        assert!(Privacy::from_wire("vpngate").is_none());
    }

    #[test]
    fn caps_requirement_fails_open_or_closed_consistently() {
        let route = sample_route();
        let with_tools = ModelCaps {
            context_window: Some(8192),
            structured_output: false,
            tools: true,
            image_input: false,
            prompt_cache: false,
        };
        let without_tools = ModelCaps {
            context_window: Some(8192),
            structured_output: false,
            tools: false,
            image_input: false,
            prompt_cache: false,
        };
        assert!(route.meets_caps(&with_tools));
        assert!(!route.meets_caps(&without_tools));
        // 字符串层的未知 token 解析为 None（fail closed），构造不出枚举。
        assert!(CapNeed::from_wire("time_travel").is_none());
    }

    #[test]
    fn ledger_is_bounded_and_seq_keeps_counting_past_the_window() {
        let mut led = RouteLedger::new();
        for i in 0..(ROUTE_LEDGER_CAP + 10) {
            led.record(RouteDecision {
                seq: 0,
                slot: "npc_dialog".into(),
                tier: ServiceTier::Realtime,
                provider_id: "rule:rule-v1".into(),
                chain_index: 0,
                waited_us: 100,
                degraded_from: None,
                exhausted: None,
                est_micro_cost: 0,
                prompt_fingerprint: i as u64,
            });
        }
        assert_eq!(led.len(), ROUTE_LEDGER_CAP);
        assert_eq!(led.entries()[0].prompt_fingerprint, 10, "oldest dropped");
        assert_eq!(
            led.entries().last().unwrap().seq,
            ROUTE_LEDGER_CAP as u64 + 10
        );
        assert_eq!(led.entries()[0].seq, 11, "seq not reset by the ring");
        assert!(led.matches(&led.clone()));
    }

    #[test]
    fn observe_fills_tier_from_route_and_unknown_slot_records_nothing() {
        let mut router = SlotRouter::new(vec![sample_route()]);
        let d = router
            .observe(
                "npc_dialog",
                "rule:rule-v1",
                1,
                42_000,
                Some("zhipu-flash".into()),
                Some(Exhausted::Cost),
                12_000,
                7,
            )
            .expect("registered slot recorded");
        assert_eq!(
            d.tier,
            ServiceTier::Realtime,
            "tier comes from the route table"
        );
        assert_eq!(d.chain_index, 1);
        assert_eq!(d.degraded_from, Some("zhipu-flash".into()));
        assert_eq!(d.exhausted, Some(Exhausted::Cost));
        assert_eq!(d.est_micro_cost, 12_000);
        assert_eq!(d.prompt_fingerprint, 7);
        assert_eq!(d.seq, 1);

        assert!(
            router
                .observe("director", "x", 0, 0, None, None, 0, 0)
                .is_none(),
            "unknown slot: no fabricated tier, no ledger entry"
        );
        assert_eq!(router.ledger().len(), 1);

        // 链上位置越界同样 fail closed：malformed 决策不进台账。
        assert!(
            router
                .observe("npc_dialog", "rule:rule-v1", 99, 0, None, None, 0, 0)
                .is_none(),
            "chain_index out of range rejected"
        );
        assert_eq!(router.ledger().len(), 1);
    }

    #[test]
    fn same_route_table_and_decision_stream_replay_entrywise() {
        let run = || {
            let mut router = SlotRouter::new(vec![sample_route()]);
            router.observe(
                "npc_dialog",
                "rule:rule-v1",
                0,
                5_000,
                None,
                None,
                3_000,
                11,
            );
            router.observe(
                "npc_dialog",
                "rule:rule-v1",
                1,
                9_000,
                Some("zhipu-flash".into()),
                None,
                2_000,
                12,
            );
            router.ledger().clone()
        };
        assert_eq!(run(), run(), "rule 档双跑决策逐条一致");
    }

    #[test]
    fn resolve_picks_first_when_it_passes_the_gates() {
        let router = SlotRouter::new(vec![sample_route()]);
        let picks = router
            .resolve(
                "npc_dialog",
                &[
                    cand("zhipu-flash", false, false, true),
                    cand("rule:rule-v1", true, false, true),
                ],
            )
            .expect("first row serves");
        assert_eq!(picks.chain_index, 0);
        assert_eq!(picks.provider_id, "zhipu-flash");
        assert_eq!(picks.degraded_from, None);
    }

    #[test]
    fn resolve_skips_open_breaker_and_marks_degraded() {
        let router = SlotRouter::new(vec![sample_route()]);
        let picks = router
            .resolve(
                "npc_dialog",
                &[
                    cand("zhipu-flash", false, true, true),
                    cand("rule:rule-v1", true, false, true),
                ],
            )
            .expect("fallback to rule row");
        assert_eq!(picks.chain_index, 1);
        assert_eq!(picks.provider_id, "rule:rule-v1");
        assert_eq!(picks.degraded_from, Some("zhipu-flash".into()));
    }

    #[test]
    fn resolve_skips_row_missing_required_caps() {
        let router = SlotRouter::new(vec![sample_route()]);
        let picks = router
            .resolve(
                "npc_dialog",
                &[
                    cand("zhipu-flash", false, false, false), // 无 tools
                    cand("rule:rule-v1", true, false, true),
                ],
            )
            .expect("caps fail → next row");
        assert_eq!(picks.chain_index, 1);
        assert_eq!(picks.provider_id, "rule:rule-v1");
    }

    #[test]
    fn resolve_privacy_gate_rejects_cloud_for_local_slot() {
        let mut route = sample_route();
        route.id = "private".into();
        route.privacy = Privacy::Local;
        let router = SlotRouter::new(vec![route]);
        let picks = router
            .resolve(
                "private",
                &[
                    cand("zhipu-flash", false, false, true), // 云端行
                    cand("rule:rule-v1", true, false, true), // 本地兜底
                ],
            )
            .expect("local slot skips cloud row");
        assert_eq!(picks.provider_id, "rule:rule-v1");
        assert_eq!(picks.degraded_from, Some("zhipu-flash".into()));
    }

    #[test]
    fn resolve_unknown_slot_and_exhausted_chain_return_none() {
        let router = SlotRouter::new(vec![sample_route()]);
        assert!(
            router
                .resolve("director", &[cand("rule:rule-v1", true, false, true)])
                .is_none(),
            "unknown slot → None"
        );
        // 链上全部熔断：无过闸项 → None，由调用方兜底。
        assert!(router
            .resolve(
                "npc_dialog",
                &[
                    cand("zhipu-flash", false, true, true),
                    cand("rule:rule-v1", true, true, true),
                ],
            )
            .is_none());
    }

    #[test]
    fn resolve_missing_registry_row_fails_closed_and_continues() {
        let router = SlotRouter::new(vec![sample_route()]);
        // 首项在候选快照里查不到：不臆造能力，跳过走下一项。
        let picks = router
            .resolve("npc_dialog", &[cand("rule:rule-v1", true, false, true)])
            .expect("missing first row skipped");
        assert_eq!(picks.chain_index, 1);
        assert_eq!(picks.provider_id, "rule:rule-v1");
    }

    #[test]
    fn resolve_double_run_returns_identical_pick() {
        let snap = || {
            let router = SlotRouter::new(vec![sample_route()]);
            router.resolve(
                "npc_dialog",
                &[
                    cand("zhipu-flash", false, true, true),
                    cand("rule:rule-v1", true, false, true),
                ],
            )
        };
        assert_eq!(snap(), snap(), "rule 档双跑选链逐位一致");
    }

    #[test]
    fn tier_default_retries_follow_tier_policy() {
        assert_eq!(ServiceTier::Realtime.default_retries(), 0);
        assert_eq!(ServiceTier::Balanced.default_retries(), 1);
        assert_eq!(ServiceTier::Batch.default_retries(), 0);
    }

    #[test]
    fn resolve_empty_chain_returns_none() {
        let mut route = sample_route();
        route.chain = Vec::new();
        let router = SlotRouter::new(vec![route]);
        assert!(router
            .resolve("npc_dialog", &[cand("rule:rule-v1", true, false, true)])
            .is_none());
    }

    #[test]
    fn resolve_is_independent_of_candidate_order() {
        let router = SlotRouter::new(vec![sample_route()]);
        let in_order = router.resolve(
            "npc_dialog",
            &[
                cand("zhipu-flash", false, true, true),
                cand("rule:rule-v1", true, false, true),
            ],
        );
        let reversed = router.resolve(
            "npc_dialog",
            &[
                cand("rule:rule-v1", true, false, true),
                cand("zhipu-flash", false, true, true),
            ],
        );
        assert_eq!(in_order, reversed, "候选快照顺序无关，按 chain 顺序选");
    }

    #[test]
    fn resolve_degraded_from_points_at_first_row_across_multiple_hops() {
        let mut route = sample_route();
        route.chain = vec!["a".into(), "b".into(), "c".into()];
        let router = SlotRouter::new(vec![route]);
        let picks = router
            .resolve(
                "npc_dialog",
                &[
                    cand("a", false, true, true),   // 熔断开
                    cand("b", false, false, false), // 能力不符
                    cand("c", true, false, true),   // 服务
                ],
            )
            .expect("third row serves after two skips");
        assert_eq!(picks.chain_index, 2);
        assert_eq!(picks.provider_id, "c");
        assert_eq!(picks.degraded_from, Some("a".into()), "降级口径记首项");
    }
}
