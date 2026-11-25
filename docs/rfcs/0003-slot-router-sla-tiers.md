# RFC 0003 — 任务槽位路由 + 服务等级分级：把"等模型的时间"接进帧预算

- 状态 / Status: **v0 已落地 / Landed v0**；**v1-a 已落地**（2026-11-23：推理等待帧级计量，不选模型）；v1-b 异步折叠与选链熔断接线、v2 成本/隐私闸待评审
- 影响契约 / Contract: 新增 `oriera.infer.route.v1`（纯数据 schema）；`oriera.infer.provider.v1` **不动**
- 性质 / Nature: 能力补全（路由分级 + 时延反馈 + 成本闸），非破坏
- 关联 / See also: RFC 0002（注册表与槽位路由表：本 RFC 只补它没定的"分级与计量"）、`docs/BENCH-CONTRACT.md`（分段计时口径）、`docs/COMPUTE-BUDGET.md`

---

## v0 落地清单（2026-11-23）

已实现（纯数据层，不动调用路径，默认关）：

| RFC 提案 | 落地形态 |
|---|---|
| `ServiceTier` | `realtime / balanced / batch`，wire 字符串 `as_str` / `from_wire` 双向，未知串 `None` 不猜档 |
| `Privacy` | `either / local / cloud`，独立于性能档 |
| `SlotBudget` | `max_inflight / max_ms_p95 / max_micro_per_1k_turns`，`None` = 不设闸 |
| 能力前置 | `CapNeed{tools, structured_output, image_input, prompt_cache}`，`SlotRoute::meets_caps`；未知 token 解析为 `None`（fail closed） |
| `SlotRoute` | `id / tier / chain / require / privacy / budget`，表是数据，引擎不硬编码槽位 |
| `RouteDecision` | `seq / slot / tier / provider_id / chain_index / waited_us / degraded_from / exhausted / est_micro_cost / prompt_fingerprint` |
| `RouteLedger` | 环形 256，序号跨窗口单调，`matches()` 逐条比对；纯观察，不进 `state_hash` |
| `SlotRouter` | 持有表 + 台账；`route(id)` 解析、`observe(...)` 记账；**未注册槽位不入台账、不编造 tier** |

v0 **没做**（按 RFC 分期留给 v1/v2）：包 `FallbackProvider` 的选链与熔断接线、deadline 截断、成本/隐私闸执行、`RouteDecision.waited_us` 进 L2 `BudgetAllocator`、契约 schema 落 `contracts/`。

验收：`oriera-infer` 71 passed、`oriera-agent` 93 passed、clippy `-D warnings` 零告警、fmt 干净、workspace `cargo check` 全过、JS 镜像 43 pass。补丁 `003-router-sla-v0.patch`。

---

## v1-a 落地清单（2026-11-23：推理等待帧级计量，不选模型）

评审拍板把 v1 拆两步：v1-a 低风险只把等待时间计量出来；v1-b 高风险再接选链/熔断与异步折叠。

| 提案 | 落地形态 |
|---|---|
| `InferWaitMeter` | infer 资源：`waited_us` 帧级累加；`add_wait_us` 饱和累加、`drain_wait_us` 取走清零、`wait_ms()` µs→ms 换算。与 `RouteLedger` 分工：本仪表是帧级压力合计，台账是每-call 审计行，不合并 |
| 同步路径计时 | cognition 同步 `complete(...)` 用 `Instant` 包住，返回后 `meter_wait` 写入 meter；**台词与导演 beat 两类调用同口径**，导演路径对称计时。零等待不插资源（rule 后端帧保持干净）。异步 job 路径本轮不接 |
| 折叠纪律（评审现场核对后修正） | 同步路径的阻塞等待**已包含**在系统 `run` 的实测墙钟内，再折进 `observe_extra_cost` 会重复计数；本轮 meter 只做可观测——把"模型等待"与"引擎工作"拆开可见。off-frame 异步等待的折叠留 v1-b |

v1-a **没做**：provider/选链/熔断接线、tier deadline 截断、异步 job 完成计时、`RouteDecision.waited_us` 回填路由台账、契约 schema 落 `contracts/`。

