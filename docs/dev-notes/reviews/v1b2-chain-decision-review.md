# 评审复核：v1-b-2 决策层（补丁 006）选链纯函数与双跑一致性

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

- 评审日期：2026-11-23
- 评审对象：`patches/006-router-resolve-decision-b2.patch`（`ChainCandidate` / `ResolvedPick` / `SlotRouter::resolve` / `ServiceTier::default_retries` + RFC 0003 b-2 决策层清单）
- 评审方式：逐文件读码 + 对照系统论八原理五规律 + 仓库纪律（金样稳定、非破坏、AI 原生、契约版本化）+ 拍板遗留约束（rule 档双跑逐位一致后再接云链）
- 复跑基线：infer 85 / agent 96（金样 1、doctest 3）/ workspace 584 passed 0 failed / JS 124 pass，clippy `-D warnings`、fmt 干净，干净 worktree 独立 target 复现

## 一、总体结论

**有条件通过 b-2 决策层（S1 已补掉）。** 选链判定落在 L7 纯函数层：不持 provider 实例、不跑熔断状态机、不改 L9 调用路径；三闸（熔断可见性 / 隐私 / 能力）合一判定，fail closed，rule 档双跑逐位一致。无 must-fix 硬伤。

本轮自评审发现 **1 项 should-fix（文档口径陈旧，S1）已修复**；**1 项挂账（开环中间态需撤回死线，S2）**；**2 项策略待拍板（P1）**；1 条 nit（S3）；3 条测试加固（S4）已落地。

> 边界重申：本刀只交付"决策"，不交付"接线"。注册表、包 `FallbackProvider`、deadline 截断、重试执行、L9 带 slot 调用，均属 #13 包装层接线，需评审 + 双跑 harness 后再动。

## 二、八原理逐条核对

| 原理 | 核对结论 | 证据 |
|---|---|---|
| 层次性 | ✅ 通过 | `resolve` / `ChainCandidate` / `ResolvedPick` 定义在 L7 infer；L9 未接触，未把选链策略下沉 L9，也未把实例解析塞进 L7 |
| 开放性 | ✅ 通过 | 候选快照不含厂商事实、不含 Key；`provider_id` 是 preset id 字符串，引擎不硬编码 provider |
| 目的性 | ✅ 通过 | 选链服务 SLA 分级；输出 `chain_index` / `provider_id` / `degraded_from` 与 `RouteDecision` 台账字段一一对齐 |
| 稳定性 | ✅ 通过 | `resolve` 是 `&self` 纯函数，无 `Instant`、无 IO、无随机；类型不进快照/`state_hash`；rule 后端帧行为逐位不变 |
| 自组织性 | ✅ 通过 | 不自动调参、不反向修改默认路径；候选快照由装配层喂入，路由器不自行探测健康状态 |
| 整体性 | ⚠️ 半环 | 三闸合一判定口径完整；但熔断状态来自外部 `breaker_open` 视图，路由器不自洽持有健康状态，整环健康感知在包装层 |
| 信息反馈律 | ⚠️ 半环 | 决策件已装，执行器（实例调用 / 熔断状态机 / 台账回填）未接；当前 `resolve` 无生产消费者 |
| 相似性 | ✅ 不涉及 | 本刀未碰跨本体迁移结构 |

## 三、五规律对照

| 规律 | 核对结论 |
|---|---|
| 结构功能相关律 | ✅ 选链沉淀为一等类型（`ChainCandidate` / `ResolvedPick`），而非散落在应用层临时判断 |
| 信息反馈律 | ⚠️ 决策半环落地，执行半环未接；与 v0 台账、b-1 仪表同属"传感器已装、执行器未接"中间态 |
| 竞争协同律 | ✅ 未重复造熔断状态机：`breaker_open` 只读视图，开合逻辑仍归 `health.rs` 的 `FallbackProvider`；与 `UsageMeter` / `CostMeter` / `RouteLedger` 分工清楚 |
| 涨落有序律 | ✅ 全不可用 / 缺行 / 未知槽位一律 fail closed 返回 `None`，不 panic、不硬撑、不臆造能力 |
| 演化优化律 | ✅ 双跑一致性 + 空链 / 乱序无关 / 跨多跳降级口径测试钉死，可作后续退化对照基线 |

## 四、发现的问题

### S1（should-fix，已修复）：三处结构注释停留在 v0"尚未选链"口径

**事实**：补丁初版沿用了 v0 注释——
- `RouteDecision`："v0 只记账；v1 才由路由器据此选链上条目"；
- `SlotRouter`："选链、熔断、deadline 截断是 v1 包 FallbackProvider 时的事"；
- `observe`："v0 路由器还不选 provider"。

