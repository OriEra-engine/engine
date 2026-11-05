# PHYSICS-FIDELITY — 物理保真层（M2-A，PLAN-embodied-general v2.2 §四 M2 / §9.1 U-1/U-3/U-6/U-9）

> 一句话：URDF 里"写了但没进物理"的动力学参数（真惯量、网格碰撞体、
> 自碰撞、接触级抓取判据）这一工作包全部**进构建**——每一项都是一个
> **opt-in spec 字段**，缺省值 = 旧行为，默认桌面实验（tabletop-4dof-pick）
> 的轨迹与 v1/v2 指纹**逐位不变**（golden 测试钉死）。
> 铁律的验证：`golden_default_path_is_bit_identical_to_pre_m0a` 全绿 +
> `contracts/golden/embodied_fingerprints.pre-m0a.json` 12 集逐个相等
> （本包验收输出附后）。

## 1. U-1 真惯量进刚体构建

**spec 字段**（`oriera_subsys::robot`）：

```jsonc
// oriera.scene/2 → robot.links[i]
{
  "inertial": {                      // 缺省省略 = 密度几何推导（旧行为）
    "mass": 3.228604,                // kg, > 0
    "com": [0.0275, -0.0665, -0.0393],  // 体轴（导入时已从 URDF link 系旋到 OriEra 体轴）
    "ixx": 0.037242, "iyy": 0.036155, "izz": 0.01083,
    "ixy": -0.004761, "ixz": -0.011396, "iyz": -0.012805   // 体轴、关于 com
  }
}
```

- **何时 opt-in**：机器人 URDF 带 `<inertial>`（真机描述/MJCF 换算件），
  或 DR 实验要按真实惯量扫参。
- **物理语义**：`Some(inertial)` 时该链体的**整个质量模型被替换**——
  质量与张量（随 `mass_scale` 线性缩放）经
  `MassProperties::with_inertia_matrix` 显式设置到刚体（含主轴分解），
  该链体的 collider 贡献零质量（rapier 零质量属性短路合并，读回逐位
  等于 spec 值）；`density`/`added_mass` 退为簿记（构建端忽略）。
  `None`（缺省）→ 与 M0 逐位相同的 density·几何 + added_mass 路径。
- **armature**：per-joint `JointSpec::armature` 自 M0a 已进 multibody
  DoF 槽位（`build_arm` 第 5 步）；本包补了读回口
  `MultibodySystem::joint_armature(i)` 作验收证明。
- **验收数字**（Panda 夹具 `tests/fixtures/panda7.urdf`，menagerie
  rev b065505 换算件；`cargo test -p oriera-embodied --features
  rigid3d,spec,urdf --test urdf_robot_spec` 的
  `panda7_inertials_reach_the_rigid_bodies`）：

  | link | URDF 质量 (kg) | `mass_properties()` 读回 | 张量迹读回 | URDF 迹 |
  |---|---|---|---|---|
  | link1 | 4.970684 | 4.970684（逐位） | 1.419097 | 1.419097 |
  | link2 | 0.646926 | 0.646926 | 0.062067 | 0.062067 |
  | link3 | 3.228604 | 3.228604 | 0.084227 | 0.084227 |
  | link4 | 3.587895 | 3.587895 | 0.073728 | 0.073728 |
  | link5 | 1.225946 | 1.225946 | 0.073650 | 0.073650 |
  | link6 | 1.666555 | 1.666555 | 0.011751 | 0.011751 |
  | link7 | 0.735522 | 0.735522 | 0.027358 | 0.027358 |

  （质量 f32 直通逐位相等；张量比对用三个旋转不变量——迹/行列式/
  Frobenius²，容差 1e-4/2e-3/1e-4 相对，因为主轴分解是正交共轭、
  读回的是特征值。link0 是固定根，rapier 对 fixed 体质量归零——其
  URDF 张量仍逐项在 `spec.links[0].inertial`。质心：读回 == spec
  逐位，模长 = URDF inertial origin 模长（旋系不变量）。）
