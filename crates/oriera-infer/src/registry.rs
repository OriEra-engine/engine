//! RFC 0003 v1-b-2 步 A — 装配期 provider 注册表（id → 实例 + 声明视图）。
//!
//! 本模块只做**装配期只读索引**：把装配层已经构造好的 provider 实例按
//! preset id 登记，附该行 preset 表声明的 [`local`](ProviderPreset::local) /
//! [`caps`](ProviderPreset::caps) 视图。它的边界（评审按这三条卡）：
//!
//! 1. **不选链、不跑熔断状态机、不发起调用**；选链判定归 `router.rs` 纯函数，
//!    熔断开合归 `health.rs`，真正发起调用归后续包装层（步 C）。
//! 2. **不感知健康状态**：注册表不读 `BreakerState`，候选快照的 `breaker_open`
//!    视图由步 B 从 `health_report()` 映射产出。
//! 3. **缺行 fail closed**：未登记的 id 一律返回 `None`，不臆造能力、不回退到
//!    某个默认实例。
//!
//! 层次对齐评审：实例解析判定归包装层；注册表只是 id → 实例的索引，取值动作
//! 由包装层发起。`Arc` 共享实例是装配语义（多个入口持同一后端），注册表自身的
//! 映射表在 clone 后独立。

use crate::provider::{ModelCaps, ModelProvider};
use std::collections::HashMap;
use std::sync::Arc;

/// 一条已登记实例的装配期记录：实例句柄 + preset 表声明的只读视图。
#[derive(Clone)]
struct ProviderEntry {
    provider: Arc<dyn ModelProvider>,
    local: bool,
    caps: ModelCaps,
}

/// 装配期 `preset id` → 实例 + 声明视图索引。
///
/// 构造发生在启动装配期；运行期只读。重复登记同一 id 视为装配错误，不静默
/// 覆盖（见 [`ProviderRegistry::register`]）。
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    entries: HashMap<String, ProviderEntry>,
}

impl ProviderRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个已构造实例及其 preset 声明视图。
    ///
    /// 返回 `true` 表示登记成功；`false` 表示该 id 已存在——**不覆盖**既有条目
    /// （fail closed：装配层应先排查重复配置，而不是让后登记的实例悄悄顶替）。
    pub fn register(
        &mut self,
        id: impl Into<String>,
        provider: Arc<dyn ModelProvider>,
        local: bool,
        caps: ModelCaps,
    ) -> bool {
        let id = id.into();
        if self.entries.contains_key(&id) {
            return false;
        }
        self.entries.insert(
            id,
            ProviderEntry {
                provider,
                local,
                caps,
            },
        );
        true
    }

    /// 按 id 取实例句柄；未登记返回 `None`。
    pub fn get(&self, id: &str) -> Option<&Arc<dyn ModelProvider>> {
        self.entries.get(id).map(|e| &e.provider)
    }

    /// 该行是否本地开源（无出口、无 Key）；未登记返回 `None`。
    pub fn local_of(&self, id: &str) -> Option<bool> {
        self.entries.get(id).map(|e| e.local)
    }

    /// 该行声明能力；未登记返回 `None`。
    pub fn caps_of(&self, id: &str) -> Option<ModelCaps> {
        self.entries.get(id).map(|e| e.caps)
    }

    /// 已登记条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否一条都没登记。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 已登记 id 快照（顺序不定，仅供装配层遍历/排障）。
    pub fn ids(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ModelCaps, RuleProvider};

    fn rule_arc() -> Arc<dyn ModelProvider> {
        Arc::new(RuleProvider::new())
    }

    #[test]
    fn register_then_lookup_provider_and_views() {
        let mut reg = ProviderRegistry::new();
        let caps = ModelCaps {
            context_window: Some(32_000),
            structured_output: true,
            tools: true,
            image_input: false,
            prompt_cache: true,
        };
        assert!(reg.register("rule", rule_arc(), true, caps));

        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());
        assert!(reg.get("rule").is_some());
        assert_eq!(reg.local_of("rule"), Some(true));
        assert_eq!(reg.caps_of("rule"), Some(caps));
    }

    #[test]
    fn unknown_id_fails_closed_to_none() {
        let mut reg = ProviderRegistry::new();
        reg.register("rule", rule_arc(), true, ModelCaps::default());

        assert!(reg.get("nope").is_none());
        assert!(reg.local_of("nope").is_none());
        assert!(reg.caps_of("nope").is_none());
        assert!(reg.ids().iter().all(|id| *id != "nope"));
    }

    #[test]
    fn duplicate_id_does_not_overwrite() {
        let mut reg = ProviderRegistry::new();
        let first = rule_arc();
        assert!(reg.register("rule", first.clone(), true, ModelCaps::default()));
        // 第二次同 id：拒绝覆盖，返回 false。
        assert!(!reg.register("rule", rule_arc(), false, ModelCaps::default()));

        // 原条目（local=true）保持不变。
        assert_eq!(reg.local_of("rule"), Some(true));
        assert!(Arc::ptr_eq(reg.get("rule").unwrap(), &first));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn clone_keeps_independent_map_but_shared_instance() {
        let mut reg = ProviderRegistry::new();
        let shared = rule_arc();
        assert!(reg.register("rule", shared.clone(), true, ModelCaps::default()));

        let mut clone = reg.clone();
        // 克隆表独立：原表新增条目不进克隆表。
        assert!(clone.register("cloud", rule_arc(), false, ModelCaps::default()));
        assert_eq!(reg.len(), 1);
        assert_eq!(clone.len(), 2);
        assert!(reg.get("cloud").is_none());

        // 实例句柄在两张表间共享（Arc 语义）。
        assert!(Arc::ptr_eq(
            reg.get("rule").unwrap(),
            clone.get("rule").unwrap()
        ));
    }

    #[test]
    fn empty_registry_reports_empty() {
        let reg = ProviderRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.ids().is_empty());
    }
}