**问题**：b-2 已落地纯函数选链（`resolve`），上述表述会误导后续接线者以为选链尚未开始，把"决策已做"与"接线未做"混为一谈。

**修复（已落地）**：三处注释同步为——`resolve` 给出候选 id，`RouteDecision` 是事后审计行；路由器不持实例、不发起调用，熔断开合 / 实例解析 / deadline 截断归包装层；`observe` 由包装层如实回填实际服务项。

### S2（should-fix 挂账）：`resolve` 当前无消费者，属开环中间态

**事实**：`resolve` / `ChainCandidate` / `ResolvedPick` 不被任何生产路径调用；认知/导演系统仍直接持有 provider 泛型，无 slot 概念；壳层 JS 走自己的 localStorage 装配，未消费这组类型。

**对照上一轮拍板**：v0 路由类型、v1-a 仪表已挂"未接线不得对外宣传、逾期撤回"死线；本刀决策层是同类中间态——决策件已装、执行器未接。分期本身符合 b-2 决策层 / 包装层拆分，但必须防止未接线 API 长期滞留。

**挂账要求**：

1. 本刀新类型不进 release note、不进对外材料；
2. #13 包装层接线必须接上注册表 + 实例解析 + 台账回填；逾期未接，与 v0 类型、b-1 仪表一并撤回；
3. RFC 遗留段显式写明本条死线。

### P1（待拍板策略，不阻塞决策层落地）

1. **Batch 档重试次数**：当前 `default_retries(Batch)=0`（成本优先不重跑）。Batch 不急、理论上反而有时间容忍有限重试；是否允许 1 次重试需结合成本闸口径拍板。当前实现偏保守，注释已写明可被装配层覆盖。
2. **HalfOpen 是否计入 `breaker_open`**：`health.rs` 熔断有 `Closed / Open / HalfOpen` 三态。包装层从 `health_report()` 映射候选视图时，`HalfOpen` 是放行试探（不标 open）还是跳过（标 open），需在接线时定策略并钉测试。本轮决策层不判状态，只读布尔视图。

### S3（nit）：`candidate_ok` 公开，存在绕过 `resolve` 自行遍历的风险

`SlotRoute::candidate_ok` 是 pub 方法，包装层理论上可绕过 `resolve` 自行遍历 chain 做判定，形成双份选链逻辑。建议 #13 接线时统一走 `resolve`，`candidate_ok` 仅作其内部判定复用（可考虑降为私有）。本轮不阻塞。

### S4（测试加固，已落地）

补三条边界回归钉：空 chain → `None`；候选快照乱序结果一致（顺序无关）；跨两跳以上降级时 `degraded_from` 仍记首项。连同既有 8 条，选链决策的边界口径已钉死。

## 五、横切纪律复核

| 检查项 | 结论 |
|---|---|
| `oriera.infer.provider.v1` 契约未动 | ✅ 新类型全 additive，`ModelProvider` trait 未改 |
| 金样字节稳定 | ✅ agent 96 passed 含金样 1，指纹测试无漂移 |
| 无厂商硬编码、Key 不入引擎 | ✅ |
| 未重复造熔断 / 计量表 | ✅ 熔断状态机仍归 `health.rs`，台账仍归 `RouteLedger` |
| 干净树复现 | ✅ 001→002→003→004→005→006 两轮 worktree 全绿 |
| clippy `-D warnings` / fmt | ✅ 零告警 / 干净 |
| JS 镜像 | ✅ 本轮未改 JS；壳层自有装配形态，v0 数据层类型本就未镜像，无遗漏 |

## 六、拍板建议

1. **b-2 决策层主体：Go。** 纯函数、fail closed、双跑逐位一致，非破坏、可独立验收；与既有预算 / 台账体系口径对齐。
2. **S1 已修复**：三处过时注释同步到 b-2 现状；补丁 006 已刷新（436 行），干净 worktree 独立 target 复现 infer 85 / agent 96 / workspace 584 全绿。
3. **S2 挂账进 RFC 遗留**：`resolve` 与 v0 路由类型、b-1 仪表同列"未接线不得对外宣传、逾期撤回"清单。
4. **P1 两项策略拍板后再进包装层**：Batch 重试口径、HalfOpen 映射策略。
5. **#13 包装层接线前置**：装配期 id→实例注册表 → 包装层解析实例并调 `observe` 落台账 → rule 档双跑 harness 逐位一致 → 再接云链；deadline 截断与重试实际执行随此段落地。
