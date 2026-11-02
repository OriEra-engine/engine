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
1. ~~JSONL→parquet 转换器~~ ✅ 3D 线已带 `scripts/jsonl_to_lerobot_v21.py`（pyarrow，v2.1 直读）
2. MuJoCo 后端（feature `mujoco`，C 工具链方案评审先行；wasm 参照面板先行）
3. 传感器观察扩展（碰撞事件/邻域扫描进 observation）
4. 视频轨（L5 帧导出→MP4，对齐 v2.1 的 video 列）
5. 3D 线：~~摩擦/接触抓取~~ ✅ 已落地（2026-10-30 四轮攻坚：双指运动学夹板×摩擦承载，探针最小复现定位——机制成立、集成难点在接近几何，夹板须悬垂于工具中心点之外）、MJCF 子集导入、腕滚转第 5 轴

## 6. 3D 具身训练台（PLAN-embodied-sim，2026-10-29）

> 一句话：L6 新增 `rigid3d` 后端（rapier3d multibody 关节臂）× L9
> `embodied_env`（Gymnasium 签名镜像 + PickPlace3D）× `lerobot`
> 模块（**LeRobotDataset v2.1 全文件树导出**）——导出物直连
> lerobot-train / openpi / GR00T 三条训练管线。

```bash
cargo test -p oriera-subsys --features rigid3d        # 14 测试（含双跑逐位一致）
cargo run -p oriera-agent --example embodied_sim_collect --features rigid3d
# → out/embodied-sim/lerobot-v21/（meta 全套 + 每集 sim_fingerprint 回执）
python scripts/jsonl_to_lerobot_v21.py out/embodied-sim/lerobot-v21   # → parquet，lerobot 直读
```

选型结论（详见 growth/PLAN-embodied-sim.md）：**rapier3d multibody
为主**（纯 Rust/Apache-2.0/wasm32 一等公民/与 rigid2d 同契约）；
MuJoCo 只作 M1 wasm 参照面板；**Jolt 撤出路线**（无 reduced-coordinate
articulation，Rust 绑定青年期）。马达三坑（都踩过）：`motor_axes`
位掩码默认空（不启用=马达死的）、`AccelerationBased` 增益单位是角
加速度（对公斤级臂太软，须 `ForceBased`）、角度读出须用
`local_to_parent`（与马达同约定，体姿相除的几何读出符号反相）。

### 6.1 实验层（M0b，PLAN-embodied-general §四 M0b）

研究院主诉的一层：**版本化评测协议 × 出处 manifest × 批量入口 × 绝对指纹金标 × spec 库治理**。

- **协议**（`oriera_agent::eval`）：`course@v1`（12 集 + 90% 硬闸 = 旧行为）/
  `paper@v1`（≥100 集只记录）；三入口共用 `evaluate(protocol, run_episode)`，
  种子带（`seed0 + i + 65536·redraws`）、退化重抽、集数预算都在这一处；
  协议 id + 参数 hash 进 `eval_report.json` 与 `run_manifest.json`。
- **manifest**（`oriera_agent::manifest`）：每次 collect 写 `run_manifest.json`
  （resolved spec 全文、spec_hash、git SHA+dirty、Cargo.lock hash、rustc、rapier、
  features、profile、OS/arch、命令行、协议）；`oriera_sim.build_info()` 暴露
  `{version, features, profile}`，`scripts/oriera_gym.py::check_build()` 对照
  manifest 告警旧 .pyd（设 `ORIERA_EXPECTED_MANIFEST=<manifest>` 即在 import 时自动核）。
- **批量入口**：`--seed N`、`--gate hard|report`、`--verify-determinism full|sample|off`、
  `--runs-root runs/` → `runs/<spec_hash8>/scripted-p<proto8>/<seed>/` 断点续跑；
  `scripts/batch_joblib.py` / `scripts/batch_example.slurm` 是扫参模板。
- **指纹 scheme v2**（`oriera_subsys::rigid3d`）：`fingerprint()`（v1：状态 ⊕ joints[0] kp/kd ⊕ mass_scale）
  逐位保留供旧回执对账；`fingerprint_v2()` = 状态 ⊕ `robot_identity_hash`（N、逐关节
  kp/kd/armature/damping/limits/axis/anchor、逐 link 几何与质量、夹爪摘要含板系朝向）⊕ spec_hash；
  `state_hash_from_v1()` 把旧回执还原成纯状态哈希（mix 每轮是双射）。金标：
  `contracts/golden/embodied_fingerprints.v2.json`（12 seed 三口径，`--fingerprints FILE` 生成）。
- **GripperSpec 板系朝向**：`approach_axis` / `gap_axis`（挂载 link 系，缺省 = 旧 +X/+Z，
  序列化省略 ⇒ 金标 spec_hash 不变）；外部朝下夹爪挂 7 轴臂的抓取过闸见
  `crates/oriera-agent/tests/gripper_down_smoke.rs`。
