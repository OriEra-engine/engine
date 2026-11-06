# POSITIONING — OriEra 具身台定位与保真卡（M3-B，2026-11-03）

> 一句话：**OriEra 具身台不拼并行吞吐，拼「可审计」**——一份 JSON 就是实验身份（`spec_hash`），每集一张逐位回执（指纹），纯 CPU 能跑，浏览器里跑的是**同一份物理代码**（rapier → wasm）。要千环境 GPU 并行、照片级渲染、灵巧手/软体，去 ManiSkill / Isaac Lab；要可复核的课程作业、论文基线和「改一个字段 = 新实验」的研究纪律，用这里。
>
> 回答 PLAN-embodied-general §9.1 **P-12**（审稿人问「凭什么不用 ManiSkill/Isaac」）。数字全部来自仓库内可复跑的命令，出处见每节末。

## 1. 差异点（同类工具对照）

| 维度 | **OriEra 具身台** | ManiSkill3 | Isaac Lab |
|---|---|---|---|
| 物理后端 | rapier3d **多体约简坐标**（纯 Rust、f32、CPU） | SAPIEN / PhysX（GPU 并行为主路径） | PhysX（Isaac Sim，GPU 并行为主路径） |
| 复现契约 | **同构建逐位一致**：每集 `sim_fingerprint` + v2 完整身份指纹；`contracts/golden/` 钉 12 集逐集哈希，改物理须过金标 | 统计复现（并行 GPU 求解不以逐位回执为契约） | 同左 |
| 实验身份 | `SceneSpec` JSON → 规范化 `spec_hash`（`meta` 不入 hash；字段拼错即报错） | Python 环境类 + 配置 | `@configclass` 层级配置 |
| 浏览器 | **同一份 rapier 编译 wasm**（`oriera-wasm`）：浏览器内双跑逐位一致；native↔wasm 为**统计一致**（libm 末位 1 ulp 级差异，docs/WASM-RENDERER.md §4——跨目标逐位是 M3 三平台矩阵议题） | 无 | 无 |
| 硬件门槛 | 任意 CPU（Windows/Linux/macOS），零 GPU | 并行路径需 NVIDIA GPU | 需 NVIDIA RTX |
| 规模取向 | 单环境 + 进程级并行（`SubprocVecEnv`） | 千～万级 GPU 并行 | 千～万级 GPU 并行 |
| 渲染 | 俯视/针孔相机（教学/示意级） | 光栅 + 光追 | RTX 照片级 |

**不做清单**（PLAN §八，与本页一致）：GPU 千环境并行、RTX 渲染、四足/人形、软体流体。

## 2. 保真卡：七个官方预设（八文件，drawer@v1 为对照档案）

共同点：抓取 = **运动学双板（`ParallelPlates`）+ 板/物体摩擦夹持**（板不是力控手指）；`holding` 标签默认 `TipRadius`（闭合且物体中心在 TCP 0.06 m 内——启发式标签，物理上的夹持由接触摩擦完成；接触力判据 `HoldingCriterion::Contact`（≥1.0 N 双板法向力 ∧ ≤0.05 m/s 滑速）在 P2-Wave2 起由官方预设 `panda-contact-criterion` 开启——`holding` = 「物理说夹住」，其余官方预设仍走 TipRadius 缺省）。求解器 = **rapier 0.36 默认**（各预设 `physics` 全为 null）：PGS 4 次迭代、每 tick 2 子步（物理步 1/120 s，tick 1/60 s）、接触软度 30 Hz / ζ=10（贴固定体 60 Hz）、warmstart 1.0、关节约束 1e6 Hz（硬）；电机 = ForceBased PD（约束式隐式弹簧-阻尼，effort 作冲量钳）。

