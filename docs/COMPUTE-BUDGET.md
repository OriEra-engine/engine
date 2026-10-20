# 算力分拨管线 — Compute Budget Pipeline

> 一句话理解：**预算从 L1 平台画像来，由 L2 仲裁器分，经 L0 词汇下发，
> 各层用拨杆花——帧先分后花，浪费在进帧之前就消掉。**
> （《OriEra 世界引擎》技术篇 T3 §3.5「算力分拨」、T15「计量横切」）

v0.2 的润滑剂是**每层各自刹车**：L5 盯着帧钟降画质、L7 写死队列上限、
L2 只会在帧爆了之后告警。v0.4 把它们升级为**一个账本 + 中央仲裁 + 各层拨杆**：
调度器在每帧开始前就决定每个系统拿到多少预算、被允许跑多快，
花的每一毫秒都先有出处。本文是这条管线的完整地图。

---

## 1. 为什么「先分后花」

AI 原生世界里有三种典型的算力浪费：

1. **挤兑**：认知、生成、物理、渲染在同一帧抢预算，谁都觉得自己最重要，
   结果一起超时，帧率崩给用户看。
2. **空转**：世界静止时物理照常积分，没人看时 NPC 照常调模型，
   每帧都在为「什么都不发生」付全价。
3. **重复**：同一帧里两个相同的意图排成两条命令，队列和世界各跑一遍。

分拨管线的对应解法：**仲裁**（L2 BudgetAllocator）治挤兑、
**空转跳过**（L6/L9）治空转、**命令合并**（L7 coalesce）治重复。
三者共同的前提是一个跨层共享的词汇——拨杆（`ComputeGrant`）。

## 2. 管线全景图

```
L1 平台画像 frame_budget_us（手机 30fps / AR 眼镜 90fps / PC 60fps）
        │
        ▼
L2 Scheduler::with_budget_ns ── 帧预算（grading 曲线）
        │  每帧 tick 开始：BudgetAllocator::allocate()
        │    Σ各系统 EWMA 耗时 vs 预算 → 份额表 + 节流拨杆
        ▼
L0 ComputeGrant { share_ms, throttle } ── 经 FrameCtx::grants 借用下发（零分配）
        │
        ├─→ L7 InferSystem   命令预算 ×throttle（下限 1 条/帧）+ submit 时合并
        ├─→ L8 GenAiSystem   in-flight 上限 ×throttle、轮询间隔 ÷throttle
        ├─→ L9 Cognition/Director  说话间隔 ×(1/throttle)（封顶 8×）
        └─→ L5 RenderSystem  独立迟滞调速器（v0.2 已有），吃同一根帧时钟
        │
        ▼
每帧 tick 结束：BudgetAllocator::observe(system_timings) ── 观测回灌，闭环
```

关键顺序：**先分拨、后运行、再观测**。第 N 帧的拨杆由前 N-1 帧的实测决定，
不存在「自己给自己分预算」。

## 3. 五个微模型（micro-models）

小、无状态或近无状态、每个都独立可测——「微」是设计约束，不是营销词。

### 3.1 L2 分拨仲裁器 `BudgetAllocator`

- **观测**：每系统耗时做 EWMA（α=0.2），冷启动用首个样本播种。
- **份额**：`share_i = budget × max(w_i, min_share)`，`w_i` 为平滑成本占比；
  每个系统保底 5% 预算，便宜的系统不会被饿死。
- **仲裁**（唯一信号：`load = Σ成本 / budget`）：
  - `load > 1` 持续 30 帧 → 全体按 `share/cost` 目标节流（比例公平，
    即大家同步降到 `1/load`；目标 ≥1.0 的小系统保持满速）。幂等：条件
    不消失就重导出同一目标，不会连乘崩塌。
  - `load < 0.6` 持续 120 帧 → 每系统 throttle +0.05，缓慢恢复。
  - 中间带 → 不动，双计数清零。**预算之内不折腾**——省不省是消费端的事。
- **节奏与 L5 调速器镜像**：快降（30 帧）慢升（120 帧），整个引擎同一呼吸频率。
- **下限**：throttle ≥ 0.1——被压制的系统至少保住十分之一速率，
  L7 再叠加「每帧至少 1 条命令」，队列只会变慢、不会饿死。
- **关断**：`AllocatorConfig::off()` → 表为空 → 一切系统读满速，逐位等于 v0.3 行为。

### 3.2 L0 分拨词汇 `ComputeGrant` / `Grants`

一个结构体 `{ share_ms, throttle }` + 一张只读表。查无此名 = 未纳管 = 满速——
**植入新系统/新模型不需要先去改仲裁器**，这是「减小植入阻碍力」的直接承诺。
表经 `FrameCtx::grants` 借用传入，稳态零分配。

### 3.3 L7 命令合并（coalesce）

`submit` 时对队尾做 O(1) 合并，只合并**可证安全**的相邻同型命令：

| 合并 | 为什么安全 |
|---|---|
| `Spawn(m)+Spawn(n) → Spawn(m+n)` | 一段 m+n 的循环与两段 m、n 循环消耗**同一条 LCG 随机流**，实体逐位一致（有测试证明） |
| `SetGravity(a)+SetGravity(b) → SetGravity(b)` | 纯资源覆写，前值不可观测，不耗随机流 |
| `Count+Count → Count` | 都在执行时读世界，二者等价 |