- **spec 库**：`specs/official/`（金标，改字段 = 新文件）+ `specs/lab/`（`extends` 覆盖层）+
  `scripts/gen_spec_index.py` → `specs/INDEX.json`（同名异 hash 报错）。

### 6.2 任务族 @v2 × 新任务 × DLS 基线（M2，PLAN-embodied-general §四 M2 / §9.1 P-2/P-6/P-11/G-7/G-10）

判据修订**只走新 id**（P-2：`@v2` 新增，v1 条目只读保留——`--task pick_place` /
`pick_place@v1` 的金标 12 集 state hash 一位不动），注册表按 id 取：

| id | 判据 | 备注 |
|---|---|---|
| `reach@v1` / `pick_place@v1` / `push@v1` | 旧语义（区间 / 区间 / 区间+抓取奖励） | 冻结 |
| `pick_place@v2` | 在区 **且已松爪（夹爪指令 < `release_below`）且静止 `rest_steps` 步（质心速度 < `rest_speed`）** | 奖励塑形与 v1 逐字相同 |
| `push@v2` | 在区 **且全程 `holding == false`**（任一帧抓到即判败） | 奖励去掉抓取项、抓到扣分 |
| `reach@v2` | 同 v1 | 只为记录 `success_criteria=v2` 与 `terminate_on_success` 协议参数位 |
| `stack@v1` | A 相对 B 的 XZ < `stack_xz_tol`、中心高差 ≈ 两半高之和 ± `stack_y_tol`、松爪、静止 | 底座 B = `scene.props[1]`（fixed），判据读 B 的**实时**位置 |
| `bin_pick@v1` | @v2 pick 判据 | 料箱 = 四面 6 cm 重墙 prop（6000 kg/m³）近似；真 fixture / 自碰撞开关待 M3 / WP-A（TODO 在 spec `notes`） |
| `drawer@v1` | 沿 `pull_axis` 拉出 ≥ `pull_distance`（相对**出生位**）、松爪、静止 | 面板 = 可动盒体 prop；真 prismatic fixture 在 M3 |

- **阈值全进 `TaskParams`**（`terminate_on_success` / `rest_steps` / `rest_speed` / `release_below` /
  `stack_*` / `pull_*`）：写进 spec 即进 hash——**取默认值时由 `SceneSpec::spec_hash` 规范化掉**
  （夹爪板系朝向同款先例），金标 `c90869dcf6a04875` 不动。
- **双报**（`oriera_agent::eval::SuccessView`）：每集 `success_once`（任一帧满足）+ `success_at_end`
  （末帧满足）；`terminate_on_success=false` 时两者才会分开；v1 任务 `success` = once 保持旧语义。
  `eval_report.json` 带 `success_once_rate` / `success_at_end_rate` 与各自 Wilson 区间。
- **env 适配不动三变量枚举**：`TableTopEnv::set_task_id(id)` 以注册表项覆盖（`task_entry`），
  `TaskKind` 只做冻结调用方（python / wasm / collect）的别名层；obs 仍 21 维（每个任务的
  `obs_extra` 同一 9 维特权块，`ObsLayout` 从任务派生）。
- **DLS 数值 IK**（`oriera_agent::ik`）：对任意 N-DoF `RobotSpec` 做 FK + 阻尼最小二乘
  （数值雅可比；3 行位置 + 3 行「工具朝下」约束——4 轴恰好复现解析解约束，7 轴留 1 维
  零空间给阻尼消解；逐步夹限位）。`embodied_sim_baseline` 用它跑五预设（collect 的 4 轴解析
  IK 金标路一行没动）。
- **五预设 + 任务专属闸**（`eval::baseline_gate`）：金标桌面 ≥90%、Panda/叠块 ≥75%、料箱/抽屉
  ≥60%（新任务首版诚实门槛）；`--reference-out` 写 `results/reference/<spec_hash8>/<protocol>.json`，
  超参/种子钉死在 `baselines/<task>/<method>.yaml`（scripted-phases / dls-ik / ppo-sb3 / act-lerobot 占位）。
- **出图**（P-6）：`scripts/oriera_results.py aggregate runs/` → `results.parquet`（无 pandas 降级 CSV）；
  `scripts/plot_results.py` 画均值 + **分层 bootstrap 95% CI**（seed 层 × episode 层两级重抽）+ IQM。
- 字段中文速查表（G-7）：`crates/oriera-agent/specs/official/README.md`。

## 5. 定位声明

OriEra 提供的是「便宜、可复位、可回放」的**世界层**：确定性 tick（#66）、
世界指纹（#67）、逐实体动作（#69）、刚体后端与数据出口（本 issue #80）。
大规模 GPU 并行、可微物理、真实传感器噪声——生态已有答案，接进来，
不重造。
