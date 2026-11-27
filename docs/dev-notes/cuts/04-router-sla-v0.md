# 第 4 刀：RFC 0003 v0 — 槽位路由表 + SLA 分级 + 决策台账

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

授权后落地的是 RFC 0003 的 **v0**：纯数据层 + 决策台账，**不动现有调用路径，默认关**。选链、熔断接线、deadline 截断、成本闸执行都按分期留给 v1/v2。

补丁：`patches/003-router-sla-v0.patch`（690 行，基于 `0387718`，叠在 001、002 之上）。

---

## 一、为什么 v0 只做数据层

RFC 0003 原本要解三件事：路由没有服务等级、推理时延不进帧预算、降级只换厂不换档。真要把这三件做穿，得包 `FallbackProvider`、改 `InferSystem` 的帧内等待、动 L7 契约——按仓库纪律那是 v1 的跨层改动。

v0 的目标是把**数据形状先钉死**：等级怎么表达、槽位表长什么样、一次决策留下什么记录。形状定下来之后，v1 接线时不用回头改类型；而在接线之前，这套类型不被任何调用路径使用，默认行为逐位不变。

## 二、落地内容（`crates/oriera-infer/src/router.rs`，新模块）

### 等级与隐私

- `ServiceTier::{Realtime, Balanced, Batch}`：语义只覆盖**最长等待、重试次数、降级顺序**，不约束选哪个模型。wire 字符串双向转换，未知串返回 `None`，不猜档。
- `Privacy::{Either, Local, Cloud}`：合规属性，独立于性能档——云/本地不是快慢问题。

### 槽位表

```rust
SlotRoute {
    id, tier, chain: Vec<String>, require: Vec<CapNeed>,
    privacy, budget: SlotBudget,
}
SlotBudget { max_inflight, max_ms_p95: Option<u32>, max_micro_per_1k_turns: Option<u64> }
```

- `chain` 是 preset id 顺序，`rule` 永远兜底；
- `require` 是能力前置（`tools / structured_output / image_input / prompt_cache`），`meets_caps(caps)` 逐条核对；未知 token 解析为 `None`，**fail closed**；
- 表是数据：宿主装配期构造，引擎不硬编码任何槽位。

### 决策台账

```rust
RouteDecision {
    seq, slot, tier, provider_id, chain_index, waited_us,
    degraded_from, exhausted: Option<Exhausted>,
    est_micro_cost, prompt_fingerprint,
}
```

- `RouteLedger` 环形 256，序号跨窗口单调（满了丢最旧，seq 不重置），`matches()` 逐条比对——与 `InferenceLedger`、召回台账同一套纪律；
- 纯观察数据，不进 `state_hash`；
- `est_micro_cost` 字段名带 `est_`：按牌价表估算，不参与任何验收门；
- `SlotRouter::observe(...)` 记账时从槽位表回填 tier；**槽位未注册则不入台账、不编造 tier**；`chain_index` 越界同样 fail closed，malformed 决策不落账。

### 刻意没做的

- 没包 `FallbackProvider`，没改 `ModelProvider` 调用路径；
- 没接 L2 `BudgetAllocator`（`waited_us` 先记在决策里，v1 才喂给仲裁器）；
- 没做成本闸/隐私闸的执行逻辑，`exhausted` 目前只是观察标记；
- 没落 `contracts/` 的 schema 文件；
- 没写 JS 镜像——v0 类型没有被任何运行路径消费，镜像同步等 v1 定型后一起做。

## 三、验收（实测）

| 项 | 结果 |
|---|---|
| `cargo test -p oriera-infer` | **71 passed**，0 failed |
| `cargo test -p oriera-agent --features json` | 93 passed（未受影响） |
| `cargo test -p oriera-agent --features "json,rigid3d,cam,spec,urdf"` | 93 passed |
| `cargo test -p oriera-infer --no-default-features` | 71 passed（新模块无 feature 依赖） |
| `cargo clippy --all-targets -D warnings` | 零告警 |
| `cargo fmt --check` | 干净 |
| `cargo check --workspace` | 13 crate + 两平台全过 |
| JS 镜像 `kernel.test.mjs` | 43 pass / 0 fail |
| 补丁可复现 | 干净 worktree 依次 apply 001 → 002 → 003，infer 71 / agent 93 全绿 |

测试覆盖的五条断言（对应 RFC 评审清单的 v0 部分）：

1. 规则档双跑：同表同决策流，台账逐条一致；
2. 未注册槽位：`route()` 返回 `None`，`observe()` 不入台账；
3. 台账环形裁剪：超容量丢最旧，序号从 11 续号不重置；
4. 决策回填 tier 来自槽位表，`degraded_from` / `exhausted` / 指纹如实落盘；
5. 能力前置：有 tools 满足、无 tools 不满足；未知 token 解析为 `None`。

## 四、补丁顺序

```bash
git apply patches/001-memory-scoring-config.patch
git apply patches/002-memory-backend-and-recall-ledger.patch
git apply patches/003-router-sla-v0.patch
```

001 是第 1 刀（系数可配置化），002 是第 2、3 刀（后端抽象 + 召回台账），003 是本刀。三者都基于 commit `0387718`。

## 五、v1 / v2 待办（需要评审后再动）

- **v1**：`SlotRouter` 包 `FallbackProvider`，按 tier 给 deadline 与重试策略；`waited_us` 进 `BudgetAllocator` 的 `infer` 段；Realtime 槽位 per-frame 截断。
- **v2**：成本闸与隐私闸执行；`exhausted` 从观察标记变成实际拦截；契约 schema 落 `contracts/oriera`。
- **开放问题**（RFC 里挂着）：成本口径按千回合还是按会话；隐私是否独立维度（当前实现是独立字段，倾向保持）；评测协议要不要为 SLA 开 `@v2`（建议先不动 `course@v1` / `paper@v1`，报告里加段 `sla` 即可）。

## 六、当前整体进度

| 刀 | 内容 | 状态 |
|---|---|---|
| 1 | 记忆打分系数可配置化 | 已落地，补丁 001 |
| 2 | 记忆后端抽象（门面 + trait） | 已落地，补丁 002 |
| 3 | 召回台账 + 进评测报告 | 已落地，补丁 002 |
| 4 | 路由表 + SLA 分级 + 决策台账（v0） | 已落地，补丁 003 |
| 5 | BudgetAllocator 吃推理时延（v1 接线） | 待评审，与 v1 同批 |
| 6 | L8 生成资产四道闸 / L9 记忆分层 | 未排期 |
