# 第 9 刀：L9 记忆分层第 1 步 — 两层后端骨架 + 巩固策略纯层（默认单层金样不动）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

L9 记忆分层的第 1 步。本轮新增一个 opt-in 的两层后端（短期缓冲 + 长期沉淀）与巩固策略纯层；**默认 `LocalMemoryBackend` 单层 Vec 一字未动**，现网写入顺序、打分公式、JSON 往返金样逐位不变。分层后端只能经既有 `MemoryStore::with_backend` 显式注入，认知/目标调用点签名不改。

补丁：`patches/008-l9-layered-memory-backend.patch`（2 文件，+353 / -2，基于 `0387718`，叠在 001→007 之上）。

---

## 一、落地内容（`crates/oriera-agent/src/memory.rs`）

### 1. `MemoryLayer` 层位枚举

`ShortTerm`（短期缓冲）/ `LongTerm`（长期沉淀）。仅描述分层后端内部布局；`Episode` 本身**不带层位标签**——条目层位字段属第 2 步 additive 契约扩展。

### 2. `ConsolidationPolicy` 巩固策略（纯配置）

```rust
pub struct ConsolidationPolicy {
    pub promote_importance: u8,   // ≥ 该重要性方可升入长期层
    pub max_promote_age: u64,     // 年龄门控；0 = 不按年龄门控
}
```

- 默认：`promote_importance = 4`，`max_promote_age = 0`。
- `sanitized()` 把重要性钳制到合法 1..=5；非有限/越界不污染整库排序（同 `MemoryScoring` 哲学）。
- `promotes(episode, now)`：重要性达标即合格；年龄门控为预留旋钮，第 1 步不启用。复用命中次数信号留第 2 步条目字段，不在此虚构。

### 3. `LayeredMemoryBackend` 两层后端

| 项 | 口径 |
|---|---|
| 结构 | `short` / `long` 两张 `HashMap<agent, Vec<Episode>>` + 首次出现顺序 `names` + 策略 + 两层容量 |
| 写入 | `record` 进短期缓冲；登记 agent 名 |
| 巩固 | `consolidate()`（trait hook）遍历短期层：策略合格条目移入长期层，其余留短期；提升后长期层按自身窗口裁剪 |
| 召回 | 合并两层条目走既有 `score_of` 打分排序取 `k`；条目只居一层，提升后离开短期层，召回不重复 |
| 裁剪 | 两层各自 FIFO 窗口；后端自带 `short_capacity` / `long_capacity`（默认 60 / 120），门面单层 `capacity` 对本后端不生效 |
| 导出 | `episodes()` 返回 `None`：两层是分立 Vec，借不出一个连续切片；JSON 导出跳过本后端，不编造合并顺序 |
| 克隆 | `clone_backend` 深拷贝两层，回滚路径彼此独立 |

### 4. 门面与导出

- 门面 API 未新增构造器；分层后端经既有 `MemoryStore::with_backend(capacity, scoring, backend)` 注入。
- `lib.rs` 再导出 `ConsolidationPolicy / LayeredMemoryBackend / MemoryLayer`，与既有后端类型同级。

## 二、拍板口径落地对照

| 拍板项 | 结论 | 落地 |
|---|---|---|
| 层位划分 | 短期缓冲 + 长期沉淀两层 | ✅ `MemoryLayer::{ShortTerm, LongTerm}` |
| 巩固触发 | 第 1 步 importance + 年龄纯策略；命中次数留第 2 步 | ✅ 重要性阈值判定，年龄门控预留 `max_promote_age` 默认关闭 |
| 承载形态 | 新后端实现 trait，默认后端零改动 | ✅ `LayeredMemoryBackend` 独立实现 `MemoryBackend`；`LocalMemoryBackend` 未改 |

## 三、明确没做（留后续）

- **门面未暴露 `consolidate` 调度**：巩固仍是后端 hook，第 3 步再接系统 tick / 会话边界触发。
- **重要性感知淘汰**：两层第 1 步均 FIFO；长期层按 importance×recency 得分淘汰留第 2 步。
- **条目溯源 / 置信度 / 复用计数字段**：`Episode` 结构未动；`source_ref` 等留第 2 步 `Option` 扩展。
- **跨层嵌入召回融合**：分层后端 `retrieve_embedded` 走 trait 默认实现退回离线档；跨层语义融合留第 3 步。
- **JSON 分层快照**：分层后端暂不导出；快照版本口径随第 2 步定。
- **跨 agent 共享语义层、远程/图谱记忆后端**：仅 trait 接口位预留，本轮未实现，不表述为已集成。
- **JS 镜像**：默认路径仍是单层；分层类不同步到 `kernel.js`，待分层后端进默认装配再镜像。

## 四、验收

干净 worktree 从 `0387718` 顺序 apply 001 → … → 007 → 008，独立 `CARGO_TARGET_DIR` 复现：

| 项 | 结果 |
|---|---|
| `oriera-agent`（`json,rigid3d,cam,spec,urdf`） | 103 passed（+7：策略钳制 / 高重要性提升 / 年龄门控留短期 / 两层合并召回不重复 / 两层各自窗口 / 不导出合并切片 / 克隆独立）；doctest 1、3 |
| `cargo test --workspace` | 599 passed / 0 failed / 4 ignored |
| clippy | `-D warnings` 零告警 |
| rustfmt | `--check` 干净 |
| JS 镜像 | 130 pass / 0 fail（本轮未改 JS） |

## 五、下一棒（评审后再动）

1. 第 2 步：`Episode` 溯源 / 置信度 `Option` 字段 + `from_json` 缺省反序列化；快照版本口径；`memory@v1` 指标骨架。
2. 第 3 步：门面暴露巩固调度接入系统 tick；长期层重要性感知淘汰；跨层嵌入召回融合。
3. 砍项条件沿用 03 §3.4：分层若在默认金样造成打分或序列化漂移，回退单层 Vec，分层只走新后端。
