# 第22刀 · opt-in 倒排索引记忆后端原型

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

补丁 **022**。基于 `0387718`（v0.13.0），顺序叠在 001–021 之后。trait 内 opt-in 内部实现，默认装配一行未动。

---

## 一、动机

记忆检索是全量扫描：每条目现场构造 bigram 集合，查询 gram 逐条 `contains`。020 实测中文夹具 1.28–1.45 µs/条，20000 条单次检索约 25.8 ms。这一刀在既有 `MemoryBackend` trait 内落一个本地倒排索引后端原型：gram → posting list 剪枝候选，候选再按既有 `score_of` 打分。只经 `with_backend` 注入，门面签名与 cognition/goal 调用点零改动。

这是原型先行，默认装配切换不在本轮；换默认后端是架构动作，另行 RFC。

---

## 二、改动面

| 文件 | 改动 |
|---|---|
| `crates/oriera-agent/src/memory.rs` | 新增 `InvertedMemoryBackend`（per-agent 条目 Vec + gram→posting 索引）；7 条等价/契约测试 |
| `crates/oriera-agent/src/lib.rs` | 导出 `InvertedMemoryBackend` |
| `crates/oriera-agent/benches/memory.rs` | 新增 `bench_index` 对照档，`-- --nocapture indexbench` 触发 |

硬约束核对：

- 未扩 trait 方法（仍为既有 9 方法 + 默认降级）；
- 未改门面签名，未改 cognition / goal 调用点；
- 默认 `LocalMemoryBackend` 与 v1 金样逐位不变；
- 未接远程；
- 属未接线新类型，按 S2 同纪律挂账，不进 release note 与对外材料。

---

## 三、正确性硬门槛

固定夹具族下，倒排后端与 `LocalMemoryBackend` 的 top-k 结果逐条一致（含并列次序），7 条测试在干净复现树上全过：

| 测试 | 覆盖场景 |
|---|---|
| `inverted_matches_local_chinese_and_english` | 中文 1/3 命中、英文短词、零命中查询 |
| `inverted_matches_local_sparse_hits` | 1/10 稀疏命中 |
| `inverted_matches_local_when_fewer_than_k` | 不足 k 条边界 |
| `inverted_preserves_record_order_on_tied_scores` | 并列分数，tie-break 为记录次序，不被 posting 并集次序打乱 |
| `inverted_falls_back_to_full_scan_when_non_hitter_could_displace` | 高重要性零命中条目可能压过弱命中条目时，显式回退全量扫描 |
| `inverted_json_round_trip_matches_local` | JSON 导出字节一致、往返后召回一致；`episodes()` 切片一致；克隆彼此独立；无向量时可见降级到离线档 |
| `inverted_keeps_index_consistent_after_prune` | FIFO 裁剪后索引重建，召回仍与本地一致 |

无损剪枝的判定：候选打分后，kth 保留分数与非候选上界（`5×importance_weight + recency_weight + about_bonus = 5.45`）比较。零命中条目仍可靠 importance / recency / about_bonus 过闸，一旦可能挤掉边界候选，**回退全量扫描**，不悄悄丢结果。

---

## 四、对照 bench 实测

跑法：release 构建，独立 `CARGO_TARGET_DIR`，中文夹具，k=10，60 样本。

```sh
cargo bench -p oriera-agent --bench memory -- --nocapture indexbench
```

| n | 全量扫描 p50/p99 µs | 倒排索引 p50/p99 µs | p50 倍数 |
|---|---|---|---|
| 1000 | 1295.4 / 1488.4 | 607.5 / 3001.1 | 2.13× |
| 5000 | 6376.9 / 6937.5 | 3040.0 / 3242.5 | 2.10× |
| 20000 | 28691.0 / 29862.9 | 13962.3 / 15094.1 | 2.05× |

**5× 目标未达标，如实记录。** 实测 p50 改善 2.05–2.13×，不到推断目标的 5×。原因分析：

1. 无损剪枝边界保守：kth 候选低于非候选上界时回退全扫描，部分请求没走索引路径；
2. 候选条目仍要现场构造条目 bigram 集合并做 `contains`，单候选成本与全扫描单条同构，省的只是被剪掉条目的 gram 构造；
3. 查询 7 gram 的并集跨条目复用，候选集大于严格命中集（1/3）；
4. posting 遍历与 HashSet 去重有常数开销，小窗口被抵消。

1000 档索引 p99（3001 µs）反超扫描（1488 µs），回退路径叠加索引冷启动是直接原因。

---

## 五、诚实边界

- 索引在 FIFO prune 时整段重建 posting，没有墓碑记账；高频裁剪下的写成本未优化。
- `retrieve_embedded` 走 trait 默认降级到离线档，没有 cosine 混合。
- 候选上界按默认系数算死；换用自定义 `MemoryScoring` 时上界随系数走，回退判定同步收窄/放宽。
- 原型未接默认装配，没有生产消费者；挂账项，不构成"已上线"。
- 全部 bench 数字为内部基线，未标定硬件前不对外。

---

## 六、验收

| 项 | 结果 |
|---|---|
| `cargo test --workspace` | 656 passed / 0 failed / 4 ignored（基线 649 + 本刀新增 7） |
| clippy 全 feature `-D warnings` | 零告警 |
| 默认 feature 新增 bench | 零告警 |
| `cargo fmt --all -- --check` | 干净 |
| JS 镜像 | 130 pass / 0 fail |
| 干净复现 | 独立 worktree 从 `0387718` 顺序 apply 001–022，独立 `CARGO_TARGET_DIR`，测试/clippy/fmt/JS 全绿 |

补丁规模：3 文件，+491/-8。

## 七、依赖

叠在 001–021 之后；依赖 002 的 `MemoryBackend` trait 与门面 `with_backend`、008 的后端装配先例。与 014/015 推理侧补丁无耦合。
