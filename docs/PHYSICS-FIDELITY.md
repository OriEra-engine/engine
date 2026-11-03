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
  rev b065505 换算件；`cargo test -p oriera-agent --features
  rigid3d,urdf --test urdf_robot_spec` 的
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
cargo test -p oriera-agent --features rigid3d,cam,spec,urdf --test urdf_robot_spec
cargo test -p oriera-agent --features rigid3d --test gripper_down_smoke
cargo test -p oriera-subsys --features rigid3d --test holding_contact_criterion
cargo test -p oriera-wasm
cargo clippy -p oriera-subsys -p oriera-agent -p oriera-wasm --features rigid3d,cam,spec,urdf --all-targets
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
