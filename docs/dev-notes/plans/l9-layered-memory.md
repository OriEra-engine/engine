# L9 记忆分层 — 现状调研与落地方案（待评审）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

- 日期：2026-11-23
- 范围：`crates/oriera-agent/src/memory.rs`（L9 长期记忆）+ JS 镜像 `examples/11-ai-cafe/engine/kernel.js`
- 性质：能力补全（单层 episodic 缓存 → 可 opt-in 的分层记忆），非破坏；契约扩展部分待拍板
- 关联：ADR 0004（L9 认知面 / embodied 拆分）、既有 `MemoryBackend` 可插拔抽象（补丁 002）

## 一、现状盘点（已验证事实）

### 1. 存储结构

`MemoryStore` 门面持有 `Box<dyn MemoryBackend>`；默认后端 `LocalMemoryBackend` 为 `HashMap<String, Vec<Episode>>`，按 agent 隔离：

- **容量**：per-agent 滑动窗口，默认 `capacity = 60`，`with_capacity` / `with_capacity_and_scoring` / `with_backend` 可注入。
- **淘汰**：`record` 后调 `prune(agent, capacity)`；超窗时 `drain(0..len - capacity)` —— **按插入顺序丢最旧的**，不看重要性、不看近因得分。
- **条目** `Episode { turn: u64, day: u32, about: String, text: String, importance: u8 }`，无层位、无来源回链、无复用/命中计数、无置信度。
- **打分**：`MemoryScoring` 系数已抽出可配置（关键词×1.6 + 重要性×0.45 + 近因×2.0 + 主体加分 1.2 + 语义×2.0，`>0.8` 门）；`score_of` 产出带分项的 `RecallTrace`，召回事件进 `RecallLedger`（256 容量台账）。
- **双档召回**：`retrieve`（离线关键词-近因-重要性）/ `retrieve_embedded`（批量嵌入 + 余弦项，embedder 失败可见降级回离线档）。
- **持久化**：`json` feature 下 `to_json` / `from_json`；快照只带条目，**不恢复** scoring 与 capacity；`from_json` 逐条 re-record，导入同样受 60 窗约束；字段缺失/类型错/importance 溢出一律 `Err`，不 panic。
- **后端可插拔**：trait 已留 `consolidate()` no-op hook、`episodes()` 可返回 `None`（远程后端不编造内容）、`clone_backend()` 保门面 `Clone`；测试有 `CannedBackend` 验证换后端不动调用点。

### 2. 调用面（门面 API，已稳定）

| 调用方 | 用法 |
|---|---|
| `cognition.rs` | `retrieve_traced` 取记忆行入 prompt；三处 `record`（双方记忆 + world 事件） |
| `goal.rs` | 目标达成/受挫写 world 记忆（importance 4） |
| `kernel.js`（JS 镜像） | 同构 `MemoryStore` 类，`capacity=60`，`record` 超窗 `splice` 丢旧 |

门面公开方法：`record / retrieve / retrieve_traced / retrieve_embedded / retrieve_embedded_traced / len_total / agents / is_empty / set_scoring / scoring / with_* / to_json / from_json`。

## 二、天花板与缺口（对照 03 文档 §3.1）

1. **单层 episodic 缓存**：无短期/长期分层，无巩固（consolidation）与主动遗忘；容量满纯 FIFO 丢旧，高重要性的旧经验可能被低重要性新条目挤出。
2. **不可溯源**：`Episode` 不挂来源回链（文档/章节/场景片段/帧号），检索结果无法回答"这条从哪来"。
3. **不跨 agent 复用**：HashMap 按 agent 隔离，无共享语义层。
4. **巩固 hook 空置**：trait `consolidate()` 为 no-op，全仓库无调用方；分层后的"升入长期层"动作尚无落点。

> 注：溯源（缺口 2）、跨 agent 共享语义层（缺口 3）与本轮分层解耦，列入第 2 步及留后，不在第 1 步混做。

## 三、金样冻结纪律（不可破的红线）

来自 03 文档 §3.2 / §3.4 与仓库 ADR 纪律：

- **默认路径行为逐位不变**：现网写入顺序、打分公式、JSON 往返检索结果作为 v1 金样冻结；默认 `LocalMemoryBackend` 的淘汰顺序与容量语义不得漂移。
- **新能力走 opt-in 口子**：分层作为新后端（或新配置档）经既有 `MemoryBackend` trait / `with_backend` 接入，认知/目标调用点签名一字不改；不动 `course@v1` / `paper@v1` 既有协议。
- **漂移即回退**：分层记忆若在默认金样上造成打分或序列化漂移，回退单层 Vec，分层只走新后端；不达标则保留 trait 与本地实现，远程/图谱实现延后。
- **诚实深度**：不支持向量的后端可见降级，不允许健康座位返回空结果；远程/图谱后端当前仅接口预留与适配，**不表述为已集成**，schema 以伙伴官方披露为准。
- **JS 同构**：Rust 默认路径与 `kernel.js` 镜像语义必须一致；Rust 改默认行为时 JS 同步改，反之亦然。

