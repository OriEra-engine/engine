# 第 5 刀：RFC 0003 v1-a — 推理等待帧级计量（不选模型）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

评审拍板把 v1 拆成两步：**v1-a 低风险**，先把"等模型的时间"计量出来；**v1-b 高风险**，再接选链/熔断与异步折叠。本刀只做 v1-a，**不改 provider 选择、不动路由表、不接 allocator 写路径**。

补丁：`patches/004-infer-wait-meter-v1a.patch`（288 行，基于 `0387718`，叠在 001、002、003 之上）。

---

## 一、这一刀要解决什么

L2 帧预算仲裁（`BudgetAllocator`）原本只能看到调度器实测的系统帧内耗时。评审时核对发现一个之前没说透的事实：**认知系统的同步模型调用是阻塞在 `System::run` 内部的**，调度器包住整个 `run` 测墙钟，所以同步等待其实已经进了 EWMA；真正没被帧预算看见的，是异步 job 路径里 off-frame 的等待。

v1-a 先把等待时间作为一个**独立的、可观测的量**计量出来，把"模型等待"和"引擎工作"拆开。至于怎么折进仲裁、折到哪个段，等 v1-b 异步路径一起定，避免在同步路径上重复计数。

## 二、落地内容

### 1. `InferWaitMeter`（`crates/oriera-infer/src/lib.rs`）

帧级模型等待仪表，µs 计量：

```rust
pub struct InferWaitMeter { pub waited_us: u64 }

impl InferWaitMeter {
    pub fn add_wait_us(&mut self, waited_us: u64)   // 饱和累加，时钟异常不回绕
    pub fn drain_wait_us(&mut self) -> u64           // 取走并清零（drain-and-reset）
    pub fn wait_ms(&self) -> f32                    // µs → ms，对齐 allocator 观测单位
}
```

与既有两个表分工，不合并：

- `UsageMeter`：记 token 用量；`CostMeter`：记花费；
- `InferWaitMeter`：**帧级压力合计**（这一帧在模型上等了多久）；
- `RouteLedger`：**每-call 审计行**（哪个槽/哪个 provider、指纹是什么）。

### 2. 同步路径计时（`crates/oriera-agent/src/cognition.rs`）

- 两类同步模型调用**同一口径**：认知台词（`CognitionSystem`）与导演 beat（`DirectorSystem`）的 `complete(...)` 都用 `std::time::Instant` 包住，返回后交给私有 helper `meter_wait`。
- `meter_wait`：等待 > 0 才经 `resource_or_default::<InferWaitMeter>()` 记账；**零等待不插资源**，rule 后端的帧保持干净，默认路径零漂移。
- 异步 job 路径（`InferJobs` poll/收割）本轮**不接**，属 v1-b 范围。

### 3. 折叠纪律（评审现场核对后修正，写进类型文档与 RFC）

- **同步路径**：阻塞等待已包含在系统 `run` 的实测墙钟内，再用 `observe_extra_cost` 折一遍会**重复计数**。本路径下 meter 只做可观测读取，不往上叠加。
- **异步路径**：等待发生在 `run` 之外，墙钟测不到；v1-b 再把 `drain_wait_us()` 折算出的 ms 经 `Scheduler::observe_extra_cost` 折到对应的在途系统段。

## 三、明确没做（v1-b / v2 范围）

- provider 选择、`FallbackProvider` 熔断接线、tier 降级链；
- Realtime 档 per-frame deadline 硬截断；
- 异步 job 完成计时与 off-frame 折叠；
- `RouteDecision.waited_us` 回填路由台账；
- 契约 schema 落 `contracts/`。

## 四、验收

干净 worktree 从 `0387718` 顺序 apply 001 → 002 → 003 → 004 后复现：

| 项 | 结果 |
|---|---|
| `oriera-infer` 测试 | 73 passed（基线 71，+2：累加/排空/清零、饱和不回绕） |
| `oriera-agent`（`json,rigid3d,cam,spec,urdf`） | 95 passed（基线 93，+2：helper 记账 + 排空 + 零等待不插资源；导演同口径计时回归钉）；金样 1、doctest 3 |
| clippy | `-D warnings` 零告警 |
| rustfmt | `--check` 干净 |
| `cargo test --workspace` | 571 passed / 0 failed（ignored 为既有基线） |
| JS 镜像 | 49 pass / 0 fail |

## 五、系统论复审与 S1 修复

补丁 004 初版过了一轮系统论八原理五规律复核（详见 `../reviews/v1a-systems-theory-check.md`）：层次性、开放性、目的性、稳定性、自组织性、反馈感知半环均通过；发现一项 should-fix：

- **S1 导演路径漏计时**：初版只包了认知台词调用，导演 beat 的同步 `complete` 未计时，同一帧两类模型调用口径不一致，仪表读数系统性偏低。已按修法 A 对称补齐：导演路径同样 `Instant` 包裹 + `meter_wait`，并加 `SleepProvider` 夹具回归钉（导演阻塞等待必须进同一仪表）。
- **S2 开环中间态挂账**：仪表当前无消费者（不进仲裁/报告/台账），与 v0 路由类型同列"未接线不得对外宣传、v1-b 逾期未接则撤回"清单。
- 其余 nit：异步 `JobDone` 耗时字段、双计防护回归钉，归 v1-b。

## 六、下一棒：v1-b 提纲（评审后再动）

1. `JobDone` 补完成耗时字段，收割时按 job 归到对应说话人/系统；
2. 异步等待 `drain_wait_us` → `observe_extra_cost` 折进 L2 仲裁，确认折到的系统名与调度器注册名一致；
3. 包 `FallbackProvider` 接 `SlotRouter`，rule 档双跑逐位一致后再接云链；
4. Realtime 档 deadline 截断；成本/隐私闸执行留 v2。
