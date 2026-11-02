# URDF-IMPORT — OriEra 机器人描述文件互通设计（M1，2026-10-31 定稿）

> 一句话：让外部世界的机器人描述（URDF）**流进** OriEra 的具身训练台——
> `urdf.xml → 关节链 → ArmDescription/SceneSpec → TableTopEnv`，反向
> （引擎描述导出 URDF）暂不做。设计评审口径：sim2real 评审 TOP3 的
> 「模型格式互通」项，解锁与 MuJoCo/Isaac 的交叉验证与真机描述复用。

## 1. 选型与合规

- XML 解析：**roxmltree 0.20**（纯 Rust、零 sys 依赖、Apache-2.0/MIT、
  read-only DOM——依赖纪律三连过）。不引 urdf-rs 等封装库（拖 u-r-s-
  f 拓扑库与nalgebra 双栈，超需）。
- Feature 门控：`urdf = ["dep:roxmltree", "rigid3d", "dep:serde_json"]`
  （agent 仓），默认构建零增量。serde_json 仅供 meshres 的 GLB JSON
  chunk 探针（U3，本 crate 已有的可选依赖，非新外部 crate）；GLB
  全量解码在 L4 `oriera-asset`，L9 不反向依赖。STL/GLB 解析全部手写。
- 双端：Rust `crates/oriera-agent/src/meshres.rs` × JS 镜像
  `examples/15-embodied-sim/engine/urdf_mesh.js`（判定规则同源）。

## 2. 范围（U0 骨架 = 本轮落地）

| URDF 元素 | 处置 |
|---|---|
| `<robot>/<link name>` | ✅ 解析（U0） |
| `<collision><geometry>` box/cylinder/sphere | ✅ 解析（几何簿记） |
| `<collision><geometry><mesh>` | ✅ U2 登记 URI → U3 解析上岗（STL 全量 / GLB 探针，见 §5） |
| `<joint>` revolute/continuous/fixed/prismatic | ✅ 解析（U0） |
| `<joint>` floating/planar | ⏸ skipped 报告（U2） |
| `<origin xyz rpy>` | ✅ SI 米/弧度，rpy 外旋 XYZ |
| `<limit lower upper effort velocity>` | ✅ lower/upper → 关节限位；effort → `MOTOR_MAX_FORCE` 参考位（U1 接线） |
| `<dynamics damping>` | ✅ → 被动关节阻尼参考（U1 接线） |
| `<inertial>` | ⏸ U2（现由几何密度推导） |
| `<mimic>` | ⏸ skipped 报告（U2：夹爪耦合） |
| `<material>/<transmission>/<gazebo>` | 忽略（非动力学语义） |

## 3. 映射：URDF → OriEra 内部模型

```
<joint origin xyz> 的 ‖xyz‖          → ArmDescription.links[i]（连杆长）
<limit lower/upper>                  → ArmDescription.limits[i]
<dynamics damping>                   → 被动阻尼参考（≥默认值取大）
<axis>                               → U1 校验位（现假设关节系 Z 主轴，与
                                       OriEra 链约定一致；非常规轴 U2 支持时
                                       用 local_axis 重定向）
fixed 关节                           → 透明吸收（工具法兰等刚接件）
根 link                              → 无 parent 引用的 link（自动识别）
```

产线出口：`UrdfReport::first_revolute_chain(n)` → `ArmDescription` →
`SceneSpec` JSON（M1 #5 已建）→ `TableTopEnv::to_env()`——**一次 URDF
导入 = 一份可复现实验**，与指纹回执体系无缝衔接。

## 4. 里程碑

- **U0**：解析骨架 + 链提取 + 映射 + 内嵌 fixture 测试 ✅
- **U1**：CLI 接线（`--urdf arm.xml --out specs/` 直接产出 SceneSpec）、
  effort/damping 接线、`check_urdf` 式链校验报告（父环/断链/类型错误
  人话报错）✅