| 预设（spec_hash8） | 机器人 / 关节 | 抓取模型要点 | 伺服参数 | 基线（course@v1，12 集） | 吞吐（单环境，release） |
|---|---|---|---|---|---|
| `tabletop-4dof-pick`（c90869dc） | 4 轴转动（yaw/肩/肘/腕） | 6×9 cm 板、开 75 mm / 合 46 mm（2 mm/侧挤压） | kp/kd 60/4 N·m/rad、armature 0.02、effort 80 N·m | 脚本 11/12 = 91.7%（金标锚）· DLS 11/12 | 采集 2334 steps/s（本轮实测）· DLS 781 |
| `panda-7dof-pick`（b5535a74） | Panda 7 轴（URDF 真惯量全张量） | 外部 gripper2f：1.7 cm 指块、开 88 mm / 合 46 mm | 300/30、armature 0.05、effort 87/12、阻尼 1.0 | DLS 10/12 = 83.3% · paper@v1 100 集 **80%**（P1-B2 相位机调参，heavy 排程只动 ≥6 DoF） | 1106（course）· 1932（paper 抽验单跑） |
| `panda-contact-criterion`（95f75626，P2-Wave2 升格） | Panda 7 轴（同上） | 同 `panda-7dof-pick`；仅 `holding` 标签换 Contact 判据（物理逐位不变，success 不读 holding——两口径同 seeds 逐集零翻转，course 12 + paper 100） | 同 Panda | DLS 10/12 = 83.3% · paper@v1 100 集 80% | 667 / 561（course 双跑）· 802 / 884（paper 双跑） |
| `stack-4dof`（249d5122，旧身份 ffc96325 复测逐位同） | 4 轴 | 同桌面板；判据 @v2（松爪 + 静止 10 步） | 同桌面 | DLS 11/12 = 91.7% | 1146 |
| `bin-pick-4dof`（1a7cfc40，P2 真 fixture 升级） | 4 轴 | 料箱 = 真 fixtures（双层壁 + 底面，P2 升级；四面重墙 prop 近似为旧身份 14fc9bf8 对照） | 同桌面 | DLS 12/12 = 100% | 564 |
| `drawer-4dof-v2`（f503b3a3，P2-W6B 真滑轨；ef18ac07 为 prop 近似对照档案 10/12） | 4 轴 | 面板 = slide prop（真 prismatic，撞不走、限位钳制），柜体 = 真 fixtures | 同桌面 | DLS 12/12 = 100%（双跑逐位一致，2026-11-05） | 1940（P2-W6B 重测入 `results/reference/f503b3a3`） |
| **`mobile-arm-4dof`（9ee15317，M3-B 新增）** | 2 prismatic（x/z，米）+ 底盘 yaw + 4 轴臂 | 同桌面板；龙门式底盘（prismatic 悬离地面 2 cm，**不建轮地接触**） | 滑轨 500/80 N/m、effort 150 N、armature 0.5 kg；臂同桌面 | `--validate` ✓；DLS 0/12（**不是物理结论**：`ik.rs` 的 FK 只会转动关节）→ 以 settle/跟踪测试替代：TCP 随底盘的位姿误差 < 1 mm | DLS 跑 182 |

吞吐出处：M2 列来自 `baselines/README.md`（2026-11-03，`embodied_sim_baseline --release`）；本轮数字来自 M3-B 验收（同机并行有 WP-A 构建，绝对值偏低，供量级参考）；contact-criterion 与 drawer-v2 行来自各自 `results/reference/` 文件自记吞吐（2026-11，同机并行有构建负载，供量级参考）。纯物理（无接触、无策略）参考：Panda 7 轴 **8220 ticks/s**（`urdf_traj_dump --release`，每 tick 含 2 子步）。

**prismatic 量纲（M3-B）**：`JointSpec.kind = Prismatic` 的关节，同一组字段按米制读——限位/home/动作目标/遥测 `angles` 为 m、`velocities` 为 m/s、`kp` N/m、`kd` N·s/m、`effort` N、`armature` kg；观测里的 `<joint>.pos` / `<joint>.vel` 列因此是**米与弧度混排**（列名即关节名，单位跟关节类型走；奖励只用 TCP/物体/目标的笛卡尔量，不读关节角）。

## 3. MuJoCo 交叉验证（第三方参照系，`python scripts/mjcf_crosscheck.py`）

参照系：menagerie `franka_emika_panda/panda_nohand.xml`（rev b065505，Apache-2.0）。

**运动学 / 参数节**（2026-11-01 起，本轮复跑不变）：可表达参数 **125/125 PASS**（轴/限位/body 位姿/质量/惯量全张量/阻尼/effort）、7 项 STRUCTURAL（armature：URDF 无此语义，不计入判定）、FK 10/10（偏差 ≤ 4e−5 mm）。