- armature 读回：7/7 == opts 0.05（`joint_armature`）。

## 2. U-3 凸包/网格碰撞策略

**spec 字段**：

```jsonc
// robot.links[i].shape 的两个新变体（点集由导入侧解析，引擎不吃 URI）：
{ "shape": { "convex_hull": { "points": [[x,y,z], …] } } }   // 链体推荐
{ "shape": { "mesh": { "vertices": […], "indices": [[a,b,c], …] } } }  // 接触面 opt-in
// robot.gripper 上：
"contact_mesh": true   // 缺省 false = 解析 cuboid 板（接触更稳）
```

- **策略**（文档化结论）：**链体 = 凸包**——rapier
  `ColliderBuilder::convex_hull` 生成流形接触，稳定；**夹爪接触面可选
  trimesh**（`contact_mesh: true`，板被铺成 12 三角形网格）——三角形
  接触用于接触保真实验，缺省关。`Mesh` 变体同样直接可建
  （`ColliderBuilder::trimesh`，索引越界是命名构建错误）。
- **导入链**（`urdf.rs` → `meshres`）：`<collision><mesh filename>`
  的 **STL** 在给了 `RobotSpecOpts::mesh_base_dir`（可选
  `mesh_packages`，`package://` 语义）时全量解析，顶点从 URDF link 系
  旋到 OriEra 体轴、减去碰撞体偏移后填进 `LinkShape::ConvexHull`；
  `mesh_uri` 保留原文作 provenance（**M2 起不再是 validate 错误**，
  只是元数据，构建端从不读它）。GLB/glTF 仍探针 → 包络降级
  （全量解码 = M3）；OBJ/DAE/文件缺失/未给根目录 → 包络降级，URI 进
  `opts.skipped`（人话行注明原因）。
- **人话报告**：`oriera_subsys::collision_report(&spec) -> String`——
  逐链体一行：形状、碰撞体积（primitive 解析式；凸包按密度 1 实算；
  开网格无闭体积 → "—"）、AABB 半尺寸。外部 STL 臂验收即
  `stl_mesh_resolves_to_a_convex_hull`（8 顶点立方 STL → 8 点凸包、
  AABB 边长 0.1、`from_robot` 过闸）。
- **验收数字**：合成 12 三角/8 顶点立方 STL → `parse_stl` 去重 8 点 →
  凸包构建 + AABB 半尺寸 [0.05, 0.05, 0.05]（±1e-5）；
  共面点云 → 命名错误 `BadShape`（凸包体积 0 被拒，坏 STL 不会变成
  隐形碰撞体）；`mesh_uri`+primitive 共存 → 建 primitive（元数据不挡
  构建）。

## 3. U-9 自碰撞 spec 开关

**spec 字段**：

```jsonc
// robot 上：
"self_collision": "adjacent_exempt"   // 缺省省略 = "off"（旧行为）
```

- **语义**：`Off`（缺省）——臂只与家具/道具碰撞，臂↔臂一刀切关
  （legacy 碰撞组逐位不变）。`AdjacentExempt`——每链体一个碰撞组
  身份位（GROUP_3..GROUP_32，即位 2..31），成员=自身位、过滤=世界
  位 + 全部**非相邻**链的位；相邻对由 per-joint
  `contacts_enabled(false)` 兜底。板（GROUP_2）与链体依旧不碰、与
  道具照常碰（抓取不受影响）。**上限**：链长 >30 时身份位用尽，
  尾部链共享位 31 并失去其间非相邻接触（如实限制；默认路径不受
  影响）。
- **何时 opt-in**：bin_pick/抽屉类任务（臂要伸进箱体，碰不到自己会
  学歪——U-9 原诉）。
