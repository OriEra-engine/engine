# 评审：L8 生成资产四道闸方案（落码前复核）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

- 日期：2026-11-23
- 评审对象：`../plans/l8-generated-asset-gates.md` + 既有 `validate_asset` / `decode_glb_mesh` / `GenAiSystem` 现状代码
- 评审方式：逐文件读码，逐条复核方案事实断言与代码一致性；对照 ADR 0003（骨架小步、离线可测、诚实深度）、仓库纪律（非破坏、厂商中立、单一钳制入口、金样稳定）
- 评审基线：当前主仓库工作区，infer 85 / agent 96 / workspace 584 passed 0 failed（前序补丁 001–006 已绿）

## 一、总体结论

**方向有条件通过。** 方案把四道闸拆成"纯校验增强（闸 1 网格合法性 + 闸 2 尺度/面数）"与"元数据 additive 扩展（闸 3 碰撞体标记 + 闸 4 溯源）"两步，分级合理、落点可行、离线可测，与 ADR 0003 骨架纪律一致。

附 **R1–R5 修订项**：R1（多 primitive 静默忽略）未明确口径前，不建议接闸 1；R2/R3 需在落码前补进口径；R4 需列清构造点；R5 为阈值依据观察项。无 must-fix 级架构偏差。

## 二、事实核对（方案断言 vs 代码）

| 方案断言 | 代码核对 | 结论 |
|---|---|---|
| 既有闸口只做尺寸上限 + 格式 magic/容器自洽 | `validate_asset`（lib.rs 554）：`max_bytes` 比对 + 各 format 签名嗅探；GLB 仅 `glb_header_is_sane` | ✅ 一致 |
| 解码器已实现深度网格校验，但 genai 闸口未接 | `decode_glb_mesh`（asset/glb.rs 132）：拓扑/计数/索引越界/componentType/越界/交错拒绝；`validate_asset` 未调用它 | ✅ 一致 |
| 解码器只支持单 primitive / 单 mesh | 硬编码 `/meshes/0/primitives/0`（glb.rs 136）；其余 mesh/primitive 不读 | ✅ 一致，见 R1 |
| `GlbMesh` 包围盒由数据实算 | glb.rs 205–212 逐顶点 min/max，不用 accessor 声明 min/max | ✅ 一致 |
| `GeneratedAsset` 无 serde derive | lib.rs 306 仅 `Debug, Clone, PartialEq, Eq`；serde 仅 `providers` feature 下部分类型有 | ✅ 一致，见 R4 |
| genai 已依赖 oriera-asset，解码路径公开 | Cargo.toml `oriera-asset.workspace = true`；asset lib `pub mod glb` + `pub fn decode_glb_mesh` | ✅ 一致，闸 1 可直接调 |
| 校验失败走占位降级 | system.rs 463 校验 `Err` → `fail(..., degraded=true)` → 占位资产入 `PendingSwaps` + `AssetFailed` 事件 | ✅ 一致 |
| 引擎单位为米 | embodied 碰撞体/AABB 半长 0.05 m，PHYSICS-FIDELITY 以 m 计 | ✅ 一致 |

## 三、纪律复核

| 检查项 | 结论 |
|---|---|
| ADR 0003 骨架小步、单人仓库无千行大 PR | ✅ 两步拆分；第 1 步只加校验函数与阈值，不动系统 trait/调用路径 |
| 诚实深度（解码器不存在的深度不假装） | ⚠️ 方向对，但 R1 指出多 primitive 被静默忽略，需补拒绝或标注 |
| 厂商中立、Key 不入引擎 | ✅ 闸口不含厂商事实；溯源只记 provider/model 标识 |
| 非破坏 | ✅ 第 1 步 additive 校验；第 2 步 `Option` 字段扩展，不改 `GenerationProvider` trait |
| 金样/确定性稳定 | ✅ 校验只拦坏资产；合法 `tiny_glb` 夹具过闸，不碰渲染/物理确定性 |
| 单一钳制入口 | ⚠️ 新阈值须并入 `ValidationLimits::effective()`，避免与 `GenAiLimits` 两处漂移，见 R3 |
| 离线可测 | ✅ 全部闸口可由 `MockProvider` / `tiny_glb` 夹具离线回归，不依赖真实远程后端 |

## 四、发现的问题

### R1（should-fix，闸 1 覆盖面）：多 primitive / 多 mesh 被静默忽略，不是被拒绝

**事实**：`decode_glb_mesh` 硬编码读 `/meshes/0/primitives/0`。文件含第二个 primitive 或第二个 mesh 时，解码器不报错，只校验第一个；其余几何未经验证就随资产过闸。

