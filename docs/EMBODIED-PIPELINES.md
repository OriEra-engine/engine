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

good-first-issue 候选（2026-11 复核：已完成的划线留档，P2 新池见 §7.2）：
1. ~~JSONL→parquet 转换器~~ ✅ 3D 线已带 `scripts/jsonl_to_lerobot_v21.py`（pyarrow，v2.1 直读）
2. MuJoCo 后端（feature `mujoco`，C 工具链方案评审先行；wasm 参照面板先行）
3. 传感器观察扩展（碰撞事件/邻域扫描进 observation）
4. ~~视频轨（L5 帧导出→MP4，对齐 v2.1 的 video 列）~~ ✅ M3 相机规格化已带 LeRobot 多视频轨（§6.4）
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

选型结论（详见 growth/PLAN-embodied-sim.md——仓外设计底稿，不在本 git 仓内；
结论要点已并入本文各节）：**rapier3d multibody
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
  `crates/oriera-embodied/tests/gripper_down_smoke.rs`。
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
| `bin_pick@v1` | @v2 pick 判据 | 料箱 = 四面 6 cm 重墙 prop（6000 kg/m³）近似；真 fixture / 自碰撞开关待 M3 / WP-A（TODO 在 spec `notes`） |（P2：料箱升真 fixture 双层壳，spec `1a7cfc40`）
| `drawer@v1` | 沿 `pull_axis` 拉出 ≥ `pull_distance`（相对**出生位**）、松爪、静止 | 面板 = 可动盒体 prop 近似（**P2 起为对照档案**：`drawer-4dof.json` ef18ac07，10/12） |
| `drawer@v2` | 判据表达式与 @v1 逐字相同（P-2 新 id 纪律——升级在场景不在判据） | **P2-W6B 真 prismatic 抽屉**：面板 = slide prop（`scene.props[0].slide`：prismatic 关节接出生位静态锚体，行程限位 + 滑轨阻尼，被夹爪摩擦拖动、撞不走）；柜体 = 真 fixtures（后壁+两侧板，顶留夹爪通道）；滑轨沿 Z（夹爪 gap 轴在 pick 姿态 = 世界 Z，挤压须∥滑轴——直列 X 轨在合爪过挤压下被求解器弹飞，见 rigid3d slide 测试探针记录）；预设 `drawer-4dof-v2.json` `f503b3a3`，基线 12/12（course@v1，双跑逐位一致，2026-11-05） |

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
  零空间给阻尼消解；逐步夹限位）。`embodied_sim_baseline` 用它跑六预设（M3 增 mobile-arm）（collect 的 4 轴解析
  IK 金标路一行没动）。
- **六预设 + 任务专属闸**（`eval::baseline_gate`）：金标桌面 ≥90%、Panda/叠块 ≥75%、料箱/抽屉
  ≥60%（新任务首版诚实门槛）；`--reference-out` 写 `results/reference/<spec_hash8>/<protocol>.json`，
  超参/种子钉死在 `baselines/<task>/<method>.yaml`（scripted-phases / dls-ik / ppo-sb3 / act-lerobot 占位）。
- **出图**（P-6）：`scripts/oriera_results.py aggregate runs/` → `results.parquet`（无 pandas 降级 CSV）；
  `scripts/plot_results.py` 画均值 + **分层 bootstrap 95% CI**（seed 层 × episode 层两级重抽）+ IQM。
- 字段中文速查表（G-7）：`crates/oriera-embodied/specs/official/README.md`。

### 6.3 npz 裸出口 × Studio 三库工作台（M2-C，PLAN-embodied-general §四 M2 / §9.1 U-8 / G-10 / 拍板项 3）

**npz 裸出口（U-8）**——不想装 LeRobot、只要 numpy 数组时用：

```bash
cargo run -p oriera-agent --example embodied_sim_collect --features rigid3d,cam,spec,urdf -- \
  --task pick_place --episodes 12 --export npz          # 与 --lerobot-v3 可并存，各写各的目录
# → out/embodied-sim/npz/episode_000000.npz …（+ meta/oriera_receipt.json、meta/oriera_spec.json、MANIFEST.jsonl）
```

```python
import json, numpy as np
z = np.load("out/embodied-sim/npz/episode_000003.npz")   # z.files == ['obs_3', 'act_3', 'meta.json']
meta = json.loads(z["meta.json"])                          # 非 .npy 成员 = 原始字节
obs, act = z[meta["obs_key"]], z[meta["act_key"]]          # float32 [N, obs_dim] / [N, act_dim]
```

