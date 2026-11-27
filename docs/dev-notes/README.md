# dev-notes：2026-11 的工程工作记录

这个目录是 2026-11-22 到 11-25 之间一批改动的**工作记录**：每一刀为什么做、怎么验证的、哪里没达标。它们是当时写下的快照，不随后续代码更新；现状以对应的 PR 和 `main` 为准。

## 先读这一条：读数口径

除特别注明外，这些文档里的毫秒数全部来自一台**没有 GPU 的开发机**（mesa llvmpipe 软件光栅化，`device_type=Cpu`），或纯 CPU 段的 criterion 基准。

- **它们不是 GPU 帧时间**，也不能外推到真卡，只用于同一台机器上的前后对比。
- 真卡上的复核结论见 RFC 0004（PR #207）的"真卡复核"一节。按仓库的读数披露口径，真卡的绝对帧时间不公开，只公开相对预算的位置。
- 文中写"真卡未测""需要真卡重跑"的地方，就是当时没有数据。

## 名词

| 词 | 意思 |
|---|---|
| 刀 | 一次可以独立评审、独立回滚的改动。编号 03 和 15–19 是空的（当时合并进了相邻的刀，之后编号改为与补丁号一致） |
| 补丁 0NN | 当时的交付方式：一个 `git diff` 文件，按序 `git apply`。这些补丁后来都变成了提交，对应关系见下表 |
| 拍板 | 评审之后定下来的决定。"待拍板"就是当时还没定 |
| 本机 / 开发机 | 跑基准的那台无 GPU 机器 |
| 路线二 | "画面简单、行为难"的一档场景方向：低多边形场景、NavMesh 寻路、动画状态机，以及窗口呈现 |
| S1 / S2…、P1 | 评审条目的编号，各评审文档开头有定义 |
| RFC 0003 / 0004 | 两份设计文档：0003 推理槽位路由与 SLA 分级（PR #212）、0004 渲染持久路径（PR #207）。合入 main 后在 `docs/rfcs/` |

## 收录了什么

**没有收录的**：对内审批材料（步 C 的审批已由 issue #216 取代）、原来的补丁总索引（本页替代），以及内部的经营分析与方向讨论。

## 索引

"现在的位置"是 2026-11-27 的状态。

### 刀文档