- **U2**：`<inertial>` 解析 × effort/axis 校验 × mesh URI 登记 ×
  prismatic 链提取 ✅
- **U3（上一轮）**：mesh 资产管线上岗 ✅——URI 解析（package:// 等）+
  STL 全量解析（Rust/JS 双端）+ GLB 探针 +
  `UrdfReport::validate_chain_with_meshes`（把 mesh 计数/包围盒尺寸
  并进链校验报告），详见 §6。
- **U4（本轮，PLAN-embodied-general v2.2 M0a 工作包 D）**：URDF →
  `RobotSpec` **全链导入** ✅——`UrdfReport::to_robot_spec`（N 轴串联链，
  不再投影到 4 轴 `ArmDescription`）、mesh 诚实降级（链段包络）、
  `<mimic>`/同父 prismatic 两指 → `GripperSpec`、`<limit velocity>` 补
  解析、Panda 7-DoF + 外部两指夹爪验收（`tests/urdf_robot_spec.rs`，
  夹具来源与许可见 `tests/fixtures/NOTICE.md`）、MJCF 交叉验证
  （§附录，125/125 PASS）。遗留进 M2：GLB 全量解码接进导入链消费
  `mesh_uri`、真惯量张量进物理、revolute 连杆夹爪（Robotiq 式）、
  非常规关节轴重定向。

## 5. U4 RobotSpec 全链映射表（URDF 元素 → RobotSpec 字段 → 降级规则）

入口：`UrdfReport::to_robot_spec(&mut RobotSpecOpts) -> Result<RobotSpec,
String>`（同一 URDF 内自带两指夹爪时一并检出）与
`UrdfReport::to_robot_spec_with_gripper(&arm, &gripper_urdf, opts)`
（**外部**夹爪 URDF 挂到臂链末体的工具法兰——最深一个被 fixed 吸收的
link，Panda 即 `attachment`）。默认把 URDF 的 Z-up（REP-103）整体旋成
OriEra 的 Y-up（`RobotSpecOpts.up_axis`，`(x,y,z) ↦ (x, z, −y)`）；
`mount` 给根 link 的世界落点。

### 5.1 关节（从根 link 走串联链，N 不限；fixed 透明吸收——link 前进、不生成关节）

| URDF | RobotSpec | 降级 / 缺省 |
|---|---|---|
| `<joint type="revolute">` | `JointSpec`（revolute） | 限位缺失 → ±π（入 `skipped`） |
| `<joint type="continuous">` | `JointSpec` | 限位恒 ±π（URDF 无限位语义，不虚构） |
| `<joint type="fixed">` | 无关节 | 子 link 并入父体；工具法兰记为末体 tip（外部夹爪挂点） |
| `<joint type="prismatic">` | **报错**（串联链） | RobotSpec M0a 的 `JointSpec` 仅 revolute——prismatic 链关节待 M0b 增 kind 字段；两条同父/`<mimic>` 配对的 prismatic 手指由 GripperSpec 检测消费 |
| floating / planar | **报错**（断链） | 人话指出类型与位置 |
| `<origin xyz rpy>` | `anchor` = 子系原点 − 父体原点（世界系）；FK：`T_child = T_parent · Trans(xyz) · Rz(y)Ry(p)Rx(r)` | 链一致性 `origin[child] == origin[parent] + anchor` 逐位成立 |
| `<axis xyz>` | `axis` = rpy 旋到父体（= 世界）后归一化；非 ±单位轴接受 | 缺省 (1,0,0)（URDF 规范缺省，入 `skipped`） |
| `<limit lower upper>` | `limits` | 见上 |
| `<limit effort>` | `effort = Some(e)` | ≤0 / 非有限 → `None`（引擎默认 80 N·m 钳） |
| `<limit velocity>` | `velocity_limit = Some(v)` | ≤0 / 非有限 → `None`（不限速） |
| `<dynamics damping>` | `damping`（被动黏性摩擦，**不是** PD 的 kd） | 缺省引擎默认 0.05 |
| ——（URDF 无此语义） | `kp`/`kd`：`opts.default_kp/kd`，`per_joint` 按名覆盖；`armature = opts.default_armature` | 缺省 60 / 4 / 0.02（4 轴预设同款） |
| 同父多关节 | 文档序首个入链 | 其余关节入 `skipped`（分支树待 M0b） |