- **验收**（`adjacent_exempt_enables_non_adjacent_self_contacts`）：
  4 轴预设折肘 `[0, 1.0, -2.5, 1.2]`——`Off` 自接触对 = **0**，
  `AdjacentExempt` = **>0**（forearm×base 非相邻对产生真实接触点）；
  两模式遥测有限；模式本身是 v2 身份（折叠仅非默认时）。

## 4. U-6 接触级抓取判据

**spec 字段**：

```jsonc
// robot.gripper 上：
"holding_criterion": "tip_radius"     // 缺省（v1 启发式，可省略）
"holding_criterion": { "contact": { "min_normal_force": 1.0, "max_slip": 0.05 } }
```

- **语义**：`TipRadius`（缺省）——夹爪需求闭合 + 方块中心距尖端
  `GRASP_RADIUS`(6 cm) 内（几何启发式，物理抓取仍由摩擦接触完成）。
  `Contact`——**两块板同时接触同一个道具**，各自法向力（求解器接触
  冲量 ÷ 子步 dt，逐对流形点求和后取绝对值）≥ `min_normal_force`，
  且板-物相对速度（板速为板心有限差分）≤ `max_slip`。`holding`
  遥测随判据走；负阈值/非有限值是命名校验错误。
- **何时 opt-in**：要拿 `holding` 当监督/成功标签的实验（U-6 原诉：
  启发式在板还没碰到方块时就标"已抓"污染标签）；真机 case 对照。
- **验收数字**（`tests/holding_contact_criterion.rs`，M0b 朝下夹爪
  场景复用，thresholds 1 N / 0.05 m/s，seed 固定）：
  - holding 时序：闭爪需求前恒 false；闭爪前 10 tick 里 TipRadius
    已标 **10/10**、Contact 只标 **5/10**（后 5 tick 板才真正压上
    ——这正是 U-6 要消除的假标签窗口）；闭爪末 30 tick Contact
    **≥25/30** 稳定为 true；提升全程方块被摩擦携带（y 0.525 →
    >0.565）；松开后 0 tick 为 true。
  - 一致率报告（闭爪+提升 230 tick）：both=216、仅 TipRadius=14
    （全部是预接触窗口）、仅 Contact=0 → Contact 是 TipRadius 成功集
    上的**保守子集**（成功集一致率 100%，预接触差异 = 设计目的）。
  - 同判据重跑逐位一致（确定性契约）。

### 4.1 U-6 验证步：Contact 判据 lab 变体 × 12 集对照（2026-11-04，P1-C2）

PLAN-embodied-v3 拍板项 3-A 兑现：`crates/oriera-embodied/specs/lab/
panda-contact-criterion.json`（extends `panda-7dof-pick`，仅换
`holding_criterion = Contact`）对官方 TipRadius 口径，同一 12 seed
（course@v1，`embodied_sim_baseline`，release，确定性复核 full 双跑
逐位一致）。

- **阈值取值依据**：`min_normal_force = 1.0 N`——50 g 方块经 μ=1.4 板
  摩擦抵重力单板需 ≈0.18 N，1 N 远高于重力负载又远低于 20 N 板
  effort（挤压段实测法向力远超它，预接触窗口 <1 N 被排除——正是要
  消除的假标签）；`max_slip = 0.05 m/s`——down-gripper 实测稳定携带
  滑速在其之下，区分「压住但打滑」与「真携带」。出处 =
  `holding_contact_criterion.rs` 的验证阈值 + gripper_down_smoke 同
  场景实测（M0b/WPA Contact 实现文档 §4 上表）。
