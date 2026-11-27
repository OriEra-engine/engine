# 第 2、3 刀：记忆后端抽象 + 召回台账进报告

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

接第 1 刀（打分系数可配置化）。这两刀解决的是同一个问题的两个面：**系数搬出来之后，谁来用、怎么证明它被用了**。

- 第 2 刀：把 `MemoryStore` 从"本地结构"改成"门面 + 可插拔后端"。系数要能被外部策略接管，前提是后端可以换——否则系数只是换了个地方写死。
- 第 3 刀：把每次召回决策记成证据（query、当时生效的系数、k、命中分项），并能挂进评测报告。这是"可解释性分层承诺"里的 L1 台账，成本近零。

补丁：`patches/002-memory-backend-and-recall-ledger.patch`（1120 行，基于 `0387718`，**叠在 001 之上**）。

---

## 一、第 2 刀做了什么

### 门面不动，后端可换

改之前所有调用点都是 `world.resource::<MemoryStore>()` 然后 `.record()` / `.retrieve()` / `.len_total()` / `.agents()`——`cognition.rs`、`goal.rs`、`examples/cafe.rs`、`studio_e2e.rs` 全在用。**调用点一个字没改**，这是这刀的硬约束：改接口就等于让所有垂直版重写。

新增的东西：

| 类型 | 作用 |
|---|---|
| `RetrievalRequest<'a>` | 一次召回请求：agent / about / query / now / k / scoring。scoring 随请求下发，远程后端可以忽略，本地后端据此打分 |
| `MemoryBackend` trait | `record / retrieve / retrieve_embedded / prune / episodes / agents / len_total / is_empty / consolidate / clone_backend`。`retrieve_embedded` 默认退回离线档，`consolidate` 默认空实现——**新后端只实现最小的 record+retrieve 就能跑** |
| `LocalMemoryBackend` | 原 `HashMap<String, Vec<Episode>>` 那套逻辑原样搬进来，行为不变；`names` 单独维护首个记录顺序，让 `agents()` 仍是零分配的 `&[String]` 借用迭代 |
| `MemoryStore` | 变成 `{backend: Box<dyn MemoryBackend>, capacity, scoring}` 的门面；新增 `with_backend()` 与 `backend()` |

打分逻辑抽成自由函数 `score_of(e, hits, now, about, semantic, scoring)`，本地后端调它，远程后端不用它。

### 测试怎么证明"真的能换"

`a_remote_backend_swaps_in_without_touching_any_call_site` 里塞了一个 `CannedBackend` 假远程：

- 写入路由到后端（门面不自己存）；
- 召回由后端作答（返回文本前缀 `remote:`）；
- 后端没有向量能力 → 默认实现自动降级到离线档，仍返回 1 条，不 panic；
- `to_json()` 里既没有 `remote:` 也没有后端私有的 `aria`——**读不出来的东西绝不编造**，`to_json` 遇到 `episodes()==None` 直接 `continue`；
- clone 之后彼此独立（len 2 vs 1）；
- 门面仍然如实报告自己的 `scoring()`。

另一条 `agent_names_follow_first_record_order` 钉死 `agents()` 的首次出现顺序与零分配借用语义。

> 没做的事：没有引入异步后端、没有改 `prune` 的淘汰策略、没有把 `capacity` 下沉到后端（容量仍由门面持有并显式 `prune`）。远程后端如果需要自己的淘汰策略，`prune` 就是它的入口，接口已经留好。

---

## 二、第 3 刀做了什么

### 数据形态

```rust
RecallEvent { seq, turn, agent, about, query, k, returned, scoring, top }
RecallHit   { about, text, total, trace }          // trace = 6 项分项
```

`push` 自己发号（`seq` 单调、从 1 起），调用方无法伪造顺序。容量 `RECALL_LEDGER_CAPACITY = 256`，超了丢最旧的，**seq 不重置**——这样报告里能看出"这段窗口是第 11~266 次召回"。

### 三个必须说清的取舍

1. **`RecallLedger` 是观察资源，不是世界状态。**
   `next_speech` 拿的是 `&World`（不可变），所以台账用了内部可变性（`Mutex` + 抗 poison 的 `lock()`）。它不参与 `state_hash`，不进 `to_json` 的记忆快照，不进任何指纹。这条有回归测试钉着：`recall_ledger_observes_without_changing_the_world` 跑两个世界各 10 tick，一个注册台账一个不注册，断言 `DialogueLog.turns` 逐条相等、`MemoryStore.len_total()` 相等、未注册侧 `resource::<RecallLedger>()` 是 `None`。

2. **不注册 = 零开销。**
   `world.resource::<RecallLedger>()` 为 `None` 时直接跳过，连事件都不构造。默认路径（不含 `attach_recall` 的报告）字节完全不变。