**动力学节（v2，M3-B 新增，只测量不调参）**：两侧同一台 Panda、同一串 7 关节目标（5 个限位内航点 × 120 tick = 600 tick）、同增益 kp/kd 300/30、同 effort 87×4 / 12×3、同 armature 0.05（menagerie 原 0.1 → 对齐 OriEra 预设）、同阻尼 1.0、同重力 9.8、无接触、同时间基（1/60 s tick × 2 子步）；MuJoCo 积分器保持 menagerie 原配 `implicitfast`。逐 tick 对比 7 个关节角：

| 指标 | 数值 |
|---|---|
| 全程 max \|Δq\| | **0.0440 rad**（joint6，第 5 tick 的起步瞬态：两侧都从 q=0 出发，joint6 以 12 N·m 饱和爬升） |
| 全程 mean \|Δq\| | **0.0024 rad** |
| 30-tick 网格 max \|Δq\| | 0.0143 rad |
| 稳态（每航点末 10 tick）max \|Δq\| | **0.0005 rad**（容差 0.05 → **PASS**） |
| 分关节 max \|Δq\| | j1 0.0328 · j2 0.0252 · j3 0.0262 · j4 0.0377 · j5 0.0188 · j6 0.0440 · j7 0.0190 rad |

结论：**平衡态（重力下 PD 的稳态下垂）两引擎一致到 5e−4 rad；差异全在目标跳变后的瞬态**。根因（按影响排序）：① 伺服形式——rapier ForceBased 电机是约束式隐式弹簧-阻尼（ERP/CFM），MuJoCo position 执行器是显式 `kp·(ctrl−q) − kv·q̇`，仅速度项隐式（implicitfast）；② 饱和——每次目标跳变 kp·误差都远超 effort（MuJoCo 侧 600 tick 中各关节饱和 11–51 tick：肩 j2 51、腕 j5 35），约束冲量钳与显式力钳塑形不同；③ 积分器——rapier 约简坐标辛欧拉 + 4 次 PGS 迭代、f32，MuJoCo 全质量矩阵、f64；④ joint4 起点 q=0 在其限位外，两侧限位约束的软化方式不同。机读全量：`out/mjcf-crosscheck/report.json` 的 `dynamics` 节。

## 4. 什么时候用谁

| 你要做的事 | 用谁 |
|---|---|
| 课程作业 / 实验报告要「同一个数」、可复核回执、零 GPU、Windows 照抄能跑 | **OriEra** |
| 论文基线要逐位可复现（spec_hash + 种子 + 指纹，`paper@v1` 协议）、改物理须过金标 | **OriEra** |
| 浏览器里给学生/评审演示，且演示与训练是同一份物理代码（同目标逐位、跨目标统计一致） | **OriEra** |
| 小 N 轴臂抓放/推/叠/抽屉、龙门式移动底盘抓取的数据采集（LeRobot v3 导出） | **OriEra** |
| 千～万环境 GPU 并行 RL、域随机化海量采样 | ManiSkill3 / Isaac Lab |
| 照片级相机仿真、合成数据、传感器噪声模型 | Isaac Lab |
| 灵巧手、接触丰富操作、软体/布料/流体、腿足 | ManiSkill3 / Isaac Lab / MuJoCo |
| 高保真接触动力学、MPC/轨迹优化研究、成熟模型库 | MuJoCo（+ MJX 上 GPU） |

## 5. 诚实边界（随期更新）

- **抓取**：运动学板 + 摩擦夹持，不是力控手指；`TipRadius` 是标签启发式（接触力判据已在官方 `panda-contact-criterion` 开启——`holding` =「物理说夹住」，其余预设缺省 TipRadius）。
- **移动底盘**：prismatic 龙门抽象，无轮地牵引/打滑；DLS 基线待 `ik.rs` 的 FK 支持 prismatic（平移而非绕轴转）。
- **fixture**：L6 已支持任意个数、每块独立摩擦（`FixtureSlab` / `MultibodySystem::from_robot_fixtures`）；料箱/抽屉的「真 fixture」版本已在 P2 落地为官方预设——`bin-pick-4dof`（1a7cfc40，双层壁 + 底面真 fixtures）、`drawer-4dof-v2`（f503b3a3，真 prismatic 滑轨面板 + 真 fixture 柜体）。
- **跨平台**：同构建逐位一致；native↔wasm 已实测为统计一致（1 ulp 级）；三平台（ubuntu/windows/macos-arm64）指纹矩阵为 M3 待办（P-10）。
- **相机**：教学/示意级（俯视 + 针孔/畸变/深度由 M3-A 推进），不对标照片级渲染。