- **12 集成功率对照（once / at_end 双报）**：

  | 口径 | once | at_end | transitions | 判据 spec_hash |
  |---|---|---|---|---|
  | TipRadius（官方 panda-7dof-pick） | **10/12（83.3%）** | 10/12 | 6574 | b5535a74f18fb57b |
  | Contact（lab 变体） | **10/12（83.3%）** | 10/12 | 6574 | 95f75626fd83f70b |

  逐集**零翻转**：同一 10 个种子成功（66539/66548 双口径同败）、逐集
  steps 逐个相等、物体轨迹逐位相同（判据只改 `holding` 遥测标签，
  不进物理回路——脚本基线开环，6574 transitions 两口径全等）。
  **Contact 显著不低——判据可以换，不折成功率**；双败种子是
  抓取相位本身的失败，与标签口径无关（如实记录，不改判据凑数）。
- **reward 通道差异（如实报）**：pick_place 的 reward 折入 holding
  （grasp 事件 + 每步持有项，`tasks.rs`），换判据后 12 集逐集
  reward_sum 全部改变（±0.3～±10.7），而轨迹/成功不变——换判据是
  **换监督信号**，对 RL 训练是实质变化（U-6 原诉的量化版：假标签
  直接进奖励）。升级官方判据前需按 P2 项重跑 reference。
- **holding 时序抽样**（seed 1000 成功集，ORIERA_SIM_TRACE=5，trace
  每 5 tick 采样，含确定性双跑 224 行）：224 行中 agree 154、
  仅 TipRadius 70、仅 Contact **0** → Contact 仍是 TipRadius 的保守
  子集。两个假标签窗口都被 Contact 消除：①闭爪需求翻转 tick（grip
  0→1）当拍 TipRadius 即标 held（板还没压上，预接触窗口，Panda 上
  ~5 tick），Contact 推迟到 ~190 tick 板真正压上；②放置段尾
  （~515–555 tick）方块已入目标区、板压强衰减，TipRadius 仍标
  held 直到集末，Contact 在力掉到 1 N 以下即摘标。
- **结论**：episode 级成功率两口径等价（10/12 = 10/12，零翻转），
  判据差异全在标签时序——即 U-6 的设计目的。是否把 Contact 升为
  官方预设判据，按 PLAN-embodied-v3 §三 P2（「Contact 判据出 lab
  变体跑 12 集对比后决策升官方」）留待决策；本变体留在 `specs/lab/`
  供复跑（`embodied_sim_baseline --spec crates/oriera-embodied/specs/
  lab/panda-contact-criterion.json`）。

## 4.2 slide prop——场景侧真 prismatic 可动 fixture（P2-W6B，2026-11-05）

PLAN-embodied-v3 §三「真抽屉/真料箱 fixture 预设」的抽屉半边。**opt-in
spec 字段，缺省逐位不变**：`scene.props[].slide`（serde 整体省略）不写
时 prop 走原自由体路径，所有既有 spec 的 `spec_hash` 与指纹一位不动。

- **机制**（`oriera-subsys::rigid3d::SlideSpec`，组件挂在 prop 实体上）：
  prop 的动态体经 `MultibodyJointSet` 的 prismatic 关节接到一个
  **出生位冻结的无碰撞体静态锚体**上——面板只保留滑轴一个自由度，
  行程限位（米，零点 = 出生位）+ 滑轴阻尼（N·s/m = 滑轨滑动摩擦模型），
  无位置电机。被夹爪板摩擦拖动；垂直于滑轴的推挤被关节约束吸收
  （「被夹拖时整箱可被撞走」的 v1 缺陷根治）；外部 `Pose3` 写入沿滑轴
  **投影采纳并钳到行程**（multibody FK 会覆写裸体写入，采纳走关节坐标
  `apply_displacements` + 前向运动学——同步契约在滑轨上同样成立）。
