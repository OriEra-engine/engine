# 第 11 刀 · #13 步 B：候选快照纯函数（补丁 014）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

- 日期：2026-11-24
- 范围：`crates/oriera-infer/src/candidates.rs`（L7 装配辅助层）
- 性质：RFC 0003 v1-b-2 包装层接线 **步 B**；纯函数，离线可测，不接调用路径
- 关联：`../plans/infer-wrapper-wiring.md` 步 B；评审 v1b2 P1-2

## 一、本轮落地

新增 `candidates.rs`，导出 `snapshot_candidates` 与 `HalfOpenPolicy`；`lib.rs` 补模块声明与再导出。

### `snapshot_candidates`

签名：
```rust
pub fn snapshot_candidates(
    health: &[(&str, BreakerState, u32)],
    presets: &[ProviderPreset],
    policy: HalfOpenPolicy,
) -> (Vec<ChainCandidate>, Vec<String>)
```

把 `FallbackProvider::health_report()` 的原始输出与 preset 表合成 `SlotRouter::resolve` 要吃的候选视图。

- `candidates`：顺序与 `health` 一致（装配链序）；`provider_id` 是**声明行的 preset id**——`ChainCandidate` 的既有契约要求它与 `SlotRoute::chain` 对齐，`resolve` 正是按它匹配。实例 id 不进候选视图。

> **修订（补丁 015）**：本刀初版把 `provider_id` 写成了健康快照的实例 id，违反 `ChainCandidate` 契约，导致 `resolve` 匹配不上 chain、整条链静默判死。双跑 harness 抓到该缺陷，已由补丁 015 修正为 preset id。**014 需与 015 一起 apply**，单取 014 会引入断链。详见 `12-double-run-harness-id-fix.md`。
- `local / caps`：来自 preset 表声明，不臆造。
- `unmatched`：查不到声明的 id 清单。

### 边界三条（评审口径）

1. **纯函数**：无 IO、无 `Instant`、不读熔断内部、不发起调用；同输入逐位同输出，可直接进双跑 harness。
2. **不判健康**：只做 `BreakerState → bool` 映射；开合逻辑仍归 `health.rs`。
3. **缺行 fail closed**：查不到声明的 id **不进候选**（不臆造 caps/local），但**进 `unmatched` 清单**——缺失必须可见，不静默丢弃。

### id 空间不闭合的显式处理

这是本轮发现并解决的真问题：

| 命名空间 | 值 |
|---|---|
| 链上条目（`SlotRoute::chain`） | preset id：`rule` / `zhipu` / `openai` |
| 实例 `ModelProvider::id()` | `rule-v1`（preset `rule` 行默认 model 名） |

两者不属于同一命名空间。按**两级查找**闭合：
1. 精确 preset id 优先（装配层显式声明过）；
2. 其次按 preset 默认 model 名（`rule-v1 → rule` 命中）。

两级都不中即 `unmatched`，**绝不引入硬编码别名表、绝不猜**。

### P1-2 的处理（不替装配层拍板）

`HalfOpenPolicy` 做成显式参数：

- `ProbeAsAvailable`（**默认**）：HalfOpen 视作可用，`breaker_open = false`。与 `ProviderHealth::admits()` 现状一致（半开本就放行一次试探）——**默认值意味着本模块落地不改变任何既有行为**。
- `TreatAsOpen`：保守跳过，`breaker_open = true`。

策略选择权留在装配层，代码不写死。P1-2 从"落码阻塞项"变成"运行时配置项"。

## 二、验收

- `oriera-infer`：98 passed（+8 候选快照回归）+ doctest 1
- `cargo test --workspace`：612 passed / 0 failed / 4 ignored（基线 604 + 8）
- `oriera-agent --features json,rigid3d,cam,spec,urdf`：103 passed，无漂移
- clippy `-D warnings`：零告警
- `cargo fmt --all -- --check`：干净

### 8 条回归

1. Closed → `breaker_open=false`，`provider_id` 为 preset id（015 修正），`local/caps` 取自 preset；
2. Open → `breaker_open=true`（两策略下均跳过）；
3. HalfOpen 两策略各一（甲放行 / 乙跳过）；
4. 精确 preset id 优先于 model 名匹配；
5. 未知 id fail closed：不进候选、进 `unmatched`、不臆造 caps；
6. 空 health → 空快照；
7. **同输入逐位一致**（双跑可用）；
8. 候选顺序跟随 `health` 顺序而非 preset 表顺序。

## 三、补丁

- `014-infer-candidate-snapshot-step-b.patch`：268 行 / 10758 字节，2 文件 +244/-0
  - `crates/oriera-infer/src/candidates.rs`（新增，242 行）
  - `crates/oriera-infer/src/lib.rs`（+2：模块声明、再导出）
- 基于 `0387718`，顺序叠 001–013 后 apply；`git apply --check` 通过，apply 后复跑 infer 98 全绿。

## 四、未做项

- **步 C 包装层**：resolve → 注册表取实例 → 调用 → `observe` 回填。属 v1 跨层，需 RFC 审批（公开评论 ≥7 天 + Core Team 多数同意）+ rule 双跑 harness 钉死。本轮仍未触碰调用路径。
- **P1-1** Batch 档重试次数（当前 0，是否允许 1）。
- **S3** `candidate_ok` 是否降私有。
- 新类型按 S2 死线不进 release note / 对外材料。

## 五、对步 C 的输入

步 C 只需三步串起来，两侧接口已就绪：

1. `snapshot_candidates(health, PROVIDER_PRESETS, policy)` 得候选；
2. `router.resolve(slot, &candidates)` 得 `ResolvedPick`；
3. `registry.get(pick.provider_id)` 取实例 → 调用 → `router.observe(...)` 回填台账。

`unmatched` 非空时应报装配错误（这些实例无声明，不该进链）。
