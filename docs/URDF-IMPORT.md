# URDF-IMPORT — OriEra 机器人描述文件互通设计（M1，2026-10-31 定稿）

> 一句话：让外部世界的机器人描述（URDF）**流进** OriEra 的具身训练台——
> `urdf.xml → 关节链 → ArmDescription/SceneSpec → TableTopEnv`，反向
> （引擎描述导出 URDF）暂不做。设计评审口径：sim2real 评审 TOP3 的
> 「模型格式互通」项，解锁与 MuJoCo/Isaac 的交叉验证与真机描述复用。

## 1. 选型与合规

- XML 解析：**roxmltree 0.20**（纯 Rust、零 sys 依赖、Apache-2.0/MIT、
  read-only DOM——依赖纪律三连过）。不引 urdf-rs 等封装库（拖 u-r-s-
  f 拓扑库与nalgebra 双栈，超需）。
- Feature 门控：`urdf = ["dep:roxmltree", "rigid3d"]`（agent 仓），默认
  构建零增量。

## 2. 范围（U0 骨架 = 本轮落地）

| URDF 元素 | 处置 |
|---|---|
| `<robot>/<link name>` | ✅ 解析（U0） |
| `<collision><geometry>` box/cylinder/sphere | ✅ 解析（几何簿记） |
| `<collision><geometry><mesh>` | ⏸ **记录进 skipped，绝不静默**（U2 接 L4 GLB） |
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

- **U0（本轮）**：解析骨架 + 链提取 + 映射 + 内嵌 fixture 三测试；
  集成入口 `UrdfReport::from_str`（CLI `--urdf <file>` U1 接）。
- **U1**：CLI 接线（`--urdf arm.xml --out specs/` 直接产出 SceneSpec）、
  effort/damping/axis 全接线、`check_urdf` 式链校验报告（父环/断链/
  类型错误人话报错）。
- **U2**：mesh 几何（package:// URI → L4 资产中心 GLB 管线）、
  `<inertial>` 真惯量、mimic 夹爪耦合、非常规关节轴、MJCF 参照对比
  （MuJoCo menagerie Panda 交叉验证力矩-轨迹一致性——sim2real 评审
  项的终点）。

## 5. 诚实边界

- U0 不做完整运动学校验（环/分支树）——`first_revolute_chain` 只走
  第一条线性链；分支机器人（双臂/腿足）U2。
- URDF 的 mesh 引用是外部文件（package:// 协议），U0 拿不到几何——
  引擎侧密度推导惯性，与真值偏差如实记录于 receipt。
- continuous 关节无限位：映射保持 OriEra 默认 ±π，不虚构。
