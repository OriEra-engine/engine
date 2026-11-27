# 第 6 刀：RFC 0003 v1-b-1 — 异步等待计量与分桶（仍不选模型）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

v1-b 拆两段：**b-1 低风险**把 off-frame 异步等待计量闭环；**b-2 高风险**再接选链/熔断/deadline。本刀只做 b-1，**不改 provider 选择、不接路由表、不触发仲裁写路径**。

补丁：`patches/005-async-wait-meter-b1.patch`（446 行，基于 `0387718`，叠在 001→004 之上）。

---

## 一、为什么要分桶

v1-a 的仪表是单累计。复核发现同步与异步两种等待的**折叠待遇不同**：

- **同步**调用阻塞在系统 `run` 内部，调度器实测墙钟已包含这段等待——再折进仲裁会双计，只能观测。
- **异步** job 在后台线程等待，发生在 `run` 之外，墙钟测不到——这段 off-frame 压力必须由仪表补记，将来由壳层折进 allocator。

混在一个桶里，壳层折叠时分不清哪笔能折、哪笔不能折。b-1 把仪表按**路径 + 系统名**分两桶，从结构上杜绝双计。

## 二、落地内容

### 1. `JobDone.elapsed_us`（`crates/oriera-infer/src/jobs.rs`）

- 作业从 `submit` 到完成的墙钟 µs，worker 线程内计时；成功、失败、panic 合成结果都记完整等待。
- 这是 off-frame 等待的原始数据，与 `usage`（token）并列，不互相替代。

### 2. 仪表分桶（`crates/oriera-infer/src/lib.rs`）

`InferWaitMeter` 演进为按系统名分两桶：

```rust
record_sync(system, us)   // 帧内同步等待，仅观测
record_async(system, us)  // off-frame 异步等待，可折叠
drain_async() -> Vec<(String, u64)>  // 只取异步桶并清零
sync_wait_us(system) / async_wait_us(system) / wait_ms()  // 观测读数
```

- 饱和累加防时钟异常回绕；零等待不插条目。
- **双计防护**：`drain_async` 只返回异步桶，同步桶不受影响——结构上保证同步等待不会被折叠二次加压。

### 3. 收割入账（`crates/oriera-agent/src/cognition.rs`）

- 认知 harvest、导演收割两处异步路径：`done.elapsed_us` 记入各自系统的 async 桶。
- 认知/导演同步路径仍记 sync 桶；系统名取各自 `name()`（`l9-cognition` / `l9-director`），折叠时压力落到正确的 allocator 行。

### 4. 折叠接线（API 备好，待装配）

壳层帧循环在 tick 后 `drain_async()`，按系统名把 ms 经 `Scheduler::observe_extra_cost` 折进对应段。引擎提供 API，装配期接线——与既有 B7 `regulator_hint_ms` 同款惯例。当前仓库尚无开异步的 example 落点，实际折叠随异步装配点接入。

## 三、明确没做（b-2 / v2 范围）

- 包 `FallbackProvider` 接 `SlotRouter` 的选链与熔断；
- Realtime 档 per-frame deadline 硬截断、重试次数；
- `RouteDecision.waited_us` 回填路由台账；
- 成本/隐私闸执行；契约 schema 落 `contracts/`。

## 四、验收

干净 worktree 从 `0387718` 顺序 apply 001 → 002 → 003 → 004 → 005，独立 target 复现：

| 项 | 结果 |
|---|---|
| `oriera-infer` | 74 passed（+1：`elapsed_us` 计量断言；分桶测试重写为 3 条） |
| `oriera-agent`（`json,rigid3d,cam,spec,urdf`） | 96 passed（+1：异步收割入账回归钉）；金样 1、doctest 3 |
| clippy | `-D warnings` 零告警 |
| rustfmt | `--check` 干净 |
| `cargo test --workspace` | 573 passed / 0 failed |
| JS 镜像 | 49 pass / 0 fail |

## 五、下一棒：b-2 提纲（评审后再动）

1. 壳层异步装配点接 `drain_async` → `observe_extra_cost`，补双计防护回归钉（同步帧不产生 extra-cost）；
2. `SlotRouter` 包 `FallbackProvider`，rule 档双跑决策逐位一致后再接云链；
3. Realtime 档 deadline 截断与重试；成本/隐私闸执行留 v2。
