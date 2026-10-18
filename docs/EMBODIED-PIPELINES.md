# 具身仿真管线 — 开源生态调研 × OriEra 接入地图

> 一句话：OriEra 不重造物理引擎，也不重造数据集格式——它把自己做成
> **便宜、可复位、可回放的训练场**（world as benchmark），物理与数据
> 出口都对接开源标准。本文是 #80 的调研底稿与接入说明。

## 1. 生态调研（2026-10-19）

| 管线 | 是什么 | 许可证 | 对 OriEra 的意义 |
|---|---|---|---|
| **[Rapier](https://rapier.rs)** (rapier2d 0.36) | 纯 Rust 2D/3D 刚体引擎；[2026 路线图](https://dimforge.com/blog/2026/01/09/the-year-2025-in-dimforge)明确做机器人精度与跨平台确定性 | Apache-2.0 | **已接入**：L6 `rigid` feature，零 C 工具链 |
| **[LeRobotDataset v3.0](https://huggingface.co/docs/lerobot/en/lerobot-dataset-v3)** | HF 机器人学习数据标准：多集合一 parquet/MP4、episode 元数据入库、Hub 流式 | Apache-2.0 | **已对齐**：JSONL 列名随 v3（转换器见 §4） |
| [MuJoCo](https://github.com/google-deepmind/mujoco)（[rusty_mujoco](https://crates.io/crates/rusty_mujoco) / [mujoco-rs](https://github.com/davidhozic/mujoco-rs)） | 学术默认物理引擎；Rust 绑定 2026 活跃 | Apache-2.0 | L6 后续后端位：接触动力学/肌腱/执行器比 ball-world 深；C 工具链是当前门槛（Windows/mingw 环境） |
| [Genesis](https://github.com/Genesis-Embodied-AI/genesis-world) | 多物理统一平台（MPM/流体/软体）+ 光渲 | Apache 系 | 互通不替代：传感器仿真丰富、[实测比 ManiSkill 慢 3-10×](https://stoneztao.substack.com/p/the-new-hyped-genesis-simulator-is) |
| [Isaac Lab](https://github.com/isaac-sim/IsaacLab) / [ManiSkill](https://github.com/haosulab/ManiSkill) | GPU 并行大规模视觉训练 / manipulation 基准 | 各自开源 | 定位不同层：它们是平台，OriEra 是可嵌引擎内核；[四强对比](https://www.bifrost.ai/blog/isaac-lab-vs-mujoco-vs-maniskill-vs-genesis) |

**选型逻辑**：Rapier 先行因为它满足接入铁律——纯 Rust（本仓 wasm/CI/本机三环境零 C 依赖）、Apache-2.0、确定性叙事与 #66/#67 同构。MuJoCo 后端位留给需要接触动力学深度的场景。

## 2. 引擎接入：L6 `rigid` 后端

```bash
cargo test -p oriera-subsys --features rigid     # 14 测试
```

- `RigidBodySystem: System<World>`——与粒子积分器同一契约（Gravity/TimeScale/bounds/y 向下），**层配置**旋钮的首个实装（VERTICALS §0）
- 同步协议：新实体建 dynamic body + ball collider；死亡实体拆 body；外部速度写入（L7 `Nudge`）经「上 tick 写回值 ≠ 当前值」检测采纳；步进后写回并钳制 contained
- 诚实边界：与积分器**不逐位兼容**（碰撞/求解器是新增自由度）；垂直版二选一，不许回放中途换

## 3. 数据出口：L9 `embodied` 记录器（零新依赖）

- `EpisodeRecorder::observe(world, robot)`——机器人视角观察（自身 pos/vel + 最近目标相对状态，8 维）
- `record(Transition)` / `finish_episode(success, world_hash)`——计数 + **每集 `World::state_hash` 回执**（复现性收据）
- `to_jsonl()` / `info_json()`——见下表列对齐

| OriEra JSONL 列 | LeRobotDataset v3 对应 | 说明 |
|---|---|---|
| `episode_index` / `step_index` | 同名 | |
| `observation.state` / `next_observation.state` | 同名语义 | 8 维：robot(4) + nearest(4) |
| `action.continuous` | `action.*` 连续形态 | 二维推力 |
| `reward` / `done` | 同名 | |
| `meta/info.json` | `meta/info.json` | 引擎版本/后端/dt/**每集指纹** |

## 4. 采集 SOP 与社区位

```bash
cargo run -p oriera-agent --example embodied_collect --features rigid
# → out/embodied/{data/chunk-000.jsonl, meta/info.json}
# 内建断言：双跑逐位一致（指纹 + JSONL）
```

good-first-issue 候选：
1. JSONL→parquet 转换器（Python + pyarrow，LeRobot v3 直读）
2. MuJoCo 后端（feature `mujoco`，C 工具链方案评审先行）
3. 传感器观察扩展（碰撞事件/邻域扫描进 observation）
4. 视频轨（L5 帧导出→MP4，对齐 v3 的 video 列）

## 5. 定位声明

OriEra 提供的是「便宜、可复位、可回放」的**世界层**：确定性 tick（#66）、
世界指纹（#67）、逐实体动作（#69）、刚体后端与数据出口（本 issue #80）。
大规模 GPU 并行、可微物理、真实传感器噪声——生态已有答案，接进来，
不重造。
