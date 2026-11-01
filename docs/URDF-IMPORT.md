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
