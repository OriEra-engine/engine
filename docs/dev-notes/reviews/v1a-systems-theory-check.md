# 评审复审：v1-a（补丁 004）系统论对齐复核

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

- 评审日期：2026-11-23
- 评审对象：`patches/004-infer-wait-meter-v1a.patch`（`InferWaitMeter` + 认知同步路径计时 + RFC 0003 v1-a 清单）
- 评审方式：逐文件读码 + 对照系统论八原理五规律 + 仓库纪律（金样稳定、非破坏、AI 原生、契约版本化）+ 上一轮拍板遗留约束
- 复跑基线：infer 73 / agent 95（金样 1、doctest 3）/ workspace 571 passed 0 failed / JS 49 pass，clippy `-D warnings`、fmt 干净，干净 worktree 独立 target 复现

## 一、总体结论

**通过 v1-a（S1 已补掉）。** 方向与系统论框架一致：观测件与决策件分离、默认路径零漂移、计量单位对齐预算口径、新类型全 additive 不动 `provider.v1` 契约。无 must-fix 硬伤。

复审初版发现 **1 项 should-fix（测量口径完整性，S1）已按修法 A 落地**；**1 项挂账（开环中间态需撤回死线，S2）**；2 条 nit/观察留 v1-b。

## 二、八原理逐条核对

| 原理 | 核对结论 | 证据 |
|---|---|---|
| 层次性 | ✅ 通过 | `InferWaitMeter` 定义在 L7 infer，与既有 `UsageMeter`/`CostMeter` 同层同先例；L9 只经 `resource_or_default` 读写，未把计量逻辑下沉 L9，也未把策略旋钮放进 L7。无层次越界 |
| 开放性 | ✅ 通过 | 仪表不含厂商事实、不含 Key、不硬编码 provider；纯引擎内计量，准入/拒绝清单不受影响 |
| 目的性 | ✅ 通过 | µs 累加、`wait_ms()` 换算到 allocator 观测的 ms 单位；饱和累加防止时钟异常回绕。计量口径与 L2 预算账对齐 |
| 稳定性 | ✅ 通过 | 零等待不插资源；类型无 `Serialize` derive，不进快照、不进 `state_hash`；rule 后端帧行为逐位不变，金样稳定 |
| 自组织性 | ✅ 通过 | 仪表是他组织观测件：不参与 agent 决策、不自动调参、不反向修改默认路径；符合"错了不可回滚的归他组织"边界 |
| 整体性 | ⚠️ 部分 | 见 S1：同一帧两类模型调用（台词 / 导演 beat）被区别计量，整环等待读数不完整 |
| 信息反馈律 | ⚠️ 半环 | 见 S2：传感器已装，"→仲裁/策略调节"半环在 v1-b；当前仪表无消费者 |
| 相似性 | ✅ 不涉及 | 本刀未碰跨本体迁移结构 |

## 三、五规律对照

| 规律 | 核对结论 |
|---|---|
| 结构功能相关律 | ✅ 等待时间沉淀为一等计量类型（`InferWaitMeter`），而非散落在应用层临时变量 |
| 信息反馈律 | ⚠️ 感知半环落地，调节半环未接；同步路径读数不进仲裁（双计防护成立），异步路径传感器尚未装 |
| 竞争协同律 | ✅ 未与 `UsageMeter`（token）/`CostMeter`（花费）/`RouteLedger`（每-call 审计）重复造表，四者分工写明在类型文档 |
| 涨落有序律 | ✅ 慢调用作为压力信号被记录，不 panic、不静默吞掉；失败回复仍走既有 `unwrap_or_else` 规则兜底 |
| 演化优化律 | ✅ 测试钉住累加/排空/清零/饱和/零等待不插资源；drain-and-reset 语义明确，可做后续退化对照基线 |

## 四、发现的问题

### S1（should-fix，已修复）：DirectorSystem 同步路径漏计时，计量口径不完整

**事实**：`cognition.rs` 内两处同步 `provider.complete`：

- L382 认知台词调用——已用 `Instant` 包住并写 `meter_wait`；
- L777 导演 beat 调用——初版只调 `meter_usage`，**未计时**。

**系统论问题**：`InferWaitMeter` 类型文档承诺记录"one model call's wall-clock wait"，实际只覆盖 NPC 台词。认知与导演同被 L2 节拍器纳管（各自 `stretched_interval` / `stretched_interval_u64`，按 `throttle` 拉长间隔），将来节流依据若读这张表，导演段等待会被系统性漏算。这是**系统偏差**，不是随机噪声，采样平均消不掉。

**修复（已落地，修法 A）**：导演同步路径对称包 `Instant`，返回后同样 `meter_wait`；新增 `SleepProvider` 夹具与回归钉 `director_sync_call_feeds_the_wait_meter_too`，断言导演阻塞等待必须进同一仪表。两类调用同口径。

**退路（未采用）**：若有意先排除导演，需把类型与方法文档口径收窄为"NPC 台词调用等待"，并在 RFC 注明导演等待留 v1-b。

### S2（should-fix 挂账）：仪表当前无消费者，属开环中间态

**事实**：`InferWaitMeter` 资源不被 allocator 读取、不进报告/台账、shell 不折叠；同步路径读数只留在世界资源里，外部观测者当前拿不到。

**对照上一轮拍板**：v0 路由器类型已挂"v1 必须接上，否则撤回"的死线；本仪表是同类中间态——传感器已装、执行器未接。分期本身符合 v1-a/v1-b 拆分，但必须防止未接线 API 长期滞留。

**挂账要求**：

1. v1-a 仪表不进 release note、不进对外材料；
2. v1-b 必须接 off-frame 异步等待折叠 + 报告可选读数；逾期未接，与 v0 类型一并撤回；
3. RFC 遗留段显式写明本条死线。

### S3（nit）：异步路径文档承诺待落地

类型文档写"async job calls are timed there instead"，但 `JobDone` 目前无耗时字段，该字段是 v1-b 提纲项。注释不构成误导，但 v1-b 落地时需同步回填，避免文档承诺悬空。

### S4（观察）：双计防护缺回归钉

"同步等待已在 `run` 墙钟内、不重复折叠"的结论目前只在注释里。v1-b 若有人往同步路径也接 `observe_extra_cost`，没有测试拦截。建议 v1-b 补一条断言：同步帧不产生 extra-cost 折叠。本轮不阻塞。

## 五、横切纪律复核

| 检查项 | 结论 |
|---|---|
| `oriera.infer.provider.v1` 契约未动 | ✅ 新类型全 additive |
| 金样字节稳定 | ✅ 95 passed 含金样 1，指纹测试无漂移 |
| 无厂商硬编码、Key 不入引擎 | ✅ |
| 无重复造计量表 | ✅ 与 Usage/Cost/RouteLedger 分工清楚 |
| 干净树复现 | ✅ 001→002→003→004 两轮 worktree 全绿 |
| clippy `-D warnings` / fmt | ✅ 零告警 / 干净 |

## 六、拍板建议

1. **v1-a 主体：Go。** 系统论对齐无硬伤，非破坏、可观测、口径一致于既有预算体系。
2. **S1 已按修法 A 落地**：导演同步路径对称补计时 + `SleepProvider` 回归钉；补丁 004 已刷新（288 行），干净 worktree 独立 target 复现 infer 73 / agent 95 / workspace 571 全绿。
3. **S2 挂账进 RFC 遗留**：仪表与 v0 路由类型同列"未接线不得对外宣传、逾期撤回"清单。
4. **v1-b 前置项**：`JobDone` 补耗时字段 → 异步折叠 → 双计防护回归钉 → 再谈选链/熔断接线。