### 5.2 link（每链体一个 `LinkSpec`；体系统一世界对齐，故碰撞体取向折进包络）

| URDF | RobotSpec | 降级 / 缺省 |
|---|---|---|
| `<collision><box size>` | `LinkShape::Box`（半尺寸 = size/2 经旋转取轴对齐包络；90° 整数倍旋转时精确） | — |
| `<collision><cylinder>` | 世界轴 ≈ ±X → `LinkShape::Capsule{half_len: L/2, radius}`（LinkShape 唯一有向 primitive，沿局部 +X） | 其他朝向 → Box 轴对齐包络（入 `skipped`；`LinkShape` 无旋转字段，M0b） |
| `<collision><sphere>` | `LinkShape::Box{half:[r,r,r]}`（包络） | — |
| `<collision><mesh filename>` | **诚实降级**：链段包络（见下）；`mesh_uri` 留 `None`（否则 `validate()` 拒绝），**URI 原文进 `opts.skipped`（M2 消费）** | — |
| 无 `<collision>` | 链段包络（入 `skipped`） | — |
| `<collision><origin xyz rpy>` | `collider_offset`（旋到世界系）；primitive 朝向同旋 | — |
| `<inertial><mass value>` | 密度按包络体积反推 `density = m / V`（**质量精确等价**，`added_mass = 0`）——真惯量张量 M2 接 | 无质量 → 引擎默认密度 1500 + `added_mass` 0.2 |
| `<link name>` | `LinkSpec.name` | — |
| ——（URDF 无此语义） | `friction` 引擎默认 0.7（根 0.5） | — |

链段包络规则（mesh / 缺碰撞体）：段 = 本体原点 → 下一关节原点（末体退用
最深吸收 fixed 子系）；长度 < 2 cm → 质量等价立方（Panda link1 即此——
joint2 原点与 joint1 重合）；半径由质量反推 `r = √(m/(π·ρ·L))`（钳
15–100 mm），段 ≈ ±X 用 Capsule 否则 Box 包络；密度再按实际包络体积
反推使质量等价。

### 5.3 夹爪（`GripperSpec`，**运动学双板近似**——真夹爪力控 M2）

检测：`<mimic>` 配对的 prismatic 对优先，其次**同父两条 prismatic**
（文档序）。revolute 连杆夹爪（Robotiq 式）暂不入列（入 `skipped`，M2）。

| URDF | GripperSpec | 规则 |
|---|---|---|
| 手指父 link | `attach_link`（链体下标） | 外部夹爪时 = 臂末体 |
| 两指板中心沿开合轴（手指 1 的轴）的间距 @ 手指 1 限位两端 | `gap_open`（大）/ `gap_closed`（小） | 手指 2 位移按 `<mimic multiplier offset>` 耦合；无 mimic 按行程比例同步 |
| 手指 link `<collision><box>` 半尺寸 | `finger_half`，按（接近轴, 其余, 开合轴）排进 builder 板系 `[X, Y, Z]` | 缺 box → 引擎默认 |
| 两指板中心中点（q=0） | `tcp_offset`（挂载体系，世界对齐——几何正确） | `finger_x` = 其沿接近轴的距离 |
| 手指 `<limit effort>` | `effort`（N，M2 力控夹爪消费；M0a 双板不消费） | 缺省 20 N |
| 手指 `<limit velocity>` | `gap_rate = v·(1+|multiplier|)/tick_hz`（缺省 60 Hz） | 缺省 0.004 m/tick |
| ——（URDF 无此语义） | `friction` 引擎默认 1.4；`kind = ParallelPlates` | — |

