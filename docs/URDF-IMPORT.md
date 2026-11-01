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
- **U3（本轮）**：mesh 资产管线上岗 ✅——URI 解析（package:// 等）+
  STL 全量解析（Rust/JS 双端）+ GLB 探针 +
  `UrdfReport::validate_chain_with_meshes`（把 mesh 计数/包围盒尺寸
  并进链校验报告），详见 §5。
- **U4（待办）**：GLB 全量解码接进导入链（L4 `oriera-asset` 已有，
  待 CLI/SceneSpec 出口消费）、`<inertial>` 真惯量接入物理、mimic
  夹爪耦合、非常规关节轴重定向、MJCF 参照对比（MuJoCo menagerie
  Panda 交叉验证力矩-轨迹一致性——sim2real 评审项的终点）。

## 5. mesh 解析（U3）

双端实现：Rust `crates/oriera-agent/src/meshres.rs`（feature `urdf`）×
JS `examples/15-embodied-sim/engine/urdf_mesh.js`。二进制/ASCII 判定
规则两端同源：**长度 ≥84 且 84+50×头声明三角形数 == 实际长度 → 二进制
优先**（"solid" 开头的伪装二进制也按二进制走——真实 ASCII 文件长度
几乎不可能精确命中公式）；否则去前导空白后 `solid` 开头 → ASCII；
双判据皆失 → 显式报错并给出两个判据的实测值。

### 5.1 支持矩阵

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

### 5.2 package:// 解析规则表

| URI 形态 | 解析 | 备注 |
|---|---|---|
| `package://<pkg>/<rel>` | `packages[pkg]/<rel>` | 缺 pkg 报错并**列出已知包** |
| `model://<pkg>/<rel>` | 同 package 语义 | Gazebo 惯用 |
| `file://<path>` | 剥前缀按本地路径 | 绝对路径按本地 OS 语义归一 |
| 裸相对路径 | `base_dir.join(path)` | CLI 场景 = URDF 所在目录 |
| `..` 逃逸 | **拒绝** | 词法归一后越过 base_dir/包根即报错；包内 `a/../b` 放行 |
| 反斜杠 | 归一为 `/` | Windows 工具链产出，跨平台行为一致 |

### 5.3 诚实边界

- STL 面法线不映射为顶点法线（去重后语义不定）——Rust `TriMesh.normals`
  恒空，渲染端按几何推导；JS 侧面法线随行（文件里是什么报什么）。
- GLB 探针不读 BIN chunk：有元信息、无几何/无 bounds——这是"有什么"
  不是"是什么"；全量解码（法线/索引/实算包围盒）在 L4。
- 退化/非有限三角形剔除**只计数不静默**（`TriMesh.degenerate`），孤儿
  顶点修剪后 bounds 由存活顶点实算。
- 联检入口：`UrdfReport::validate_chain_with_meshes(want, base_dir,
  packages)`——每条 mesh 资产的三角形数/包围盒尺寸/探针计数并进
  validate_chain 报告，资产缺位按人话入列，绝不 panic。

## 6. 诚实边界（U0 起）

- U0 不做完整运动学校验（环/分支树）——`first_revolute_chain` 只走
  第一条线性链；分支机器人（双臂/腿足）U2。
- continuous 关节无限位：映射保持 OriEra 默认 ±π，不虚构。
- mesh 引用是外部文件（§5 支持矩阵外的格式拿不到几何）——引擎侧密度
  推导惯性，与真值偏差如实记录于 receipt。

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