**不合并** `Teleport`/`SetSpeed`：浮点加法/乘法无结合律，
`(x+dx₁)+dx₂ ≠ x+(dx₁+dx₂)`（末位差异）——改变可观测数值的就不是去重，
是行为变更。这条边界写在 `submit` 的文档注释里，由测试看守。

### 3.4 L6 物理空转跳过（idle skip）

三个**数学上恒等于无操作**的条件（对有限浮点成立）：

1. 冻结时间：`dt·scale = 0` → 每个增量恰为 ±0.0；
2. 完全静止：零重力 + 所有速度恰为 0.0；
3. 没人在界外：积分的边界钳制对越界实体**即使速度为零也会生效**，
   所以只要还有实体停在界外，这一帧就不是无操作、不许跳
   （合并后 review #52 抓出的边界，修复前「逐位等价」在越界场景不成立）。

判定扫描是只读的 `query2`，与积分写循环覆盖同一批实体对，
两个条件都无法满足时立即放弃。

「看起来闲」（速度很小）的世界**照常积分**——近似是 L2 仲裁器的业务，
物理层永远不做近似。`idle_frames` 计数让「省下的工作」可观测。

### 3.5 L9 注意力节拍器（attention metronome）

预算压力下认知/导演的说话间隔按 `1/throttle` 拉长（封顶 8×）；
玩家一碰世界（`world.write_event(WakeEvent)`），认知当帧回到基础节拍——
**注意力是唯一高于账本的信号**。说话内容本身一个字不动：
省的是次数，不是质量。

## 4. 植入一个新模型：三步，零引擎改动

「减小植入模型阻碍力」的验收标准——新模型进世界，引擎层代码零修改：

1. **实现 `ModelProvider`**（L7 边界，一个 trait 两个方法），
   `last_usage()` 如实报告 token——计量自动接入 `UsageMeter`；
2. **壳层配置 `ProviderConfig`**（或直接 `RuleProvider` 离线开发）；
   Key 只存在壳层，引擎永不接触；
3. **挂上调度器**：你的系统不用关心分拨——没注册进仲裁器的系统读满速；
   想要被节流保护，只需 `ctx.grants.throttle_of(self.name())` 一行。

节流、计量、合并、空转跳过、降级……全部自动获得。反过来，引擎侧
新加一个耗算力的系统也同理：默认满速，接一行拨杆即纳管。

## 5. 确定性与回放

拨杆由墙钟观测驱动，属于**运行时调控**，不属于世界语义：它改变的是
「哪一帧做多少工作」，不改变「做了之后世界是什么样」。需要逐位重放
调度节奏的负载（如压测回放），用 `AllocatorConfig::off()` 关断，
行为与 v0.3 完全一致。世界状态回放（书 T5 RecordInput/Checkpoint）
不受影响：随机流在 L7 的固定种子 LCG 里，合并与节流都不改写它。

## 6. 观测清单（壳层读什么）

| 读数 | 出处 | 回答什么 |
|---|---|---|
| `Scheduler::system_timings()` | L2 | 这帧被谁吃了 |
| `Scheduler::allocator().load()` | L2 | 帧整体超没超订（>1 超订） |
| `Scheduler::headroom()` | L2 | 60 帧均值还剩多少预算 |
| `Scheduler::grants().get(name)` | L2 | 某系统这帧分到多少、被压到几成 |
| `InferSystem::coalesced` | L7 | 合并省掉了多少条命令 |
| `InferSystem::deferred / max_pending` | L7 | 洪峰有多高、摊了多久 |
| `PhysicsSystem::idle_frames` | L6 | 物理空转省了多少帧 |
| `UsageMeter` | L7 | 模型调用次数与 token 成本 |
| `RenderSystem::quality` | L5 | 画质被压到哪一档 |

PC 壳（`platforms/pc`）是全量接线示范：报告区直接打印 allocation 表。

## 7. 层契约对照

| 层 | 契约 | 拨杆怎么用 |
|---|---|---|
| L0 | `oriera.common.grants.v1`（`ComputeGrant`/`Grants`） | 只定义词汇，无行为 |
| L2 | `oriera.core.allocator.v1`（`BudgetAllocator`/`with_allocator`） | 产出拨杆 |
| L6 | `oriera.subsys.idle.v1`（`idle_frames` 空转跳过） | 恒等无操作早退 |
| L7 | `oriera.infer.adaptive.v1`（throttle 缩放 + coalesce） | 消费拨杆 + 去重 |
| L8 | `oriera.genai`（`GenAiLimits` 拨杆感知） | in-flight/轮询缩放 |
| L9 | `oriera.agent.cadence.v1`（`WakeEvent` + 节拍器） | 消费拨杆 + 注意力覆盖 |

一句话收束：**算力分拨不是让引擎变省，而是让浪费无处藏身——
每一毫秒先有出处，每一次省略都可证明，每一次压制都可恢复。**

## v0.7 成本记账（PLAN-deepseek-upgrade B5）

算力分拨管「这一帧花多少」，v0.7 的 `CostMeter` 管「这一局花多少钱」：
`UsageMeter` 计 tokens，`CostMeter` 按 `PROVIDER_PRESETS.price` 登记牌价
折算成钱——`TokenUsage.cached`（前缀缓存命中）按缓存价计，省下的部分
单列 `saved_micro`。未登记牌价的行照计量、记 `unpriced_calls`（本地行
电费口径）；跨币种不求和（`mixed_currency` 拒绝外汇幻觉）。分拨管节流，
账本管复盘，两本账互不越位。