**已知近似（诚实边界）**：builder 的双板按「挂载 link 局部 +X 前伸、
局部 Z 开合」摆放（`GripperSpec` 无板系朝向字段——M0b 缺字段，见文末缺字段清单/工作包报告）。对外部夹爪，`tcp_offset`（TCP 遥测）是几何正确的，但
指板位置在该约定系内摆放——接近轴非 +X 的夹爪（如 Panda 法兰朝下的
真实构型）需要 M0b 的板系朝向字段才能板/TCP 全对齐。

### 5.4 其余

| URDF | RobotSpec | 备注 |
|---|---|---|
| `<robot name>` | `RobotSpec.name` | 缺省 `"urdf"` |
| `opts.mount` | `RobotSpec.base` 与根 link `origin` | 缺省原点 |
| —— | `home` = 零位（限位不含零时钳到最近限位，如 Panda joint4 → −0.0698） | — |
| —— | `action_scale = vec![2.0; n]`、`mass_scale = 1.0`（预设同款，包装层旋钮） | — |

验收（`cargo test -p oriera-agent --features rigid3d,urdf --test
urdf_robot_spec`）：panda7（menagerie rev b065505 换算件 + mesh 降级 +
datasheet velocity）7 关节 limits/effort/velocity/damping 逐项对照、
`MultibodySystem::from_robot` 构建并在限位内目标跟踪（最差关节误差
0.063 rad < 0.15）、gripper2f（手写 mimic 两指）检出 + 挂载构建 + 指
板按 spec 落位、4 轴 fixture 与旧投影关节数一致。夹具来源/许可：
`tests/fixtures/NOTICE.md`。

## 6. mesh 解析（U3）

双端实现：Rust `crates/oriera-agent/src/meshres.rs`（feature `urdf`）×
JS `examples/15-embodied-sim/engine/urdf_mesh.js`。二进制/ASCII 判定
规则两端同源：**长度 ≥84 且 84+50×头声明三角形数 == 实际长度 → 二进制
优先**（"solid" 开头的伪装二进制也按二进制走——真实 ASCII 文件长度
几乎不可能精确命中公式）；否则去前导空白后 `solid` 开头 → ASCII；
双判据皆失 → 显式报错并给出两个判据的实测值。

### 6.1 支持矩阵

| 格式 | Rust | JS | 说明 |
|---|---|---|---|
| STL 二进制 | ✅ 全量（TriMesh：按位去重顶点+索引+bounds+退化计数） | ✅ 全量（非索引展开，喂 three.js） | 长度交叉验证 |
| STL ASCII | ✅ 容错 token 解析 | ✅ 同 | 指数记数/缺 endsolid 均可 |
| GLB（.glb） | ✅ 轻量探针（容器校验 + meshes/primitives/name） | — | 全量解码走 L4 `oriera_asset::glb::decode_glb_mesh` |
| glTF（.gltf 裸 JSON） | ✅ 探针（同 GLB JSON 路径） | — | 不保证外部 buffers 存在性 |
| OBJ/DAE/… | ❌ 显式报错 | — | 等 L4 管线扩展 |

两端输出形状有意不同、各表各用：Rust `TriMesh` 面向合批/物理/校验
（去重 + 索引 + 退化计数）；JS 面向 three.js 直喂（非索引展开、面
法线重复 3 次、与 STLLoader 同语义）。

### 6.2 package:// 解析规则表