| 文档 | 补丁 | 现在的位置 |
|---|---|---|
| [第 1 刀：记忆检索系数可配置化 + 打分可审计](cuts/01-memory-scoring-config.md) | 001 | 草稿 PR #210 |
| [第 2、3 刀：记忆后端抽象 + 召回台账进报告](cuts/02-memory-backend-and-recall-ledger.md) | 002 | 草稿 PR #210 |
| [第 4 刀：RFC 0003 v0 — 槽位路由表 + SLA 分级 + 决策台账](cuts/04-router-sla-v0.md) | 003 | 草稿 PR #212 |
| [第 5 刀：RFC 0003 v1-a — 推理等待帧级计量（不选模型）](cuts/05-infer-wait-meter-v1a.md) | 004 | 草稿 PR #212 |
| [第 6 刀：RFC 0003 v1-b-1 — 异步等待计量与分桶（仍不选模型）](cuts/06-async-wait-meter-b1.md) | 005 | 草稿 PR #212 |
| [第 7 刀：RFC 0003 v1-b-2 决策层 — 纯函数选链 + 档位重试口径（未接调用路径）](cuts/07-router-resolve-b2.md) | 006 | 草稿 PR #212 |
| [第 8 刀：L8 生成资产闸 1 + 闸 2 — 网格合法性深校验 / 尺度面数上限（闸 3、闸 4 未动）](cuts/08-l8-mesh-validation-gates.md) | 007 | 草稿 PR #211 |
| [第 9 刀：L9 记忆分层第 1 步 — 两层后端骨架 + 巩固策略纯层（默认单层金样不动）](cuts/09-l9-layered-memory-skeleton.md) | 008 | 草稿 PR #213 |
| [第 10 刀 · #13 步 A：装配期 ProviderRegistry 纯结构（补丁 009）](cuts/10-provider-registry-step-a.md) | 009 | 草稿 PR #214 |
| [第 11 刀 · #13 步 B：候选快照纯函数（补丁 014）](cuts/11-candidate-snapshot-step-b.md) | 014 | 草稿 PR #214 |
| [第12刀 · #13 步 C 前置：双跑 harness + 一处真 bug 修正](cuts/12-double-run-harness-id-fix.md) | 015 | 草稿 PR #214 |
| [第13刀 · 长跑 soak + GLB 解码器属性/fuzz](cuts/13-soak-and-glb-fuzz.md) | 016 / 017 | 草稿 PR #214（016）/ #211（017） |
| [第14刀 · 整机帧成本（整机油耗）](cuts/14-world-frame-cost.md) | 018 | 草稿 PR #215 |
| [第20刀 · 记忆基准口径对齐与中文大窗口补测](cuts/20-memory-bench-large-window.md) | 020 | 草稿 PR #215 |
| [第21刀 · InferJobs 尾延迟分位数离线计量](cuts/21-infer-jobs-tail-quantiles.md) | 021 | 草稿 PR #215 |
| [第22刀 · opt-in 倒排索引记忆后端原型](cuts/22-inverted-index-memory-prototype.md) | 022 | 草稿 PR #215 |
| [第23刀 · 渲染层帧成本基准](cuts/23-gpu-frame-cost.md) | 023 | 草稿 PR #207 |
| [第 24 刀：持久渲染路径（RFC 0004 步 1a）](cuts/24-persistent-render-path.md) | 024 | 草稿 PR #207 |
| [第 25 刀：异步回读（RFC 0004 步 1b）](cuts/25-async-readback.md) | 025 | 草稿 PR #207 |
| [第 26 刀：蒙皮网格持久路径（RFC 0004 步 1c）](cuts/26-skinned-persistent-path.md) | 026 | 草稿 PR #207 |
| [第 27 刀：NavMesh 寻路（路线二第一块）](cuts/27-navmesh.md) | 027 | #208（已合入 main） |
| [第 28 刀：动画状态机（路线二第二块）](cuts/28-animation-state-machine.md) | 028 | #208（已合入 main） |

### 调研与落地方案

| 文档 | 补丁 | 现在的位置 |
|---|---|---|
| [L8 生成资产四道闸 — 现状调研与落地方案（待评审）](plans/l8-generated-asset-gates.md) | 007 | 草稿 PR #211 |
| [L9 记忆分层 — 现状调研与落地方案（待评审）](plans/l9-layered-memory.md) | 008 | 草稿 PR #213 |
| [#13 v1-b-2 包装层接线 — 现状调研与落地方案（待评审，未落码）](plans/infer-wrapper-wiring.md) | 009 / 014 / 015 | 草稿 PR #214 |

### 评审

| 文档 | 补丁 | 现在的位置 |
|---|---|---|
| [评审：L8 生成资产四道闸方案（落码前复核）](reviews/l8-gates-plan-review.md) | 007 | 草稿 PR #211 |
| [评审复审：v1-a（补丁 004）系统论对齐复核](reviews/v1a-systems-theory-check.md) | 004 | 草稿 PR #212 |
| [评审复核：v1-b-2 决策层（补丁 006）选链纯函数与双跑一致性](reviews/v1b2-chain-decision-review.md) | 006 | 草稿 PR #212 |

### 性能与定标

| 文档 | 补丁 | 现在的位置 |
|---|---|---|
| [性能测试矩阵与指标口径](perf/perf-matrix-and-metrics.md) | 010–013 | 草稿 PR #215 |
| [三候选场景规模定标 · 哪个对引擎最友好](perf/scenario-scale-calibration.md) | 019 | 草稿 PR #215 |
| [跑在主机上的任务清单（需要真 GPU）](perf/real-gpu-task-checklist.md) | 023–026 | 草稿 PR #207 |