验收：`oriera-infer` 73 passed（+2）、`oriera-agent` 95 passed（+2：helper 记账 + 导演同口径计时）、clippy `-D warnings` 零告警、fmt 干净、workspace 测试全过、JS 镜像 49 pass。补丁 `004-infer-wait-meter-v1a.patch`。

---

## 背景

L7 现在能做的事已经不少：`ModelProvider` 最小接口（`id / complete / last_usage`）、`ProviderPreset` 表带 `ModelCaps`（上下文/结构化输出/工具/图像/prompt cache）与 `ModelPrice`（µ-currency per 1M tokens，按日期登记的快照）、`FallbackProvider` 带逐 provider 熔断（阈值 3、冷却 30s）与降级链、`InferJobs` 有 `max_inflight` 上限。RFC 0002 已经把"槽位路由表"以数据形态提出来。

缺口不在"能不能路由"，在**路由没有服务等级，决策没有计量**：

1. **Tier 缺位。** 实时对话的 NPC 和批处理的世界生成，用的是同一套超时与重试。结果是要么实时被长尾拖死，要么批处理被过短超时切碎。"要快要省"目前只能靠调用方各自拍脑袋。
2. **时延不进预算。** L2 `BudgetAllocator` 的 `observe(&[(stage, ms)])` 走帧内 EWMA，`infer` 段只有"帧循环自己花的毫秒"，**等模型的时间不在里面**。所以帧率抖动一旦来自推理，仲裁器看不见，也调不动。
3. **降级只换厂，不换档。** 熔断后走链上下一条 provider。但从"低延迟云档"降到"本地小模型"这种**档位降级**没有表达方式，链是扁平顺序。
4. **成本无上限。** `ModelPrice` 只是牌价表，运行时没有"这次会话最多花多少"的闸。多槽并发时成本可以无声超支。

后果是可测的：帧时间分布不可解释、成本不可预算、评测里没法声明 SLA（"这个场景 P95 ≤ 33ms"写不进去，因为没有东西在守它）。

## 提案

### 1. 槽位表是数据，不是代码

按 `spec` 的"实验即文件"惯例，槽位表连 schema 都走文件：

```toml
[[slot]]
id = "npc_dialog"
tier = "realtime"          # realtime | balanced | batch
chain = ["zhipu-flash", "deepseek", "ollama-local", "rule"]
require = ["tools"]        # ModelCaps 的硬性前置，不满足就跳过该行
privacy = "either"         # cloud | local | either

[slot.budget]
max_inflight = 2
max_ms_p95 = 600           # realtime 档的守门值（含网络）
max_micro_per_1k_turns = 200000
```

`rule` 永远兜底，且 `rule` 档必须能通过全部断言——离线可跑是既有红线。

### 2. Tier 只约束三件事

`ServiceTier::{Realtime, Balanced, Batch}` 各自的语义限定为：**最长等待、重试次数、降级顺序**。明确**不**约束"选哪个模型"——那是槽位表（数据）的事，引擎不替用户做内容决策。这条边界写进类型文档，避免 tier 变成策略入侵的入口。

### 3. `SlotRouter`（`oriera-infer/src/router.rs`，新增模块）

包住现有 `FallbackProvider`，复用它的熔断与健康状态，外加 tier 语义：

- `Realtime`：调用带 deadline，超时即返回 `InferError::Timeout` 并在决策里标 `degraded`，**绝不阻塞帧**；
- `Balanced`：允许一次重试，deadline 放宽；
- `Batch`：成本优先，可以等，但要受 `max_micro_per_1k_turns` 约束。

### 4. 决策是观察记录，与召回台账同纪律

```rust
RouteDecision {
  slot, tier, provider_id, chain_index, waited_us,
  degraded_from: Option<String>, exhausted: Option<Exhausted{ cost | deadline }>,
  est_micro_cost: u64,          // 由 ModelPrice 表估算，标 estimate
  prompt_fingerprint: u64,      // 复用 provider.rs 现成的那把
}
```

进 ring 台账（容量 256、满丢最旧、序号单调）、**不进 `state_hash`**、**不注册零成本**。这是上一轮 `RecallLedger` 已经验证过的形态，直接复用同一套纪律，不新造概念。

### 5. 接 L2：只加数据，不改签名