- **同一份行，另一种编码**：数组就是写 `lerobot-v21/`（及 `lerobot-v30/`）的那批 `Row`，按
  little-endian f32 原样落盘（不经文本往返）；同一次采集里 npz 与两棵 LeRobot 树逐集逐值
  （float32 位模式）一致——验收用 numpy 对拍 12 集 4198 行全等。`meta.json` 带 `obs_names` /
  `action_names`、shape、`dataset_from/to_index`、`fps`、`robot_type`、`task` 与 modality 切片
  （不用 LeRobot 也能按 arm / gripper / ee / object / goal 切向量）。相机帧不进 npz（PNG/mp4 轨走 LeRobot 出口）。
- **零新依赖**：`.npz` 就是成员为 `.npy` 的 ZIP（`np.savez` 自己也写 ZIP_STORED）；
  `lerobot.rs` 手写 stored ZIP（local header / central directory / EOCD）+ NPY v1.0 头（64 字节对齐）
  + CRC-32 查表，固定 DOS 时间戳 ⇒ 重导字节一致。ZIP32 上限（4 GiB / 65535 项）超了报错不写坏文件
  （单集 ~60 KB，远够）。行宽与声明不符 = 契约错误，**一个文件都不写**。
- **没有 `--export parquet`**：Rust 侧没有零依赖的 parquet 写器，不为它引 crate（依赖纪律）——
  parquet 一律走现有转换器 `python scripts/jsonl_to_lerobot_v21.py <lerobot-v21 目录>`（pyarrow）；
  `--export parquet` 会给出这句人话并退出。
- 不加 `--export` 时零行为变化：默认路径产物（lerobot-v21 整棵树，含 PNG 帧与 MANIFEST）与改动前逐字节一致。

**Studio 三库工作台（G-10 / 拍板项 3）**——`/sim` 首屏 = 官方预设卡片墙（六预设，默认选中作业基线；mobile-arm 卡 M3 已登记待补 UI
`tabletop-4dof-pick`），卡片 = 名称 / 一句话 / 难度（`meta.difficulty`，金标文件不带该字段时回退「入门」）/
任务与机器人；点卡片 = 整份 spec 换装、**不重载 iframe**。

- **spec 服务管线**：Studio 仓 `node scripts/sync-engine-snapshot.mjs` 把 `crates/oriera-embodied/specs/`
  的 `official/*.json` + `INDEX.json` 逐字同步进 `engine-snapshot/specs/`，并生成 `resolved/<name>.json`
  （`extends` 展开版，`merge_overlay` 的 JS 逐条移植）。浏览器里的 wasm 没有文件系统，
  `resolve_extends` 读不到基 spec，所以**发给内核的是展开版**；展开不改身份——六预设 raw(extends) /
  resolved 在 CLI `--validate` 与浏览器 wasm 里 spec_hash 与 `INDEX.json` 四方一致。
- **换装协议**：沿用 M1 kernel 的 postMessage `{source:"oriera-sim", cmd:"spec", json, seed:1000}` →
  内核 `applySpec + buildSceneFromSpec + buildSliders + envReset` → 回 `{type:"state", note:"spec-applied", specHash}`。
  Studio 4 秒等不到回执 = 「画面内核待重建」兜底（选中态退回画面里真实那份 + CLI 命令）；
  录制中/有未导出的集时先确认（不同预设的数据不混进一个数据集）。
- **内核版本守卫**：同步器用快照里的 oriera-wasm 逐个加载 resolved spec、核对 spec_hash == INDEX，
  并核对内核自带默认实验 = 作业基线；不过就 WARN（重建 oriera-wasm 后重跑同步器）。
- **参数微调（G-3 升级）**：物体 / 目标位置数字框 → 生成一份新 spec（固定出生点）经同一 postMessage
  换装；**改了 = 新实验**，画面显示新 spec_hash，「复制这份 spec JSON」存文件后 CLI `--spec` 算出同一个号
  （与 §9.2 G-B1「自由滑块与指纹冲突」的化解：不做不计指纹的沙盒，每次微调都是有身份的实验）。
- 已知缺口：渲染器只摆 `props[0]`（`TableTopEnv` 只公开它的位姿，M1 已登记）——叠方块的底座 B、
  料箱的四面墙在物理和判据里都在、画面里暂时看不见；Studio 在这两个预设上如实提示。

### 6.4 相机规格化：针孔内参 × 畸变 × 外参（M3-A，PLAN-embodied-general §四 M3 / §9.1 U-5）

相机从「正交示意」升级为**标定模型**：`scene.cameras` 进 spec（`deny_unknown_fields`
继承 spec 规则；缺省空 = 旧 `--cam` 俯视逐字节不变，也不动 spec_hash——`cameras`
为空时整体跳过序列化）。