- **几何取舍（探针实测钉死，见 rigid3d in-module slide 测试注释）**：
  夹爪板对滑轨面板的合爪挤压必须**∥滑轴**。直列布局（拉出 −X、滑轴 X、
  gap 轴 Z）下挤压 ⊥ 滑轴：法向被关节锁死 → 求解器经摩擦锥把 4 mm 过
  挤压沿唯一的自由方向（滑轴）泄放，面板被弹飞到行程限位。预设
  `drawer-4dof-v2` 因此把滑轨放沿 **Z**（拉出 −Z；预设夹爪 gap 轴在
  pick 姿态 = 世界 Z），面板在臂轴线上（合爪面与面板面平行、yaw=0
  不楔出），面板尺寸 = 120×50×50 mm（5 cm 夹持带与金标方块同宽同高，
  顶缘 0.55 逐毫米沿用经验证的合爪净空）。
- **验收**：`drawer-4dof-v2`（`drawer@v2`，spec_hash `f503b3a3`）12 集
  course@v1 基线 **12/12 = 100%**（release，`--verify-determinism full`
  双跑逐位一致；Wilson [75.8, 100.0]；闸 ≥60% pass）；单元面五项
  （出生位保持/外力拖到限位+拖回+闭合端钳制/teleport 采纳投影钳制/
  夹爪摩擦拖动 ≥8 cm/双跑逐位）+ agent 集成六项（v2 身份/滑轨保持/
  拖到位成功正例/短拉反例/不松爪反例/v1 对照档案）全绿。旧
  `drawer-4dof`（ef18ac07）保留为对照档案。

## 5. wasm 只读口（M1 遗留收尾）

- `OrieraSim.gap() -> f32`：当前指板间距（m）。镜像与引擎
  `MultibodySystem::gap_now` 是**同一 f32 递推**（同需求历史、同 spec
  常量），逐位一致——M1 遗留 #2 关闭（引擎侧 `MultibodySystem::gap()`
  只读口已备，等 `TableTopEnv` 转发后可切真值，口径不变）。
- `angles()`：已在（观测通道；`obs_noise=0` 时即真值——引擎侧真值口
  同样等 env 转发）。
- **prop 姿态四元数（遗留，如实报告）**：`Pose3.rot` 本来就是
  `pub`——真正的缺口是 `TableTopEnv.world/sys` 私有且无
  `prop_pose()` 访问器，而 `embodied_env.rs` 在并行工作包手里（铁律
  禁碰）。`poses()` 里 prop 仍标 `"rot_known": false`；三行修复是
  `TableTopEnv::prop_pose(&self) -> Pose3` 转发 + wasm 侧换用（
  `poses()` 的 `props[0].rot` 字段与 `rot_known` 开关已就位）。
- **顺手修正**：`poses()` 的指板镜像此前硬编码 +X/+Z 摆放，M0b 的
  板系朝向（`approach_axis`/`gap_axis`）没接——已改为按 spec 轴放置
  （缺省轴归一化后是精确单位向量，默认预设的输出**逐位不变**）。
- node 侧 `wasm_golden_check.mjs` 口径不变（指纹/obs 面）。

## 6. 身份与指纹纪律（为什么不破坏金标）

`robot_identity_hash`（v2）对 M2 新字段**只在非默认时折叠**：
`inertial` None / `ConvexHull`/`Mesh` 不存在 / `self_collision=Off` /
`holding_criterion=TipRadius` / `contact_mesh=false` → 一个哈希轮都
不多。v1 三元组折叠不动、`sim_fingerprint` 不动 → 默认实验的 v1/v2
指纹与 pre-M2 逐位相同。serde 侧全部 `skip_serializing_if`（默认值
不出现在 spec JSON）→ `spec_hash` 也不变。

## 7. 验收命令速查

```bash
cargo test -p oriera-subsys --features rigid3d,cam
cargo test -p oriera-embodied --features rigid3d,cam,spec,urdf --test urdf_robot_spec
cargo test -p oriera-embodied --features rigid3d --test gripper_down_smoke
cargo test -p oriera-subsys --features rigid3d --test holding_contact_criterion
cargo test -p oriera-wasm
cargo clippy -p oriera-subsys -p oriera-embodied -p oriera-agent -p oriera-wasm --features rigid3d,cam,spec,urdf --all-targets
cargo check --target wasm32-unknown-unknown -p oriera-wasm
cargo run -p oriera-agent --example embodied_sim_collect --features rigid3d,cam,spec,urdf \
  -- --task pick_place --episodes 12 --out out/_m2a   # 对 contracts/golden/embodied_fingerprints.pre-m0a.json
```

