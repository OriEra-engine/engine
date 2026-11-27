# 第 10 刀 · #13 步 A：装配期 ProviderRegistry 纯结构（补丁 009）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

- 日期：2026-11-23
- 范围：`crates/oriera-infer`（L7 装配索引层）
- 性质：RFC 0003 v1-b-2 包装层接线 **步 A**；纯装配结构，离线可测，不接调用路径
- 关联：`../plans/infer-wrapper-wiring.md` 步 A；评审 v1b2 复核 S2/P1/S3

## 一、本轮落地

新增 `crates/oriera-infer/src/registry.rs`，导出 `ProviderRegistry`；`lib.rs` 补模块声明与再导出。

### `ProviderRegistry` 职责

装配期 `preset id` → 实例句柄 + preset 声明视图的只读索引：

- `register(id, Arc<dyn ModelProvider>, local, caps) -> bool`：登记；**重复 id 不覆盖**，返回 `false`（fail closed，装配层先排查重复配置）。
- `get(id) -> Option<&Arc<dyn ModelProvider>>`：取实例句柄；未登记 `None`。
- `local_of(id) -> Option<bool>` / `caps_of(id) -> Option<ModelCaps>`：取声明视图；未登记 `None`。
- `len / is_empty / ids()`：遍历与排障。
- `Clone`：映射表独立，实例句柄经 `Arc` 共享（装配语义）。

### 边界（按评审三条卡）

1. **不选链、不跑熔断、不发起调用**：选链归 `router.rs` 纯函数，熔断开合归 `health.rs`，发起调用归步 C 包装层。
2. **不感知健康状态**：不读 `BreakerState`；候选快照的 `breaker_open` 视图由步 B 从 `health_report()` 映射。
3. **缺行 fail closed**：未登记 id 一律 `None`，不臆造能力、不回退默认实例。

层次对齐评审：实例解析判定归包装层；注册表只是 id → 实例索引，取值动作由包装层发起。

## 二、验收（独立 target 干净复现）

- `oriera-infer`：90 passed（+5 注册表回归）+ doctest 1
- `cargo test --workspace`：604 passed / 0 failed / 4 ignored（基线 599 +5）
- `oriera-agent --features json,rigid3d,cam,spec,urdf`：103 passed，无漂移
- JS 镜像：130 pass / 0 fail
- clippy `--workspace --all-targets --features json,rigid3d,cam,spec,urdf -- -D warnings`：零告警
- `cargo fmt --all -- --check`：干净

### 新增回归（5 条，离线 RuleProvider 夹具）

1. 登记后实例句柄与 local/caps 视图均可取；
2. 未知 id 三类查询全 `None`（fail closed）；
3. 重复 id 拒绝覆盖，原条目与实例句柄不变；
4. clone 映射表独立、实例 `Arc` 共享；
5. 空注册表 `is_empty / len / ids` 口径。

## 三、补丁

- `009-infer-provider-registry-step-a.patch`：208 行 / 7405 字节，2 文件 +184/-0
  - `crates/oriera-infer/src/registry.rs`（新增，182 行）
  - `crates/oriera-infer/src/lib.rs`（+2：模块声明、再导出）
- 基于 `0387718`，顺序叠 001–008 后 apply；`git apply --check` 通过，apply 后复跑 infer 全绿。

## 四、未做项与阻塞（步 B/C/D 前置）

- **步 B 候选快照纯函数**：依赖 P1-2（HalfOpen 映射：放行试探 vs 跳过），待拍板。
- **步 C 包装层**：resolve → 注册表取实例 → 调用 → `observe` 回填；属 v1 跨层，需 RFC 审批（公开评论 ≥7 天 + Core Team 多数同意）+ rule 双跑 harness 钉死。
- **步 D L9 带 slot**：跨 L7/L9，随步 C 之后。
- **P1-1** Batch 档是否允许 1 次重试；**S3** `candidate_ok` 是否降私有。
- 本轮注册表未接 `create_provider` 装配链路、未被任何生产路径消费；新类型按 S2 死线不进 release note / 对外材料。

## 五、纪律核对

- additive：`ModelProvider` trait、既有契约未改；
- 金样/指纹无漂移；无厂商硬编码、Key 不入引擎；
- 未重复造熔断/计量表；离线夹具 hermetic。