```json
"cameras": [{
  "name": "wrist",
  "kind": { "pinhole": {
    "width": 160, "height": 120,
    "fx": 90.0, "fy": 90.0, "cx": 80.0, "cy": 60.0,
    "dist": { "k1": 0.15, "k2": 0.02, "p1": 0.0, "p2": 0.001 },
    "pos": [-0.07, 0.075, 0.0], "look_at": [0.08, -0.04, 0.0], "up": [0.0, 0.0, -1.0],
    "channels": { "rgb": true, "depth": true },
    "attach": "ee"
  }}
}]
```

**语义（真机标定对接说明）**：

- **内参**：OpenCV 约定——`u = fx·X/Z + cx`（像素下标即连续坐标，主点 ≈ 图像中心），
  与 `cv2.calibrateCamera` 的相机矩阵逐字段对应；spec 内只存 k1/k2/p1/p2 四系数
  （Brown–Conrady：径向 + 切向），更高阶系数真机标定后再说（研究模式自行扩展）。
- **外参**：`pos`/`look_at`/`up` 是相机位姿（光轴 = look_at−pos，图像右 = up×光轴，
  图像下 = right×光轴）。`attach: "ee"` 时三者是**末端（TCP）系偏移**：原点 =
  `telemetry.ee_pos`、旋转 = `telemetry.ee_rot`（四元数 [x,y,z,w]），外参每 tick 重算
  ——腕相机随臂动，语义与真机「眼在手」标定一致（手眼方程的仿真版）。
- **深度**：相机系前方距离，**毫米 u16 灰度 PNG**，0 = 无回波（传感器惯例）；
  `depth.{name}` 轨与 `observation.images.{name}` 同名配对。
- **畸变应用点**：渲染先按理想针孔光栅化（z-buffer 三角光栅、逐面片 flat shading +
  头灯方向光、1/z 透视插值），畸变作为**纯后处理**（逆映射最近邻重采样）；`project()`
  用同一条正映射——所以「世界点 → project() → 渲染像素」自洽，量化门槛
  **重投影误差 < 2 px**（三点 × N 相机：夹爪 tip / 方块中心 / 目标心，颜色 + z-buffer
  深度双重核对）——U-5 标定板思路的仿真版，`embodied_cam_capture` 示例每步自检。
  畸变系数全 0 时后处理整段跳过（与无畸变路径逐字节一致）。
- **不做清单**：纹理级 wgpu 渲染明确不做（PLAN §八「RTX 渲染 不做」）——渲染是
  诚实的状态图像（cuboid/capsule/cylinder/disc 低面数解析体），不是照片仿真；真机
  侧拿这份数据当「已知外参内参的合成相机」用，不声称域外观零差距。

**腕相机示例**：`specs/lab/wrist-cam-demo.json`（extends 金标 + wrist 160×120
RGB+depth 带畸变 + top 320×240 RGB 俯拍）：

```bash
cargo run -p oriera-agent --example embodied_cam_capture --features rigid3d,cam,spec -- --spec crates/oriera-embodied/specs/lab/wrist-cam-demo.json --episodes 2
# → out/embodied-cam/lerobot-v21/ + lerobot-v30/ + eval_report.json（含标定表与吞吐行）
```

**LeRobot 多视频轨**：每台相机一条 `observation.images.{name}` 视频轨 +
一条 `depth.{name}` 图像轨（u16 PNG 序列，v2.1 树 `videos/chunk-000/depth.{name}/…`、
v3 树 `videos/depth.{name}/chunk-000/file-000/…`）；`features_json` / `write_dir(v3)`
按声明顺序展开，深度轨不进 episodes 元数据的视频指针列（image dtype 无视频时间戳，
转换脚本按帧目录拼装）。批量采集同款能力：`embodied_sim_collect --cameras-from-spec`
（用 spec.cameras 替换默认俯视；`--cam` 默认俯视不变）。

**定位**：这是**研究模式**的能力（PLAN §9.2 U-5 挂起项的 M3 兑现）——学生作业基线
仍是 21 维状态向量 + 正交俯视；真机标定板全流程（拍照→cv2 标定→回填 spec）属于
该模式下的工作流，引擎提供 spec 字段、渲染管线与 <2px 自检门槛三件事。

### 6.5 CI 门禁：三出口冒烟腿 × Windows 腿 × 指纹矩阵（PLAN-embodied-v3 §二 #14/#17/#18）

`.github/workflows/ci.yml` 具身相关腿一览（2026-11-04）：

