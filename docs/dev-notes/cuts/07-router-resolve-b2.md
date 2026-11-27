# 第 7 刀：RFC 0003 v1-b-2 决策层 — 纯函数选链 + 档位重试口径（未接调用路径）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

b-2 是拍板标注的**高风险跨层段**。本轮只把**选链决策**落在 L7 纯函数层：不持 provider 实例、不跑熔断状态机、不改 L9 调用路径。装配期注册表、包 `FallbackProvider`、deadline 截断留评审后；rule 档双跑逐位一致是接云链的前置条件。

补丁：`patches/006-router-resolve-decision-b2.patch`（436 行，基于 `0387718`，叠在 001→005 之上）。

---

## 一、为什么先做纯决策层，不直接包 FallbackProvider

现场核对出的接线缺口：v0 路由器的 `SlotRoute.chain` 存的是 **preset id 字符串顺序**，而 `FallbackProvider` 持有的是 `Arc<dyn ModelProvider>` **实例**；中间缺一层装配期构造的 id→实例注册表。同时认知/导演系统当前不带 slot、直接持有 provider 泛型，完整接 router 属动 L9 调用路径的跨层高风险改动。

按仓库纪律（改架构/动契约先评审、跨层需 Core Team 多数同意）与拍板口径（rule 档双跑逐位一致后再接云链），本轮把可独立验证的**决策判定**先做扎实：给定候选行只读快照，纯函数返回首个过闸项；实例解析、熔断开合、超时取消仍在包装层一侧。这样双跑比对可以在不碰真实云链的前提下钉死。

## 二、落地内容（`crates/oriera-infer/src/router.rs`）

### 1. `ChainCandidate` — 候选行只读快照

装配期喂给路由器的链上候选视图：

```rust
pub struct ChainCandidate {
    pub provider_id: String,  // 与 SlotRoute::chain 项对齐
    pub caps: ModelCaps,
    pub local: bool,          // 本地开源行（无出口、无 Key）
    pub breaker_open: bool,    // 此刻跳过的可见性标记
}
```

- 路由器**不持实例、不跑熔断状态机**；`breaker_open` 只是"该行此刻被跳过"的标记，开合逻辑仍在 `health.rs`，由包装层从 `health_report()` 读入。

### 2. `ResolvedPick` — 纯决策结果

```rust
pub struct ResolvedPick {
    pub chain_index: usize,
    pub provider_id: String,
    pub degraded_from: Option<String>,  // 首项服务为 None，非首项记首项 id
}
```

### 3. `SlotRouter::resolve(slot_id, candidates) -> Option<ResolvedPick>`

按 `chain` 顺序返回首个过闸项，三道闸：熔断未开 + 隐私相符 + 能力满足。

- **未注册槽位** → `None`（不猜默认档，与 `route()` 同纪律）。
- **注册表缺该行** → fail closed 跳过，不臆造能力；链上全不可用 → `None`（调用方兜底，rule 行应在链尾且永不 open）。
- 以 `chain` 顺序为准，`candidates` 顺序无关，按 id 匹配。
- **纯函数**：不记台账、不发起调用；选中后由包装层调 `observe` 落账。

### 4. 隐私闸

`Privacy::{Either, Local, Cloud}` 与行 `local` 标志匹配：`Local` 跳过云端行，`Cloud` 跳过本地行，`Either` 都可。隐私是独立合规维度，不进性能档。

### 5. 档位重试口径 `ServiceTier::default_retries`

| 档 | 重试次数（不含首调） | 口径 |
|---|---|---|
| Realtime | 0 | deadline 内不重试，超时即降级到链下一项 |
| Balanced | 1 | 允许一次重试 |
| Batch | 0 | 成本优先，失败不重跑（重跑加价），直接降级 |

纯策略默认值，可被装配层按槽位预算覆盖；本轮只给口径，**不执行重试**。

### 6. 双跑一致性

同表同快照两次 `resolve` 逐位返回同一 `ResolvedPick`（测试钉死，与 `RouteLedger::matches` 同一套逐条比对纪律）。

## 三、明确没做（留包装层 / 评审后）

- 装配期 preset id → provider 实例注册表；`resolve` 只选出 id，实例解析在包装层。
- `SlotRouter` 包 `FallbackProvider`、真实熔断开合状态机接入（本轮只读 `breaker_open` 视图）。
- tier deadline 截断 / 超时取消：`complete` 现为同步阻塞、无 timeout 包装；重试的实际执行未接。
- 认知/导演系统带 slot 概念、接 router 调用路径（跨 L7/L9，高风险，需评审 + rule 双跑 harness）。
- `RouteDecision.waited_us` 回填台账、成本/隐私闸执行、契约 schema 落 `contracts/`。

> 挂账纪律：本轮新增的 `resolve` / `ChainCandidate` / `ResolvedPick` 与 v0 路由类型同列"未接线不得对外宣传"清单；包装层接线前不进对外材料。

## 四、验收

干净 worktree 从 `0387718` 顺序 apply 001 → 002 → 003 → 004 → 005 → 006，独立 target 复现：

| 项 | 结果 |
|---|---|
| `oriera-infer` | 85 passed（+11：选链首项/熔断跳过/能力不符/隐私闸/未知槽位/全不可用/缺行 fail closed/双跑一致/空链/候选乱序无关/跨多跳降级口径 + 档位重试映射） |
| `oriera-agent`（`json,rigid3d,cam,spec,urdf`） | 96 passed；金样 1、doctest 3 |
| clippy | `-D warnings` 零告警 |
| rustfmt | `--check` 干净 |
| `cargo test --workspace` | 584 passed / 0 failed |
| JS 镜像 | 124 pass / 0 fail（本轮未改 JS；壳层走自己的 localStorage 装配形态，v0 数据层类型本就未镜像） |

## 五、下一棒（评审后再动）

1. 装配期 id→实例注册表；包装层把 `resolve` 选出的 id 解析为 `FallbackProvider` 链上实例，选中后调 `observe` 落台账。
2. rule 档双跑 harness：同表同快照决策逐位一致后，再接云链；熔断开合状态机从 `health_report()` 喂入 `breaker_open`。
3. Realtime 档 deadline 截断与重试实际执行；成本/隐私闸执行留 v2。
4. b-1 遗留：壳层异步装配点接 `drain_async` → `observe_extra_cost`，补双计防护回归钉（同步帧不产生 extra-cost 折叠）。