**问题**：闸 1 若直接复用解码器，会留下"未校验几何静默放行"的口子，与 fail closed 哲学（`CapNeed` 未知 token 拒、索引越界拒）不一致。

**修法建议（落码前拍板其一）**：
- 方案 A（推荐，fail closed）：闸 1 先读 JSON 计数 `meshes.len()` / `primitives.len()`，超过 1 即 `Unsupported("multi-primitive mesh not yet validated")` 拒绝；与解码器当前支持面一致。
- 方案 B：放行但在资产元数据标注"仅首 primitive 已校验"，留待解码器扩展。骨架阶段不建议 B（静默未校验几何进场景）。

### R2（should-fix，闸 2 口径）：面数换算与有限性校验未定义

**事实**：
- 索引网格 `indices.len()/3` 为三角数；非索引网格 `indices` 为空，三角数为 `positions.len()/3`。`read_positions` 直接 `from_le_bytes`，**不拒 NaN/Inf**；解码器也未校验索引数/顶点数是否为 3 的倍数。

**修法建议**：
- 闸 2 面数上限按两种拓扑分别换算；索引数、顶点数非 3 倍数的坏网格拒绝。
- 包围盒核对前先逐坐标 `is_finite()`；NaN/Inf、`max < min` 退化包围盒拒绝。
- 半长逐轴 `(max[i]-min[i])/2` 取最大轴比对 `max_half_extent`。

### R3（should-fix，阈值透传链路）：新限值须并入单一钳制入口

**事实**：`GenAiLimits`（system.rs 50）只内嵌 `max_bytes`；run 内 271 行由 `limits.max_bytes` 构造 `ValidationLimits`。若 `ValidationLimits` 加顶点/三角/半长上限，需明确透传链路。

**修法建议**：优先 `ValidationLimits` 自带合理默认并在 `effective()` 内钳制（同 `max_bytes` 哲学），`GenAiLimits` 不重复存这些字段；系统层 271 行构造处沿用默认或透传。避免两处各存一份阈值漂移。

### R4（should-fix，契约兼容）：溯源字段的构造点清单与兼容性

**事实**：`GeneratedAsset` 共有 11 处字面量构造：`cogview.rs:138`、`hot_swap.rs:62`、`lib.rs:800/814`、`mock.rs:66`、`rodin.rs:290`、`seedance.rs:193`、`system.rs:254`、`tripo.rs:120`、`tests/chain_l4.rs:83`。加**必填**字段会打破全部构造点。

**修法建议**：溯源字段用 `Option<AssetAttribution>`，结构体派生 `Default`；构造点补 `None`（或 `..Default::default()`），不要求适配器当场填。`providers` feature wire schema 是否序列化新字段单独评估（向后兼容：新字段 `Option` 缺省反序列化为 `None`）。

### R5（nit，阈值依据）：面数/尺度上限缺实测量级对照

**事实**：建议 half_extent ≤ 2.0 m、vertices ≤ 50k、triangles ≤ 100k；`tiny_glb` 半长 ≤ 0.8 已覆盖夹具，但 50k/100k 对文生 3D 实测返回量级无对照。

**建议**：骨架阶段上限以"防炸弹 / 防飞出去"为主，取宽松值先拦极端；正常生成是否被卡，待接真实 Tripo/Rodin 返回样本后回调。阈值最终值随 P1 拍板。

## 五、待拍板项（沿用方案清单，复核成立）

1. 尺度默认阈值：half_extent / vertices / triangles 取值（R5 建议先宽松）。
2. 溯源闸口径：缺溯源 fail closed 拒绝，还是宽松记录打标（建议骨架先宽松记录，强制闸留后）。
3. 碰撞体标记枚举：`RenderOnly / NeedsCollider / ColliderReady` 是否可用。
4. `GeneratedAsset` 溯源字段是否进 `providers` wire schema。
5. 多 primitive / 蒙皮 GLB：按 R1 方案 A 显式拒绝，还是补解码器支持。

## 六、拍板建议

1. **第 1 步（闸 1 + 闸 2）可进入落码，前置条件**：R1 选方案 A 显式拒绝多 primitive；R2 面数换算 + finite 校验口径写进实现；R3 阈值并入 `ValidationLimits::effective()` 单一钳制。
2. **第 2 步（闸 3 标记 + 闸 4 溯源）**：待 R4 构造点清单确认、溯源口径（待拍板 2）定了再动；字段 additive、`Option` 缺省。
3. **留后项不变**：碰撞体几何派生、USDZ zip 嵌套深度（解压炸弹）、强制溯源闸。
4. 落码后按既有流程：独立 `CARGO_TARGET_DIR` 干净复现 infer/agent/workspace + clippy `-D warnings` + fmt + JS 镜像，再出补丁 007。