`InferSystem` 把 `RouteDecision.waited_us` 折进 `BudgetAllocator::observe` 的 `infer` 段。`observe` 签名不动——它本来就吃 `&[(String, f32)]`。Realtime 槽位再加一个 per-frame deadline（帧预算内的硬截断），由帧循环侧传进来。

> v1-a 修订（见上节）：同步阻塞等待已在 `run` 墙钟内，不重复折叠；`InferWaitMeter` 先做拆分计量。off-frame 异步等待经 `drain_wait_us` → `observe_extra_cost` 的折叠与 deadline 截断归 v1-b。

### 6. 契约

新增 `contracts/oriera` 的 `oriera.infer.route.v1`：槽位表 schema + 决策记录 schema，**纯数据**，不带行为。`oriera.infer.provider.v1` 一个字节不改。

## 可测断言（RFC 必附，否则不评审）

1. `rule` 档双跑：同一输入序列产出的 `RouteDecision` 序列逐位一致（含 `prompt_fingerprint`）。
2. 熔断 Open 时：选中链上下一条，且 `degraded_from` 非空、`chain_index` 递增。
3. `Realtime` 档遇到故意慢的假 provider：返回 `Timeout`，帧 tick 不被拖长（用 `FrameCtx` 计时断言，不看墙钟）。
4. 成本预算耗尽：决策显式标 `exhausted.cost`，**不静默降级、不伪造回复**（沿用"错误要如实报"的既有口径）。
5. 不注册 `SlotRouter`：全部既有测试与金样指纹逐位不变（默认路径零漂移）。

## 替代方案

| 方案 | 否决理由 |
|---|---|
| tier 写进 `ModelProvider` 内部 | 破坏"引擎不硬编码厂商策略"；且复用不到 `FallbackProvider` 的熔断链 |
| 应用层做路由 | 等于把 SLA 交给每个垂直版 fork，违反"垂直版不 fork 引擎，只拧三个旋钮" |
| 保持现状（全局单模型 + 扁平链） | 成本与时延不可控，评测无法声明 SLA——这正是本 RFC 要解的问题 |
| 在 `ProviderPreset` 里加"推荐 tier"字段 | 表本身可以加**建议**字段（保持 `Eq`，用整数/枚举），但**约束**必须在槽位表里，不能混 |

## 影响面

- **L7**：新增 `router.rs` 一个模块 + 一个数据契约。`provider.rs` / `health.rs` / `jobs.rs` 不动。
- **L2**：`BudgetAllocator` 只多吃一种 `observe` 数据，接口与节流算法不动。
- **L9**：可选消费 `RouteDecision`（例如把"这次等太久"写进角色状态），非必须。
- **平台壳**：槽位表与成本闸配置由壳提供，引擎只解析——Key 与预算值都不进仓库。

## 兼容性

非破坏。默认不注册 `SlotRouter` = 今天的单 provider 直连行为；`chain` 只有一项时路由退化为直通。迁移路径：先只落 v0（数据 + 决策记录，不动调用路径），确认台账与指纹无漂移后再接 allocator。

## 风险

1. **台账膨胀**：多槽并发时 256 条窗口可能几秒就满。缓解：默认只记 `degraded` 与 `exhausted` 事件，全量记录 opt-in。
2. **tier 语义变成策略入侵**：缓解就是第 2 条的三项限定，写进文档 + 评审时按这条卡。
3. **成本估算失真**：`ModelPrice` 是带日期的快照，模型调价就错。缓解：字段名带 `est_`，报告里标 estimate，不参与任何验收门。

## 开放问题（请在评论里答）

1. 成本闸按"每千回合"还是"每会话/每世界日"？前者与评测对齐，后者与账单对齐，两者都要就得双闸。
2. 隐私（cloud/local）是 tier 的一个维度，还是独立字段？我倾向独立——它是合规属性，不是性能属性。
3. 评测协议要不要动？建议**不动** `course@v1` / `paper@v1`，只在报告里加一段 `sla` 证据，等真实客户提出 SLA 条款再考虑 `@v2`。

## 投票

v0（数据层 + 决策台账，不动调用路径）已由项目所有者直接授权落地。v1（接 L2 仲裁、包 `FallbackProvider`）与 v2（成本/隐私闸执行）按 `GOVERNANCE.md` 仍需公开评论 ≥7 天 + Core Team 多数同意。