## 8. URDF-IMPORT §7.1 缺字段清单勾账（2026-11-03）

| §7.1 缺口 | 状态 |
|---|---|
| `GripperSpec` 板系朝向 | ✅ M0b 加字段，本包把导入侧接上（approach/gap 轴 + 挂载系 AABB 半尺寸） |
| 惯量张量只进质量等价 | ✅ 本包：全张量直进 `LinkSpec.inertial` 进构建 |
| `LinkShape` 无 Mesh 变体 | ✅ 本包：`ConvexHull`/`Mesh`（无旋转字段——旋转折进体轴点集本身，链体凸包策略下不缺） |
| `JointSpec` 无关节类型 | ⏳ 未做（prismatic 链关节仍报错；M0b 未加 kind） |
| 每链仅一个 `<collision>` | ⏳ 未做（多 collider 列表，后续包） |

## 9. P-12 收账：MuJoCo 动力学对比的口径——tick≤30 启动瞬态，以 settled 0.0005 rad 为准（2026-11-04）

PLAN-embodied-general §9.1 P-12（「审稿人问『凭什么不用 ManiSkill/Isaac』无答案；
抓取模型注释与实现矛盾」）在 M3 交付了 MuJoCo 动力学级对比
（`scripts/mjcf_crosscheck.py` v2 DYNAMICS 节，meausurement-only）。下轮评审
重开时直接引用本段口径，不要用全轨最大偏差说话：

- **全轨最大 |dq| = 0.0440 rad**（601 tick × 7 关节，mean 0.0024 rad）——
  这个数字**全部来自目标阶跃后 ≤30 tick 的启动瞬态**：worst 在 tick 5
  （joint6，rust 0.524 vs mujoco 0.480），30-tick 采样格最大 0.0143 rad。
  瞬态根因是伺服公式不同（rapier ForceBased 软约束 = ERP/CFM 弹簧阻尼 vs
  MuJoCo 显式力 `kp·(ctrl−q) − kv·q̇`），加上阶跃后 kp·error 超 forcerange
  的饱和段整形不同（mujoco.saturated_ticks 逐关节有数）、积分器/求解器
  （symplectic Euler + PGS vs implicitfast + Cholesky）、joint4 限位瞬态
  ——同平衡点、不同瞬态阻尼，`report.json` 的 `root_cause` 四条原文可引。
- **settled 口径（判准）**：每个 waypoint 末 10 tick（DYN_SETTLE_TAIL）的
  max |dq| = **0.0005 rad**，相对 0.05 rad 容差两个数量级余量 → 动力学
  verdict **PASS**。脚本 rule 原文：*"PASS = settled (last ticks of every
  waypoint) max |dq| <= tol; transient numbers are measured and reported,
  never tuned"*——瞬态数字如实测量、永不调参（两边引擎的瞬态整形差异
  不是"误差"，是可解释的设计差异）。
- 复跑：`python scripts/mjcf_crosscheck.py`（exit code = 运动学/参数
  verdict；动力学只随 SUMMARY 行附报）。2026-11-01 实测报告：
  `out/mjcf-crosscheck/report.json`（max 0.0440 / settled 0.0005 / PASS）。
- rigid3d.rs 头注释矛盾项已在 M2/M3 波次修正（Grasping 段之后接 M0b/M2/
  M3 三段如实描述），本项无余量。

（本段与 URDF-IMPORT.md 附录 A 的参数保真对比互补：A 是「参数级」，本段
是「轨迹级」——评审问动力学一致性时以 settled 口径答，问参数一致性时引
附录 A。）
