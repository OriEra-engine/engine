# 第12刀 · #13 步 C 前置：双跑 harness + 一处真 bug 修正

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

补丁 **015** · 基于 `0387718`（v0.13.0），顺序叠在 001–014 之后。

---

## 一、这一刀做了什么

两件事，第二件是第一件的产物：

1. **补上 RFC 0003 rule 档的双跑 harness**——步 C 接云链前的硬前置，此前只有各层单测，没有"整条链接起来"的回归。
2. **harness 一上来就抓到一个真 bug**：步 B（补丁 014）的候选快照把 `provider_id` 写成了实例 id，导致 `resolve` 永远匹配不上 chain，整条链静默判死。已修正。

**这就是双跑 harness 的价值**：步 A 的 5 条单测、步 B 的 8 条单测各自全绿，612 个 workspace 测试也全绿——但两层串起来是断的。单测钉的是各层自己的口径，钉不住层与层之间的 id 约定。

---

## 二、bug 的根因：三个 id 空间被混用了两个

`ChainCandidate.provider_id` 的字段注释写得很清楚：

> preset id（与 `SlotRoute::chain` 里的项对齐）。

而步 B 的实现保留了 `health_report()` 报来的原始 id，并在注释里写"id stays as health reported it"——**方向错了**。

三个空间必须分清：

| 空间 | 形态 | 谁在用 |
|---|---|---|
| **preset id** | `rule` / `zhipu` | `SlotRoute::chain`、`ChainCandidate::provider_id` |
| **实例 id** | `rule-v1`（`rule` 行的默认 model 名） | `ModelProvider::id()`、`health_report()`、`RouteDecision::provider_id`（台账） |

`health_report` 报实例 id，`resolve` 按 preset id 匹配 chain。步 B 的职责就是这次翻译，但它翻译了 caps/local 却漏了 id 本身。

**故障表现**（探针实测）：

```
health = [("rule-v1", Closed, 0)]     chain = ["rule"]
→ candidate ids = ["rule-v1"]          unmatched = []
→ resolve = None                        ← 声明查到了，链路仍然断
```

`unmatched` 为空说明两级查找成功，所以从步 B 自己的单测看一切正常；断在下一步。

### 修法

`snapshot_candidates` 产出的 `provider_id` 改为 `preset.id`。实例 id 不进候选视图——包装层回填台账时再从实例上取 `id()`，两个空间各归各位。

由此推出**步 C 的正确接线口径**（写进模块头文档）：

1. registry 以 **preset id** 作 key 登记实例（`register("rule", Arc<RuleProvider>)`，实例自己的 `id()` 仍是 `rule-v1`）；
2. `resolve` 返回 preset id；
3. `registry.get(pick.provider_id)` 取实例；
4. 调 `observe` 时台账传 `instance.id()`。

另声明一个边界：同 preset 的多个实例会产出同名候选，`resolve` 取链序首个；需要区分应给它们不同的 preset 行，而不是指望候选视图替装配层裁决。

---

## 三、harness 覆盖面（15 条）

文件：`crates/oriera-infer/tests/route_double_run.rs`（429 行）。**不发起任何模型调用**——registry 里放离线 `RuleProvider`，只验证"决策出的 id 能取到实例"，不调 `complete`。

链路：

```
health_report ──► snapshot_candidates ──► resolve ──► registry ──► observe
  (实例 id)         (preset id 翻译)       (纯决策)    (取实例)     (台账)
```

按六组断言组织：

1. **双跑逐位一致**（3 条）：健康链、降级链、多槽位决策流。同快照跑两遍，`ResolvedPick` 与整本 `RouteLedger` 逐条相等。`waited_us` 喂固定值——比的是决策与台账结构，不是墙钟。
2. **顺序无关**（1 条）：候选快照乱序不改变选链结果，且断言两份快照的 id 序确实不同（否则测试是空的）。
3. **fail closed 贯通**（4 条）：全熔断、未声明实例、未知槽位、空链，一律 `None` 且不落账。
4. **id 空间回归**（2 条）：`rule-v1 → rule` 翻译后 chain 真能匹配上；registry 用 preset id、台账记实例 id。这两条是 harness 存在的理由。
5. **HalfOpen 策略可观测差异**（1 条）：`ProbeAsAvailable` 选中半开的云端行，`TreatAsOpen` 降级到 rule——把 P1-2 两个策略的分叉钉成可回归的行为差异。
6. **台账与 caps 边界**（4 条）：越界 `chain_index` 不落账、空健康快照、caps 来自 preset 表、能力闸拒绝未登记 tools 的行。