## 四、分步建议（骨架小步）

### 第 1 步 · 分层数据模型 + 淘汰/巩固策略纯层（建议本轮评审后落）

| 项 | 结论 |
|---|---|
| 现状 | 单层 `Vec<Episode>`，FIFO 丢旧；`consolidate()` no-op 无调用方 |
| 缺口 | 无短期/长期两层结构；无"高价值条目留存、低价值条目先淘汰"的策略口径 |
| 落点 | ① 定义层位模型（短期缓冲层 / 长期沉淀层，术语待拍板）；② 分层容器与巩固/淘汰规则写成**纯函数策略**，可离线夹具回归；③ 默认 `LocalMemoryBackend` **行为不变**，分层由新后端 `LayeredMemoryBackend`（或 opt-in 配置）承载，经 trait 接入；④ 巩固触发先只用现有字段（importance + age）定规则，命中计数等新字段留第 2 步 |
| 不做 | 不接系统 tick 自动巩固调度；不改默认打分公式；不动 JSON v1 快照；不接远程/图谱后端；不做跨 agent 共享 |
| 风险 | 两层结构若设计不当会反向逼着改默认后端；纪律上分层只能落在新后端，默认后端保持单层金样 |
| 骨架可否落 | ✅ 可落（层位术语与巩固口径待拍板） |

### 第 2 步 · 条目溯源字段 + 快照兼容 + 评测指标

| 项 | 结论 |
|---|---|
| 现状 | `Episode` 无 `source_ref` / 置信度；JSON v1 严格缺字段即 Err；无 `memory@v1` 评测协议 |
| 缺口 | 检索结果无回链；长时保持率/抗遗忘保留率无账本 |
| 落点 | `Episode` 加 `Option` 溯源/置信度槽位（缺省 `None`，`from_json` 放宽为缺省反序列化）；评估是否升 JSON v2 或 v1 加可选字段；`memory@v1` 指标骨架（命中率、回链命中率、长时保持、抗遗忘），与具身评测同一套 spec_hash + 指纹账本 |
| 待拍板 | 溯源字段集合；快照版本口径；溯源闸是拒绝还是宽松打标 |
| 骨架可否落 | ⚠️ 契约扩展，待第 1 步落地 + 口径拍板 |

### 第 3 步 · 巩固调度接系统 + 跨层召回融合（留后）

- 巩固由纯策略落到系统 tick / 会话边界触发；长期层与短期层召回结果融合排序口径。
- 跨 agent 共享语义层、远程/图谱记忆后端联调，待伙伴接口与真实样本。

## 五、待拍板项

1. **层位术语与划分**：短期缓冲（working / episodic buffer）vs 长期沉淀（consolidated / semantic）两层；还是沿用认知科学的 episodic vs semantic 知识层。建议先做"短期缓冲 + 长期沉淀"两层，与现有 episodic 条目直接对应。
2. **巩固触发口径**：第 1 步纯策略按什么升入长期层 —— importance 阈值 / 年龄衰减 / 二者组合；是否需要"复用命中次数"信号（该信号需第 2 步加字段，第 1 步先用 importance+age）。
3. **分层淘汰策略**：长期层淘汰按 importance×recency 得分，还是 LRU 式近因；短期层是否沿用现 FIFO 窗。
4. **分层承载形态**：新 `LayeredMemoryBackend` 实现 trait，还是 `LocalMemoryBackend` 内加 opt-in 双层开关。建议新后端，默认后端零改动，金样最稳。
5. **溯源字段是否本轮进 `Episode`**：建议留第 2 步，本轮分层模型不依赖溯源。
6. **快照兼容**：第 1 步分层条目不进 JSON v1；序列化是否需要表达层位，待第 2 步定版本口径。
7. **跨 agent 共享语义层**：建议本轮不涉，留第 3 步及以后。

## 六、留后与砍项条件

- 留后：碰撞体无关；记忆图谱/远程后端联调；跨 agent 共享；自动巩固调度；语义摘要合并（episode → summary）。
- 砍项条件（沿用 03 §3.4）：分层在默认金样造成打分或序列化漂移 → 回退单层 Vec，分层只走新后端；图谱后端联调不达标 → 保留 trait 与本地实现，远程实现延后。
- 红线：对外只表述"记忆后端接口已预留、分层 opt-in 骨架落地"，不写"已集成记忆图谱 / 跨 agent 共享记忆"；伙伴 schema 以官方披露为准。
