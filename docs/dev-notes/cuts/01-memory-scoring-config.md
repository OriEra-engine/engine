# 第 1 刀：记忆检索系数可配置化 + 打分可审计

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

> 执行时间：2026-11-23　｜　仓库基线：`OriEra-engine/engine` @ `0387718`（v0.13.0）
> 性质：真改代码 + 真跑测试，不是建议稿。补丁：`patches/001-memory-scoring-config.patch`

## 一、为什么先动这里

学术视角的结论是"结构—算法分离本身不致病，病在算法假设住在结构层"。具体到代码：`crates/oriera-agent/src/memory.rs` 里那组打分系数（关键词×1.6、重要性×0.45、近因×2.0、主体加分 1.2、语义×2.0、门槛 0.8）是以字面常量形式写在 `retrieve` / `retrieve_embedded` 内部的——想验证某个系数到底值不值得留，只能改代码重编，既不可消融也不可审计。

这一刀同时兑现两件事：学术侧的"把假设从结构层搬到可配置层"，技术侧的 R3（记忆后端解耦的第一步）。之所以选它做第一刀，是因为它风险最低、收益最实、且不触碰任何既有语义。

## 二、具体改动

**1. 新增 `MemoryScoring`（6 个系数，`Default` = 历史公式原值）**

| 字段 | 默认 | 含义 |
|---|---|---|
| `keyword_weight` | 1.6 | 关键词 bigram/整词命中的权重 |
| `importance_weight` | 0.45 | 情节重要性（1–5）权重 |
| `recency_weight` | 2.0 | 近因项 `1/(1+age)` 权重 |
| `about_bonus` | 1.2 | `episode.about == query.about` 的固定加分 |
| `semantic_weight` | 2.0 | 语义余弦项权重（仅嵌入档；离线档恒喂 0） |
| `min_score` | 0.8 | 检索门槛，低于等于此值丢弃 |

`sanitized()` 把非有限值（NaN/Inf）回落到默认——配置是调用方输入，一个 NaN 不该毒化整个 store 的排序。

**2. `MemoryStore` 持有 `scoring`**
- `scoring()` 读、`set_scoring()` 换（内部 sanitize）
- `with_capacity_and_scoring()` 一次性配好，供未来的远程/图后端在装配期注入

**3. 打分拆成可审计的迹**
- 新增 `RecallTrace`：`keyword_hits` / `keyword_term` / `importance_term` / `recency_term` / `about_bonus` / `semantic_term` / `total`
- 新增 `ScoredEpisode { episode, trace }`
- 新增 `retrieve_traced()` 与 `retrieve_embedded_traced()`
- 原 `retrieve()` / `retrieve_embedded()` 改为委托给 traced 版本再剥掉迹——两条路径不存在漂移

这条迹是 L1 台账的最小落点：评测报告要能复算名次，而不是只信任一个总数。

**4. 浏览器镜像同步**
`examples/11-ai-cafe/engine/kernel.js` 的 `MemoryStore` 增加第三个可选参数 `scoring`，默认值与 Rust 逐项相同，非有限值同样回落。老调用 `new MemoryStore(key, cap)` 语义不变。

## 三、刻意没动的东西（风险控制）

- **默认行为逐位不变**：`MemoryScoring::default()` 就是原来那组常量，公式顺序也保持 `keyword + importance + recency + about + semantic`。
- **JSON 快照不动**：`to_json` 输出保持逐字节稳定，所以 `json_round_trip_preserves_retrieval`、`to_json_is_stable_regardless_of_agent_insertion_order`、`import_honors_the_sliding_window` 三条契约测试原样通过。scoring 是运行时配置，不写进快照（要持久化是下一版的事，需新 version 字段）。
- **金样不动**：`tests/goal_behavior_golden.rs` 仍绿。
- 顺手修了一处文档错位：原本挂在 `offline_score` 上的 bigram 说明，移回 `bigrams()` 本身。

## 四、验收结果（实跑）

命令与环境：`cargo 1.99.0`，stable-x86_64-unknown-linux-gnu。

| 检查项 | 命令 | 结果 |
|---|---|---|
| 单元 + 集成 | `cargo test -p oriera-agent --features json` | **85 passed**（原 80，新增 5） |
| 金样 | 同上，`goal_behavior_golden` | 1 passed |
| 文档测试 | 同上，Doc-tests | 3 passed |
| 完整具身特征 | `cargo test -p oriera-agent --features rigid3d,cam,spec,urdf` | 85 + 1 + 3 全绿 |
| 严格 lint | `cargo clippy -p oriera-agent --features json --all-targets -- -D warnings` | 通过，零告警 |
| 浏览器镜像 | `node --test examples/11-ai-cafe/engine/kernel.test.mjs` | **43 pass / 0 fail** |

新增的 5 条测试本身就是这刀的验收判据：

1. `scoring_default_reproduces_the_historic_formula` — 默认逐项等于历史常量，默认 store 与 `with_capacity` 都带上它。
2. `traced_terms_reconstruct_the_ranked_score` — 分项之和 == 总分（1e-5 内），离线档 `semantic_term` 恒 0，且 plain 档与 traced 档结果完全一致。
3. `zeroing_one_coefficient_is_an_ablation_not_a_rewrite` — 关键词系数置零后排序确实反转（旧但词密 → 近但因空），置零后 `keyword_term` 恒等于 0（无隐性残留），恢复默认后排序逐项回到基线。这条就是"单变量置零"的最小可判实验。
4. `non_finite_and_custom_gates_are_sanitized_not_poisoning` — NaN/Inf 回落默认；抬高门槛得到空结果而不是 panic。
5. `zero_semantic_weight_equals_the_offline_tier` — 语义权重置零后，嵌入档与离线档逐字节等价。

## 五、怎么落地

```bash
cd <engine 仓库>
git checkout -b feat/memory-scoring-config
git apply /path/to/001-memory-scoring-config.patch
cargo test -p oriera-agent --features json
cargo clippy -p oriera-agent --features json --all-targets -- -D warnings
```

补丁基于 `0387718` 生成，若目标分支已前移，`git apply -3` 走三方合并更快。

## 六、下一刀的顺序（按依赖，不按喜好）

1. **`MemoryBackend` trait 抽出**（承接 R3）：把 `record` / `retrieve` / `retrieve_traced` / `consolidate` / `prune` 收进 trait，`MemoryStore` 成为默认本地实现。这一步之后，远程/图记忆后端才能真正挂上来，否则接进来就是第二个黑箱。
2. **召回台账接进评测报告**：把 `RecallTrace` 随 episode 一起落进评测产物，让"这条记忆为什么被召回"可复算、可对照——这是 L1 台账从"有结构"到"进 CI"的一步。
3. **ModelRouter + SLA 分级**：L7 目前只有单 provider 边界 + 降级链，多模型按成本/时延/能力路由还没有。这一刀要动接口，建议等第 1 刀合入、`MemoryBackend` 稳定后再开。

前两刀都不碰物理与具身金样，风险可控；第三刀会触碰推理边界，需要单独的 RFC。
