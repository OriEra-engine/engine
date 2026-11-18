# 性能与基准

> 本文档记录基准怎么跑、当前参考数据，以及各层预算的出处。
> 历史数据只追加不覆盖（旧版曾被自动脚本覆盖丢失，已删除该脚本）。

## 怎么跑

```bash
cargo test --workspace                     # 单元测试（含 ECS 10k 冒烟）
cargo bench -p oriera-ecs                  # criterion 基准：spawn_10k / query_10k
```

## 当前参考数据（2026-10-15，架构整理分支）

criterion `spawn_10k` / `query_10k`（Windows / stable Rust）：

| 基准 | 结果 |
|---|---|
| spawn_10k（spawn + 双组件插入） | 见 `cargo bench` 输出，量级：单帧毫秒级 |
| query_10k（Position×Velocity join 计数） | 同上 |

> 数字随机器差异很大，以自己跑出来的为准；这里只锁方法和量级。
> CI 不跑 bench（criterion 只在本地跑），防止慢机器污染记录。

## 预算出处（书 T3 §3.5 + 附录 C）

| 层 | 预算 | 来源 |
|---|---|---|
| 整帧（PC 60fps） | 16.6 ms | L1 manifest `frame_budget_us: 16666` |
| 整帧（AR 90fps） | 11.1 ms | 同上 `ar_glasses` |
| L5 渲染 | 8.0 ms p99 | `manifests/L5-render.yaml` |
| L6 物理步进 | 1.5 ms p99 | `manifests/L6-subsys.yaml`（AR 帧切片） |
| L7 端侧推理 | 1.0 ms p99 | `manifests/L7-infer.yaml` |

## 记录新数据

在下方按日期追加一节，格式：

```markdown
## YYYY-MM-DD <机器/工具链>

- spawn_10k: <时间>
- query_10k: <时间>
```

## 算子层容量参考与候选清单（2026-11-13，战略路线图 A6 前置）

> 数据来源：昇腾 Triton 算子大赛实测沉淀（`_triton_work/variants/` 九变体台账；官方 eval 环境基线 910B4 · CANN 9.2.0-beta.2 · triton-ascend 3.6.0）。
> 纪律（实测说话）：任何内核改动必须同时满足——(a) msvc-local bench 前后对比（本文件追加记录）(b) 渲染/物理/AI 金样全绿 (c) 有 HiDevLab A3 实验数据背书。纸面推算不作为采纳依据。

### NPU 水位速查（容量模型参考，勿与 x86/WebGL 数字直比）

| 指标 | 实测值 | 适用条件 |
|---|---|---|
| 纯 Vector kernel 稳态带宽 | 0.31–0.40 TB/s（距 HBM 峰值 ~2.5x） | 910B4 · bf16 单趟流式读 · 无软流水（`variants/opt-pipeline/kernel.py:11`） |
| tiny shape 设备时延 | 8.5 µs（仅 16 program，占用不足） | grid=(B·L,) 单行一 program（`variants/opt-splitv`） |
| 目标并行度 | T×SLICES ≈ 512（SLICES ≤ 32） | 占满数十向量核的选片经验值（`opt-splitv/run`） |
| UB 预算 | 256KB：BLOCK 16384+双缓冲=192KB 可行；32768=320KB 不可行 | bf16/fp32/exb 布局实算（`opt-pipeline/kernel.py:34-36`） |

### 五个可迁移优化模式 → 引擎内核映射

| 模式 | 大赛出处 | 引擎候选内核 | 状态 |
|---|---|---|---|
| 软件流水（stage 双缓冲） | opt-pipeline | oriera-splat GPU 排序 / oriera-render 蒙皮批处理（访存受限型） | 候选，待 A3 数据 |
| 切片 tile + 配置分档 prune | opt-tuning / opt-multirow | 蒙皮顶点批 tile 尺寸、泼溅 tile 调度 | 候选（multirow 2D 大 tile 已被大赛证伪，勿走） |
| 两趟 vs 单趟（L2 驻留判据） | opt-two-pass | splat 前向 alpha-blending 的 pass 结构（V≤8k 且 L2 放得下才两趟划算） | 候选，判据先写死在注释 |
| 数值纪律（clamp±20 代 max-归约树 + 免 rescale） | opt-numerical | L7 推理侧 softmax 类、深度归一化；渲染 PBR NaN 兜底 | 候选，误差预算证明先行（大赛做法：尾块 ≤1.7e-5 ≪ atol 2e-3，250× 裕度） |
| 分区归约（P∈{1,2,4,8} 幺半群，P=1 精确退化保底） | opt-xdiscipline | 泼溅/蒙皮的归约 ILP 化 | 候选，P=1 回退锚点必须保留 |

> 并行度水位 ≈512 是 NPU 向量核经验值；GPU（wgpu）侧对应方法论是 workgroup 尺寸 × SM 占用率标定——迁移的是"目标占用率标定法"，不是数字本身。
> HiDevLab A3 平台镜像栈（triton-ascend 3.2.2 · CANN 9.1.0）与大赛基线（3.6.0 · 9.2.0-beta.2）存在版本差，A3 实验结论须标注版本口径，勿混写为 910B4 复测（见 `out/hidevlab/接入手册v0.md` §6）。