| 腿 | 跑什么 | 硬顶 |
|---|---|---|
| **data-pipeline**（新） | 三出口一致性冒烟：2 集 `embodied_sim_collect --task reach --export npz` → `jsonl_to_lerobot_v21.py` 转换 → `lerobot_v3_selfcheck.py --v21`（含 MANIFEST 哈希对账）→ `oriera_results.py aggregate` → `plot_results.py`（无 matplotlib 走 CSV 降级）。全 pass 才绿——Python 数据管线从此有 CI 钉（QA-5/DE-2 此前为零） | 15 min |
| **windows-check**（新） | `cargo check/clippy -p oriera-embodied --features rigid3d,cam,spec,urdf,json` + `-p oriera-agent` 同参（ADR 0004 兼容壳 + python 桥）——PR 门禁从此不漏 Windows（本机主力平台）；check 比 test 便宜 | 25 min |
| rust（matrix） | 各 feature 腿 test+clippy；Build 步骤按腿收窄为本腿 `-p + features`（原 `cargo build --workspace` 对 feature 腿的编译单元零复用） | 60 min |
| **embodied-slow**（新，2026-11-06） | 慢测试分层夜间腿：`cargo test -p oriera-subsys --features rigid3d,cam -- --ignored` + `-p oriera-embodied --features rigid3d,spec,urdf -- --ignored`——抓取冒烟 ≈188s / Contact 接触判据 ≈110s / panda 7 轴跟踪等 `#[ignore = "slow: …"]` 物理长跑的 nightly 兜底（`workflow_dispatch` 可手动补跑），不进 PR 门禁 | 30 min |
| **embodied-fingerprints-matrix** | 三平台金标采集 × golden 对拍（manual dispatch） | 45 min |

**顶层改造（CI P1/P2）**：`concurrency: ci-<ref>` 同分支新推送取消旧跑；
`workflow_dispatch` 加 `matrix_only`（boolean，默认 false）——勾选后常规
job 全部跳过、只跑指纹矩阵（`gh workflow run ci.yml -f matrix_only=true`）；
指纹矩阵每 OS 归档 `eval_report.json` + `run_manifest.json` +
`lerobot-v21/meta/oriera_receipt.json` 为 artifact `fp-matrix-<os>`（#17，
离线对账不再翻日志）；browser 腿补 30 min 硬顶。

**慢测试分层约定（PLAN-embodied-v3 §二 #20，2026-11-06）**：物理长跑
测试分两层。**快层**（默认 `cargo test` 可见）随 PR 门禁 rust 矩阵腿跑；
**慢层**标 `#[ignore = "slow: <原因>"]`（现状三件：`gripper_down_smoke`
四种子抓取 + 同种子确定性双跑 ≈188s、`holding_contact_criterion` 接触
判据 3×610 tick ≈110s、`urdf_robot_spec` panda 7 轴 800 tick 跟踪），
由 nightly `embodied-slow` 腿 `cargo test -- --ignored` 兜底。约定：
新慢测试一律 `#[ignore = "slow: …"]` 并在文件头写明本地/夜间跑法；
快层单文件不许逼近 rust 腿的 `timeout 600` 护栏；慢层只进夜间，
不许逼近 30 分钟硬顶。

**数据集 MANIFEST 提交点语义（DE-P1）**：引擎直出目录以
`MANIFEST.jsonl` 为原子提交点；`jsonl_to_lerobot_v21.py` 转换收尾在同
级落 `MANIFEST.converted.json`（原清单全部条目 +
`"converted": true, "converted_at": <utc>`）——**转换后原 MANIFEST 只
指向 `raw_jsonl/` 归档，本文件才是转换后目录的提交点**；selfcheck
`--v21` 见该标记即按「已转换」口径对账（哈希在 Python 端用
splitmix64 byte-fold 对拍重算，不信任清单自述）。

## 5. 定位声明

OriEra 提供的是「便宜、可复位、可回放」的**世界层**：确定性 tick（#66）、
世界指纹（#67）、逐实体动作（#69）、刚体后端与数据出口（本 issue #80）。
大规模 GPU 并行、可微物理、真实传感器噪声——生态已有答案，接进来，
不重造。

## 7. 防抄袭对表 × gfi 候选池（PLAN-embodied-v3 P2「生态门面焕新」，2026-11）

### 7.1 指纹级查重：`scripts/fingerprint_dupcheck.py`

「同代码同参数必得同一串逐集指纹」是本台的确定性契约（§6.1），反过来它就是
最硬的防抄袭凭据：两份交上来的数据集若**逐集指纹全部相同**，只可能是同一份
采集冒充两次独立实验——比任何文本查重都硬。助教一条命令：