### 顺带确认的一个真实约束

夹具假设了 `rule` 行支持工具调用，5 条测试失败。查 preset 表：`rule` 的 caps 是 `ModelCaps::UNREGISTERED`（`tools: false`）。**是夹具假设错了，不是代码错**——离线规则后端确实不支持工具调用，能力闸拒得对。

改成两个槽位：`npc_dialog` 不要求 tools（rule 可兜底），`tool_agent` 显式要求 tools（rule 被正确拒掉）。并补一条断言把这个约束钉住：同一份候选，在 `tool_agent` 上整链耗尽，在 `npc_dialog` 上 rule 兜住。符合诚实深度——不假装它有这个能力。

---

## 四、验收

| 项 | 结果 |
|---|---|
| `oriera-infer` 默认 feature | 98 passed（单测）+ **15 passed（harness）** + 1 doctest |
| `cargo test --workspace` | **627 passed / 0 failed / 4 ignored**（前值 612，+15） |
| `oriera-agent --features json,rigid3d,cam,spec,urdf` | 103 passed + 1 + 3 doctest，无漂移 |
| JS 镜像 | 130 pass / 0 fail |
| clippy（全 feature，`-D warnings`） | 零告警 |
| `cargo fmt --all -- --check` | 干净 |

**干净复现**：fresh worktree 从 `0387718` 顺序 apply 001–015，独立 `CARGO_TARGET_DIR` 复跑——627 passed / 0 failed，clippy 零告警。补丁 `git apply --check` 通过。

补丁规模：516 行 / 21487 字节，2 文件 +459/-13。

---

## 五、对 014 的取代关系

补丁 **014 含缺陷版本**（`provider_id` 用实例 id），其 CDN 链接已发出。

- 014 **不作废**，仍是有效历史步骤；
- 015 修正它，两者顺序 apply 后即为正确状态；
- 若只取单个补丁使用，**必须 014 + 015 一起**，单取 014 会引入静默断链。

文档 `11-candidate-snapshot-step-b.md` 描述的是缺陷版本口径，`../plans/infer-wrapper-wiring.md` 步 B 小节同；两处已在本次一并更正。

---

## 六、边界与未做的事

- **不碰包装层**。步 C（`FallbackProvider` 接 `resolve`）仍需跨层审批（`GOVERNANCE.md` 多数同意 + RFC 0003 公开评论 ≥7 天），本刀只做纯测试设施与步 B 的自身修正，不触发该流程。
- **不改任何既有对外行为**。`snapshot_candidates` 是步 B 新增的、尚未被任何生产路径调用的函数，修它不影响现有 `FallbackProvider` 的自遍历逻辑。
- **S2 死线仍然有效**：`resolve` 与 v0 路由类型未接线，不得进 release note 或对外材料。本刀不构成"已接线"。
- **未跑真实远程后端**。harness 全程离线，不给 SLA 承诺。

---

## 七、下一步

双跑 harness 这个硬前置**已解阻**。步 C 现在缺的只有跨层审批，不缺技术前提。

审批材料建议附三样：本刀的 15 条 harness 结果、`../reviews/v1b2-chain-decision-review.md` 的 S2/S3 挂账、以及性能矩阵里 router `resolve` 的 ns 级读数（首项即中 ~29 ns，每降级一跳 +15–20 ns，不构成帧压力）。

仍待拍板、本刀未替装配层决定的：P1-1（Batch 重试次数）、P1-2 的策略选择（已做成运行时参数，默认 `ProbeAsAvailable`）、S3（`candidate_ok` 是否降私有）、Realtime deadline 下限（基准建议 ≥1 ms）。
