# #13 v1-b-2 包装层接线 — 现状调研与落地方案（待评审，未落码）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

- 日期：2026-11-23
- 范围：`crates/oriera-infer`（L7 路由/熔断/注册表）+ `crates/oriera-agent`（L9 cognition/director 调用面）
- 性质：RFC 0003 v1 跨层接线；**本文件只做现状盘点与分步方案，不含已落地代码**
- 关联：RFC 0003 §v1-b-2 遗留段、`../reviews/v1b2-chain-decision-review.md`（S2 挂账 / P1 待拍板 / S3 nit）、ADR 0004

## 一、现状盘点（已验证事实）

### 1. 决策层已落地（补丁 006，纯函数，不持实例）

`router.rs`：

- `SlotRouter { routes, ledger }`：`resolve(slot_id, &[ChainCandidate]) -> Option<ResolvedPick>` 纯函数选链；`observe(...)` 把已发生决策落 `RouteLedger`（256 环形）。
- `ChainCandidate { provider_id, caps, local, breaker_open }` 只读快照；路由器不查注册表、不发起调用。
- `ResolvedPick { chain_index, provider_id, degraded_from }`。
- `ServiceTier::default_retries` 仅给口径（Realtime 0 / Balanced 1 / Batch 0），**不执行重试**。

### 2. 执行层现状（未接决策层）

- `FallbackProvider`（`health.rs:182`）：持 `Vec<Arc<dyn ModelProvider>>` 实例链 + 逐 provider `ProviderHealth`；`complete` 内部按链序自行 `admits → complete`，**不经过 `SlotRouter::resolve`**；`health_report() -> Vec<(id, BreakerState, streak)>`。
- 熔断三态 `Closed / Open / HalfOpen`；`admits`：Closed 放行，Open 冷却后转 HalfOpen，HalfOpen 放行试探。
- `PROVIDER_PRESETS`（`provider.rs:955`）：每行 `id / local / caps: ModelCaps / price`，可直接组装候选视图的 `local` 与 `caps`。
- `ModelProvider::complete` 同步阻塞，**无 timeout 包装**；重试未执行。

### 3. L9 调用面现状

- `CognitionSystem<P: ModelProvider>`（`cognition.rs:145`）、`DirectorSystem<P>`（`:675`）泛型各持单个 `Arc<P>`；**无 slot 概念、不持 router**。
- `InferSystem`（`infer/lib.rs`）是命令队列资源，不持 provider 实例也不持 router。
- 调用链：L9 系统直接调 `self.provider.complete(...)`；记忆/台账/指纹各自记账，路由决策未参与。

## 二、接线缺口（对照 RFC b-2 遗留 79–81 行）

| # | 缺口 | 现状 | 落点 |
|---|---|---|---|
| G1 | 装配期 id→实例注册表 | `FallbackProvider` 链是装配时 `Vec`，无 id 索引；`resolve` 只回 id 字符串 | L7 注册表：preset id ↔ `Arc<dyn ModelProvider>` 只读索引 |
| G2 | 候选快照构建 | 无代码把 preset 表 + `health_report()` 汇成 `ChainCandidate` 列表 | L7 纯函数：输入健康快照 + preset 表，输出 `Vec<ChainCandidate>` |
| G3 | 包装层选实例 + 调用 + 台账回填 | `FallbackProvider` 内部自遍历；`observe` 无生产调用方 | 包装层：`resolve` 取 `ResolvedPick` → 注册表取实例 → 调用 → `observe` 回填 |
| G4 | deadline 截断 / 重试执行 | `complete` 同步阻塞；重试只有口径 | 按 tier 包 deadline；重试按 `default_retries` 口径落地（P1-1） |
| G5 | L9 带 slot 接入 | cognition/director 泛型持单 provider，无 slot | 系统装配期绑定 slot，经包装层取实例（跨层，高风险） |
| G6 | `waited_us` 折 L2 | 决策台账已记等待微秒，未折进 `BudgetAllocator` | `InferSystem` 把 `RouteDecision.waited_us` 折进 infer 段（`observe` 签名不动） |

## 三、分步建议（风险递增，前一步可独立离线验收）

### 步 A · 装配期注册表（L7 纯结构，最低风险）✅ 已落地（补丁 009）

- 新增 `ProviderRegistry`：装配期登记 `HashMap<String, Arc<dyn ModelProvider>>`，附每行 preset 的 `local / caps` 视图；按 id 只读索引取值，缺行 fail closed 返回 `None`。**索引本身不做选链、不做实例解析判定**；取值动作由步 C 包装层发起（对齐复核"实例解析归包装层"边界）。
- 不发起调用、不跑熔断；不接 L9。
- 离线夹具：登记/缺行/重复不覆盖/clone 独立（5 条回归）。

### 步 B · 候选快照构建辅助（L7 纯函数）✅ 已落地（补丁 014）

- 输入：`FallbackProvider::health_report()` + 注册表/preset 视图；输出 `Vec<ChainCandidate>`。
- `local / caps` 取自 preset 表；`breaker_open` 由 `BreakerState` 映射——**HalfOpen 口径做成显式 `HalfOpenPolicy` 参数（P1-2 的落点），默认 `ProbeAsAvailable` 与 `admits()` 现状一致，本模块落地不改变任何既有行为**。
- **id 空间不闭合已显式处理**：链上条目用 preset id（`rule`），实例 `id()` 报的是 `rule-v1`。按"精确 preset id → preset 默认 model 名"两级查找定位声明行，两级都不中则 fail closed 不进候选，但进 `unmatched` 清单留痕。
- **产出的 `provider_id` 是 preset id**（`ChainCandidate` 契约要求与 chain 对齐），实例 id 不进候选视图。初版误用实例 id 致 `resolve` 静默断链，已由补丁 **015** 修正；**014 须与 015 一起 apply**。
- 纯函数，同表同健康快照逐位一致；离线回归 8 条 + 端到端 harness 15 条（补丁 015）。