```bash
# 两个 run 目录（各自也支持 glob：runs/<hash8>/<method>/<seed>/）
python -I scripts/fingerprint_dupcheck.py out/student-a out/student-b --csv out/dup.csv
# 自指演示：同一目录跑两次 → 逐集全同 → exit 1（重复检出）
python -I scripts/fingerprint_dupcheck.py out/my-run out/my-run
```

- **读什么**：`lerobot-v21/meta/oriera_receipt.json` 的
  `episode_fingerprints_v1`（旧 receipt 只有 `episode_state_hashes` 键——值同，
  读兼容）；未转换的采集产物读 `eval_report.json` 的 `fingerprints` 块。
  两种来源的 `episode_state_hashes` **同名不同义**（P0-4 一名两义修复的背景）：
  receipt 里它是 v1 的旧键名，eval_report 里它是纯状态口径——脚本按来源分家
  取值，不会把纯状态当 v1 对。
- **退出码**：0 = 未检出指纹级重复（含一方无指纹——`embodied_sim_baseline`
  的产物不带逐集指纹，脚本如实说明）；1 = 指纹级重复（逐集全同）；
  2 = 输入/读取错误。`--view v2` 换完整身份口径，`--csv` 落逐集对照表。
- **判定口径（诚实边界）**：同 spec 同种子逐位一致本就是引擎契约（LAB-GUIDE
  §2）——全同不必然是抄袭，也可能是同一实验交两遍/助教复跑。脚本把
  spec_hash / seed / method 一起打出来供人判定：**spec_hash 不同而指纹全同 =
  最强实锤**；spec_hash 相同 = 重复提交/复跑，同样不能当两次独立采集计分。
  脚本只出事实表，结论由助教下。

### 7.2 good-first-issue 候选池（P2 刷新，欢迎认领；开 issue 前先对
[CONTRIBUTING.md](../CONTRIBUTING.md)「具身仿真台贡献入口」的验收锚）

1. ~~**drawer 真 prismatic fixture**~~ ✅ P2-W6B（2026-11-05）：走了更简的场景侧
   面而不是 RobotSpec 抽屉链——`scene.props[].slide`（prismatic 关节接出生位静态
   锚体，行程限位 + 滑轨阻尼，`MultibodyJointSet` 挂 `MultibodySystem` 的
   prop 同步协议），预设 `drawer-4dof-v2.json`（`drawer@v2`，基线 12/12）；
   旧 `drawer-4dof.json`（ef18ac07）保留为对照档案。
2. **sphere → Ball 直通**：URDF/mesh 的 sphere 碰撞体现在走 Box 包络近似
   （`urdf.rs`：sphere → Box 包络）；`LinkShape::Ball` 直通 + `LinkShape`
   旋转字段——mesh 碰撞量化的第一步（PLAN P2）。
3. **深度轨 v3 元数据列**：`depth.{name}` 图像轨暂不进 LeRobot v3 episodes
   元数据的视频指针列（image dtype 无视频时间戳），转换脚本按帧目录拼装
   补齐——等官方口径或定引擎侧补位方案（v0.11.0 release-notes known-limits #3）。
4. **Contact 判据升官方**：lab 变体 12 集对照已零翻转（P1-Wave3），升官方
   需按治理规则重跑 `results/reference/` 并写双轨说明（PLAN 拍板项 3 的 B 侧收尾）。
5. **渲染器摆 props[1..]**：叠块底座 B、料箱墙在物理与判据里都在、画面里
   看不见——wasm 侧 `prop_pose()` 真值口已通，差渲染器接线（v0.11.0
   known-limits #4）。
6. **Studio 导出逐集指纹收口**：浏览器内核已在导出 postMessage 里携带
   每集指纹与 `robot`/`specHash` 透传（P2-W5A 批次）；剩余收尾是 Studio
   落盘侧（`sim.rs`）把这些字段写进 receipt/info.json（此前是空数组 +
   硬编码 robot 名）并随发版验证（PLAN P2 排期项）。
7. **URDF base 朝向支持**：导入侧固定 Z-up→Y-up 上轴旋转，基座任意朝向的
   URDF（吊装/壁装）还不认（PLAN P2）。
8. **失败相位遥测进报告 + SIM_TRACE 文档化**（PLAN P2 排期项）。

这批候选已同步挂进 [CONTRIBUTING.md](../CONTRIBUTING.md)「具身仿真台贡献入口」
与 [FIRST_PR.md](FIRST_PR.md) 的指路牌（先写进文档池，转正开 issue 由维护者批量执行）。