3. **多槽并发下事件落库顺序取决于取锁顺序。**
   这跟 `DialogueLog` 的落地序是同一类不确定性。要让报告逐位可复现，需要配合 `DeterminismLevel::Recorded`。这一点写进了类型文档，不靠口头约定。

### 进评测报告

`HeadlessRunAccount` 加了一个**可选**证据槽：

```rust
#[cfg(feature = "json")]
#[serde(default, skip_serializing_if = "Option::is_none")]
pub recall: Option<serde_json::Value>,
```

`attach_recall(&ledger)` 把整本台账嵌进去。没挂的时候 `recall` 键根本不出现在 JSON 里——老报告、老往返测试一个字节都不动（`account_json_roundtrips_and_double_run_build_bits_match` 仍然全过）。

`cafe` 例子也接上了，跑完会打印：

```
召回台账 13 条（cognition 侧每次检索落一条：query/系数/k/命中分项）
```

13 条 = 40 回合里 next_speech 真正发起召回的次数。这个数字是当场跑出来的，不是估的。

---

## 三、验收（全部实测，可复跑）

| 项 | 结果 |
|---|---|
| `cargo test -p oriera-agent --features json` | **93 passed**（第 1 刀后 85 → 第 2 刀 87 → 第 3 刀 93），金样 1、doctest 3 全过 |
| 全具身特征 `json,rigid3d,cam,spec,urdf` | 93 passed，同上 |
| `--no-default-features`（json 关闭） | 77 passed——新槽位与测试都被 `cfg` 正确挡住 |
| `cargo clippy --all-targets -D warnings` | 零告警（两个特征组合都跑了） |
| `cargo fmt --check` | 干净 |
| 金样指纹测试 | 2 passed，`state_hash` 未变 |
| JS 镜像 `node --test examples/11-ai-cafe/engine/kernel.test.mjs` | 43 pass / 0 fail（kernel.js 本刀未再改） |
| 补丁可复现性 | 新建 worktree 从 `0387718` 依次 apply 001、002 → 93 passed |

补丁本身的验证方式（不是在工作区自测，而是干净树上重建）：

```bash
git worktree add --detach /tmp/verify HEAD
cd /tmp/verify
git apply patches/001-memory-scoring-config.patch
git apply patches/002-memory-backend-and-recall-ledger.patch
cargo test -p oriera-agent --features json
```

---

## 四、刻意没动的

- **默认路径数值不变**：`to_json` 字节稳定、金样指纹不动、`MemoryStore::new()` 行为与改动前逐位一致。
- **契约未动**：`oriera.infer.provider.v1`、记忆快照 `version=1` 都没碰。新东西全是新增类型 + 可选资源。
- **没接 `bench_account` 之外的报告产物**：`oriera-embodied/src/eval.rs` 的 `eval_report.json` 这轮没碰。具身评测协议（`course@v1` / `paper@v1`）有自己的 `spec_hash` 口径，把召回台账塞进去需要先定"跨会话召回算不算评测证据"，这是设计问题不是实现问题，见待决事项。

---

## 五、下一刀：为什么先出 RFC 而不是直接改代码

原计划第 4 刀是 `ModelRouter + SLA 分级`。真动手前撞上仓库自己的纪律：`CONTRIBUTING.md` 第 11 行——"改架构 / 加新层 / 动契约 → 先在 Discussions 开 RFC，7 天评论期后再动手"；`GOVERNANCE.md` 第 47 行还要求破坏性变更走 major。

路由要动 L7 接口面、并且要把推理时延接进 L2 的帧预算仲裁——按纪律必须先有 RFC。所以这轮交付的是 **RFC 0003 草案**，形态直接可投 Discussions，含提案接口、契约影响、可测断言、替代方案与否决理由。

代码等评论期结束、拍板后再落。理由：三刀已经能跑、能验、能进报告；第四刀如果先写代码再补 RFC，评审时返工的是接口而不是文档。

---

## 六、当时的待决项（三件）

1. **召回台账是否进具身评测报告**（`eval_report.json`）。建议：先不进。理由——召回是对话/决策侧证据，具身评测的证据链是 `spec_hash + 协议 id + 参数哈希`，混进来会让"同一 spec 同一协议可复现"这条变模糊。要进就单开一个 `recall@v1` 证据段，别塞进现有协议。
2. **RFC 0003 是否直接进 Discussions**。草案已是可粘贴的 markdown 正文，差的只是发布动作。
3. **第 5 刀候选**（按技术侧结论的优先级）：L2 `BudgetAllocator` 吃推理时延 > L8 生成资产四道闸 > L9 记忆分层（现在单层 Vec + 容量 60）。倾向先做 BudgetAllocator 的接线，因为它和 RFC 0003 是同一件事的两半，隔开做会返工。