### 双跑 harness ✅（补丁 015，步 C 硬前置已解阻）

`crates/oriera-infer/tests/route_double_run.rs`，15 条，全程离线不调 `complete`。覆盖 health → snapshot → resolve → registry → observe 整链：双跑逐位一致（含多槽位台账流）、候选乱序无关、fail closed 贯通、id 空间翻译回归、HalfOpen 两策略可观测差异、越界 `chain_index` 不落账、能力闸拒绝未登记 tools 的行。

由此确立的接线口径：**registry 以 preset id 作 key** → `resolve` 返回 preset id → `registry.get(pick.provider_id)` 取实例 → `observe` 台账传 `instance.id()`（实例 id 形态）。两个 id 空间各归各位。

### 步 C · 路由包装层（L7，包 `FallbackProvider`）

- 包装对象：先构建候选快照 → `router.resolve(slot, &candidates)` → 注册表取实例调用 → 成功/失败如实 `observe` 回填台账（`provider_id / chain_index / degraded_from / waited_us / exhausted / est_micro_cost / fingerprint`）。
- 复用 `FallbackProvider` 既有熔断链，不重复造状态机；熔断视图仍只读。
- 重试按 tier 口径执行（Batch 口径待 P1-1）；deadline 截断在此段落地。

### 步 D · L9 带 slot 接入（跨 L7/L9，最高风险）

- cognition/director 装配期绑定 slot，经步 C 包装层取实例，替换直持 `Arc<P>` 泛型的调用面。
- `waited_us` 折 L2 `BudgetAllocator` infer 段；`memory@v1`/路由台账与具身评测同一套指纹账本。

### 双跑 harness（接云链前置）

rule 档同表同快照两次决策逐位返回同一 `ResolvedPick`；包装层决策与纯函数 `resolve` 结果一致。双跑未钉死前不接真实远程后端。

## 四、待拍板项

### P1-1 · Batch 档重试次数（评审 P1-1）

当前 `default_retries(Batch)=0`（成本优先不重跑）。Batch 不急、理论上有时间容忍有限重试；是否允许 1 次重试，需结合成本闸口径拍板。保守默认维持 0，装配层可覆盖。

### P1-2 · HalfOpen 是否计入 `breaker_open`（评审 P1-2）

`health_report` 给三态。构建候选视图时：
- 方案甲：HalfOpen 视为放行试探 → `breaker_open=false`（与 `admits()` 现状一致，HalfOpen 本就放行一次试探）；
- 方案乙：HalfOpen 视为跳过 → `breaker_open=true`（保守，试探流量也不走）。

**补丁 014 的处理**：做成 `HalfOpenPolicy` 显式参数，默认 `ProbeAsAvailable`（方案甲，等价现状，不引入行为变更）；`TreatAsOpen` 可在装配层一行切换。**策略决策仍留给装配层，代码不替装配层拍板。**

### S3 · `candidate_ok` 可见性（评审 nit）

`SlotRoute::candidate_ok` 当前 pub，存在包装层绕过 `resolve` 自行遍历、形成双份选链的风险。建议步 C 统一走 `resolve`，`candidate_ok` 降为私有（仅其内部复用）。

### 其他

- 注册表生命周期：装配期一次性登记，还是预留热更位（第 1 步建议一次性）。
- 成本估算 `est_micro_cost`：是否步 C 即接 `ModelPrice` 表估算，还是先记 0 留 v2。

## 五、纪律前置与红线

- **跨层审批**：RFC 0003 末节——v1（接 L2 仲裁、包 `FallbackProvider`）按 `GOVERNANCE.md` 需公开评论 ≥7 天 + Core Team 多数同意；步 A/B 为 L7 纯结构/纯函数，可先评审落地，步 C/D 属 v1 跨层，审批在前。
- **双跑前置**：rule 档逐位一致 harness 钉死前不接云链；远程后端未经真实压测。
- **S2 死线**：`resolve` 与 v0 路由类型、b-1 仪表同列"未接线不得对外宣传、逾期撤回"清单；步 C 未接上注册表 + 实例解析 + 台账回填前，新类型不进 release note / 对外材料。
- **非破坏**：默认金样路径（rule 直持 provider）行为逐位不变；包装层 opt-in 装配。
- **诚实深度**：注册表缺行 fail closed，不臆造能力；不支持向量/远程的后端可见降级，不返回空结果假装健康。

## 六、风险与砍项条件

- 包装层若在默认金样造成决策顺序或台账漂移 → 回退 v0 直持 provider，包装层只走 opt-in 装配。
- 远程/图谱后端联调不达标 → 保留 trait 与本地 rule 实现，远程实现延后。
- 步 D 若 L9 泛型改造面过大 → 拆为"系统持 slot id + 门面解析"两步，不一次推翻 cognition/director 类型参数。

## 七、建议下一步

1. 先评审本方案与 P1 两项口径；P1 拍板后步 A/B 可作为 L7 纯结构先行落地（离线可测、不跨层）。
2. 步 C/D 走 RFC v1 跨层审批 + 双跑 harness 后再动。
3. 步 A/B 落地前，#13 维持挂账，不改动生产调用路径。