| URI 形态 | 解析 | 备注 |
|---|---|---|
| `package://<pkg>/<rel>` | `packages[pkg]/<rel>` | 缺 pkg 报错并**列出已知包** |
| `model://<pkg>/<rel>` | 同 package 语义 | Gazebo 惯用 |
| `file://<path>` | 剥前缀按本地路径 | 绝对路径按本地 OS 语义归一 |
| 裸相对路径 | `base_dir.join(path)` | CLI 场景 = URDF 所在目录 |
| `..` 逃逸 | **拒绝** | 词法归一后越过 base_dir/包根即报错；包内 `a/../b` 放行 |
| 反斜杠 | 归一为 `/` | Windows 工具链产出，跨平台行为一致 |

### 6.3 诚实边界

- STL 面法线不映射为顶点法线（去重后语义不定）——Rust `TriMesh.normals`
  恒空，渲染端按几何推导；JS 侧面法线随行（文件里是什么报什么）。
- GLB 探针不读 BIN chunk：有元信息、无几何/无 bounds——这是"有什么"
  不是"是什么"；全量解码（法线/索引/实算包围盒）在 L4。
- 退化/非有限三角形剔除**只计数不静默**（`TriMesh.degenerate`），孤儿
  顶点修剪后 bounds 由存活顶点实算。
- 联检入口：`UrdfReport::validate_chain_with_meshes(want, base_dir,
  packages)`——每条 mesh 资产的三角形数/包围盒尺寸/探针计数并进
  validate_chain 报告，资产缺位按人话入列，绝不 panic。

## 7. 诚实边界（U0 起）

- U0 不做完整运动学校验（环/分支树）——`first_revolute_chain` 只走
  第一条线性链；分支机器人（双臂/腿足）U2。
- continuous 关节无限位：映射保持 OriEra 默认 ±π，不虚构。
- mesh 引用是外部文件（§6 支持矩阵外的格式拿不到几何）——引擎侧密度
  推导惯性，与真值偏差如实记录于 receipt。

### 7.1 RobotSpec 缺字段清单（U4 实导入撞到的表达力缺口，给 M0b）

全部按纪律**未改** robot.rs（冻结），导入侧用包络/约定绕行并如实记录：

| 缺口 | 现状绕行 | M0b 建议 |
|---|---|---|
| `JointSpec` 无关节类型（仅 revolute） | prismatic 链关节直接报错 | 增 `kind`（prismatic 链滑关节、滑轨场景前置） |
| `LinkShape` 无旋转 / 无 Mesh 变体 | 旋转折进轴对齐包络；mesh → 链段包络 + URI 进 `skipped` | 旋转字段（或四元数 offset）+ M2 mesh collider |
| `GripperSpec` 无板系朝向（finger_x 恒 +X、gap 恒 Z） | tcp_offset 几何正确，板按 builder 约定系摆放 | 板系朝向 / gap 轴字段——外部夹爪「抓取过闸」（M0a 验收②后半）的前置 |
| 每链仅一个 `<collision>` 被消费 | 只取首个 collision | 链体多 collider 列表 |
| 惯量张量只进质量等价（密度反推） | ixx..iyz 已解析未进物理 | M2 真惯量进刚体构建 |

## 附录：MJCF 全量交叉验证（2026-11-01 · MuJoCo 3.17.0）

上一轮（ba021d0）做过 MJCF Panda 对比调研；本轮把 MuJoCo 当**第三方参照
系**做全量交叉验证：MuJoCo 编译后的模型逐参数读出，与 OriEra URDF 导入链
（`urdf.rs`，只读未改）的逐关节解析结果对比，外加工作空间 FK 抽查。

### A.1 两份真源

| 真源 | 文件 | 来源 | 许可 |
|---|---|---|---|
| MuJoCo 侧 | `out/mjcf-crosscheck/mujoco_menagerie/franka_emika_panda/panda_nohand.xml` | [google-deepmind/mujoco_menagerie](https://github.com/google-deepmind/mujoco_menagerie/tree/main/franka_emika_panda) rev `b065505`（sparse clone 实际下载，非合成） | Apache-2.0 |
| OriEra 侧 | `out/mjcf-crosscheck/panda_converted.urdf` | **换算参照**：7 关节精简 URDF，由上表 MJCF 1:1 换算生成（`scripts/mjcf_crosscheck.py` 内置模板，每次运行重写；body pos/quat→origin xyz/rpy、class panda range/damping、actuator forcerange→effort、`<inertial>` fullinertia 直拷） | 随源 |

- 仓库内无上轮遗留 MJCF/URDF（全仓 find 证实，ba021d0 只在 commit message
  里带结论）；mujoco pip 包亦无内置 franka——menagerie 为下载真源。
- Rust 探针：`crates/oriera-agent/examples/urdf_chain_dump.rs`（feature
  `urdf`，serde_json 由该 feature 自带，未加依赖）——把 `UrdfReport`
  逐关节/逐 link、`validate_chain(7)` 报告原文、`first_revolute_chain(4)`
  的 ArmDescription 映射结果序列化为 JSON。

### A.2 复跑

```bash
python scripts/mjcf_crosscheck.py
```

vendor 缺失时自动 sparse clone menagerie；Rust 侧 `cargo run --example
urdf_chain_dump -p oriera-agent --features urdf`（构建锁冲突等 30s 重试）。
产出 `out/mjcf-crosscheck/report.json` + 控制台表格，末行
`SUMMARY: PASS=125 TOLERANCE=0 FAIL=0 STRUCTURAL=7 TOTAL=132 -> VERDICT: PASS`
（exit code = verdict；2026-11-01 首测 104/8 → 非对角入 U3 后 125/7 →
armature 转 STRUCTURAL 后可作 CI 门禁：exit 0 = 无非结构性 FAIL）。容差：限位 0.02 rad、质量 5%、长度 1 mm、
damping 10%；PASS ≤ tol < TOLERANCE ≤ 2·tol < FAIL；结构缺项如实记 FAIL。

### A.3 对比表（MuJoCo 编译值 vs OriEra 导入值）

关节参数（7 关节全量，diff 列为全表最大值）：

| 参数 | joint1 | joint2 | joint3 | joint4 | joint5 | joint6 | joint7 | 最大差异 | 判定 |
|---|---|---|---|---|---|---|---|---|---|
| type（MJCF↔URDF） | hinge↔revolute | 同 | 同 | 同 | 同 | 同 | 同 | — | 7/7 PASS |
| axis | (0,0,1) | (0,0,1) | (0,0,1) | (0,0,1) | (0,0,1) | (0,0,1) | (0,0,1) | 0 rad | 7/7 PASS |
| range lower | −2.8973 | −1.7628 | −2.8973 | −3.0718 | −2.8973 | −0.0175 | −2.8973 | 7.5e−9 rad | 7/7 PASS |
| range upper | +2.8973 | +1.7628 | +2.8973 | −0.0698 | +2.8973 | +3.7525 | +2.8973 | 5.7e−8 rad | 7/7 PASS |
| damping | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 | 0 % | 7/7 PASS |
| armature | 0.1 | 0.1 | 0.1 | 0.1 | 0.1 | 0.1 | 0.1 | **n/a** | **7/7 FAIL（结构）** |
| effort（forcerange） | 87 | 87 | 87 | 87 | 12 | 12 | 12 | 0 % | 7/7 PASS |

连杆参数（8 body，diff 列为全表最大值）：

| 参数 | link0..7 + attachment | 最大差异 | 判定 |
|---|---|---|---|
| body pos（=joint origin xyz） | 0.333 / −0.316 / 0.0825 / (−0.0825,0.384) / 0.088 / (0,0,0.107) 等 | 4.3e−9 m | 8/8 PASS |
| body rot（quat↔rpy） | ±90°·x 链 + 135°·z（attachment） | 4.2e−8 rad | 8/8 PASS |
| mass | 4.970684 / 0.646926 / 3.228604 / 3.587895 / 1.225946 / 1.666555 / 0.735522 kg | 4.8e−8 相对 | 7/7 PASS（attachment 双侧无质量） |
| inertial com（body_ipos↔inertial origin） | 见 report.json | 2.6e−9 m | 7/7 PASS |
| inertia 对角（ixx/iyy/izz） | 全 21 项 | 4.7e−8 相对 | 21/21 PASS |
| inertia 非对角（ixy/ixz/iyz） | 最大 0.019169 kg·m²（link1 iyz） | 2.4e−8 相对 | 21/21 PASS（U3 已补，符号同号直等） |

FK 工作空间抽查：10 组随机关节角（seed 20261101，限位内均匀采样），
MuJoCo `mj_forward` 读 `attachment_site` xpos，对 OriEra 导出链几何手算
FK——算法：`T = Π Trans(origin_xyz)·Rz(y)Ry(p)Rx(r)·Rot(axis,q)`（rpy 为
extrinsic XYZ；Rust 无 FK 实现，按任务要求在脚本里手算并注明）。
**10/10 PASS**：原始偏差 max **4.0e−5 mm**（f32 截断量级，≤5 mm 容差三个
数量级余量）。

### A.4 偏差清单（FAIL / 超差项逐条根因）

1. **armature ×7（结构缺项，唯一余项）**——MJCF class panda
   `armature="0.1"`（转子惯性反射），URDF 1.0 规范无 armature 语义，
   `ArmDescription` 亦无对应字段。根因：表达力缺口（规范层），非解析
   错误。影响：名义加速段偏"轻"，实际由 PD kp/kd 与 forcerange 代偿。
2. **惯量非对角项 ×21 —— 已闭环（2026-11-01）**：`UrdfInertial` 补
   ixy/ixz/iyz 三字段 + 解析（URDF 惯例可省略，缺省 0）+ chain dump
   出值 + crosscheck 逐项真值对比（符号实证：R·diag·Rᵀ 张量非对角与
   属性值同号直等）。21/21 PASS（最大 2.4e−8 相对）。
3. **结构性发现（非 FAIL 行，informational）**——`first_revolute_chain(4)`
   把 Panda 映到 ArmDescription：`links=[0.333, 0.01, 0.316]`——joint2
   origin ‖xyz‖=0 被 `max(0.01)` 钳位，平面 4 阶映射对真实 7DoF 臂退化
   （ba021d0「7 DoF vs OriEra 4 DoF」的量化版）；`limits` 取 joint1..4、
   `passive_damping` 每关节覆盖写塌缩为标量（本次恰全 1.0 不可见差异）。
4. **参照系版本漂移（记录）**——ba021d0 引 damping 0.27（旧版
   menagerie / franka datasheet 齿轮箱阻尼）；当前 rev b065505 默认
   `damping="1"`。导入链两侧一致，是参照系自身在演进——复跑时以
   report.json 记录的 revision 为准。
5. **f32 截断（记录）**——`urdf.rs` 全量 f32，本表所有 PASS diff 的
   来源（~1e−8 相对量级），远低于容差，无需处理。

### A.5 结论（参数保真度判定）

- **可表达参数 125/125 全 PASS**：几何（axis/限位/body pos/rot）与动力学
  （mass/惯量全张量/damping/effort）对 MuJoCo 编译值一致到 f32 精度；
  `validate_chain(7)` 零告警；FK 10/10 ≤5 mm（实测 4e−5 mm）。
- **armature ×7 记 STRUCTURAL**（"参照系有、URDF/OriEra 无处安放"的
  表达力缺口——URDF 规范层就没有这个语义），不是导入链解析错误——
  按纪律未改任何 OriEra 物理参数凑对，如实入册但不计入 verdict。
- **判定：URDF 导入链对 URDF 可表达参数的保真度 PASS**；表达力缺口
  （armature 字段、7DoF→4DoF 平面化）进 backlog。
