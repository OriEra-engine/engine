# 第 8 刀：L8 生成资产闸 1 + 闸 2 — 网格合法性深校验 / 尺度面数上限（闸 3、闸 4 未动）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

L8 四道闸的第 1 步。本轮把既有 `validate_asset` 从"尺寸上限 + 格式 magic"加深到：GLB 先过 L4 mesh 解码器深校验（拓扑、索引越界、有限坐标、单 mesh/单 primitive 口径），再核对顶点数 / 三角数 / AABB 半长三道限额。闸 3（碰撞体标记）、闸 4（溯源）属第 2 步元数据 additive 扩展，本轮一行未碰。

补丁：`patches/007-l8-asset-mesh-validation-gates.patch`（3 文件，+320 / -32，基于 `0387718`，叠在 001→006 之上）。

---

## 一、本轮之前的闸口现状

既有 `oriera-genai::validate_asset` 只做两道闸：

1. 尺寸 `max_bytes` 上限；
2. 各格式 magic / 容器自洽嗅探；GLB 仅 `glb_header_is_sane`（magic、版本、声明总长、JSON chunk 长度）。

解码器 `oriera-asset::glb::decode_glb_mesh` 早已实现深度网格校验，但 genai 闸口没有接它——坏网格只要容器头自洽就能过闸。本轮把这条深校验链路接上。

## 二、落地内容

### 1. 解码器侧（`crates/oriera-asset/src/glb.rs`）

冻结单 mesh / 单 primitive 路径补齐 fail closed 拒绝：

- **多 mesh**：`meshes.len() > 1` → `Unsupported("multi-mesh glb not supported by frozen mesh path")`。
- **多 primitive**：`primitives.len() > 1` → `Unsupported("multi-primitive mesh not supported by frozen mesh path")`。
- **坐标有限性**：逐坐标 `is_finite()`，NaN / Inf → 新变体 `GlbError::NonFinitePosition`。
- **三角拓扑三倍数**：有索引时 `indices.len() % 3 != 0` 拒；无索引时 `positions.len() % 3 != 0` 拒。

多 primitive / 蒙皮 GLB 归 `decode_glb_scene` / `decode_glb_rig` 另两条解码路径，不在冻结 mesh 闸口范围。

### 2. 闸口限额（`crates/oriera-genai/src/lib.rs`）

`ValidationLimits` 由单 `max_bytes` 扩展为四字段：

| 字段 | 类型 | 默认 | 硬顶 | 越界回退 |
|---|---|---|---|---|
| `max_bytes` | `u64` | 64 MiB | 既有硬顶 | 0 / 越界 → 默认 |
| `max_vertices` | `u32` | 50 000 | 1 000 000 | 0 / 越界 → 默认 |
| `max_triangles` | `u32` | 100 000 | 1 000 000 | 0 / 越界 → 默认 |
| `max_half_extent` | `f32` | 2.0 m | 100.0 m | NaN / ≤0 / 越界 → 默认 |

- 四字段统一在 `ValidationLimits::effective()` 内钳制，**单一钳制入口**；`GenAiLimits` 不重复存拓扑/尺度字段，避免两处阈值漂移。
- `validate_glb_mesh`：header 快速自检 → `decode_glb_mesh` 深校验（解码器 Err 折 `GenAiError::Validation`，消息前缀 `glb mesh: {e}`）→ 核对顶点数、三角数、半长。
- 三角数按拓扑分别换算：索引网格 `indices.len() / 3`；非索引网格 `positions.len() / 3`。
- 半长逐轴 `(max[i] - min[i]) / 2`，取最大轴比对 `max_half_extent`；包围盒由顶点数据实算，不取 accessor 声明的 min/max。
- 解码器已保证三倍数 / finite / 索引越界，闸口不重复校验。

### 3. 透传链路（`crates/oriera-genai/src/system.rs`）

`GenAiSystem::run` 构造 `validation` 时只覆盖 `max_bytes`，顶点 / 三角 / 半长沿用 `ValidationLimits::default()`；两处既有 `ValidationLimits` 字面量补 `..Default::default()`。

### 4. 类型兼容

`ValidationLimits` 新增 `f32` 字段后不再实现 `Eq`（f32 不满足），derive 改为 `Debug, Clone, Copy, PartialEq`；`assert_eq!` 仅需 PartialEq + Debug，测试不受影响。

## 三、评审修订项 R1–R5 落地对照

| 修订项 | 拍板口径 | 落地情况 |
|---|---|---|
| R1 多 mesh / 多 primitive | 方案 A，fail closed 显式拒绝，不静默忽略其余几何 | ✅ 解码器计数 >1 即 `Unsupported` 拒绝；多 primitive 归 scene 路径 |
| R2 面数换算 / 有限性 | 索引、非索引分别换算三角；NaN/Inf 拒；索引/顶点非 3 倍数拒；半长逐轴取最大轴 | ✅ 解码器 finite + 三倍数校验；闸口双拓扑换算 + 逐轴半长 |
| R3 阈值单一钳制 | 新限值并入 `ValidationLimits::effective()`，不与 `GenAiLimits` 两处漂移 | ✅ 四字段同一 `effective()` 钳制；GenAiLimits 不重复存拓扑字段 |
| R4 溯源字段兼容 | 第 2 步用 `Option` + `Default`，本轮不碰 | ✅ 本轮未改 `GeneratedAsset` 结构 |
| R5 阈值依据 | 宽松起步防炸弹，接真实样本后回调 | ✅ 默认 50k 顶点 / 100k 三角 / 2.0 m；硬顶 1M / 1M / 100 m |

## 四、明确没做（留第 2 步 / 后续）

- **闸 3 碰撞体标记**：`RenderOnly / NeedsCollider / ColliderReady` 枚举与资产标记字段，待评审拍板后 additive 落地。
- **闸 4 溯源**：provider / model 出处记录与强制闸口径（fail closed 拒绝 vs 宽松打标）未定；字段 `Option` 缺省，不进本轮。
- **碰撞体几何派生**：生成 GLB 当前无碰撞体派生几何库，embodied 碰撞体仍来自 URDF `<collision>` 或 spec 默认 5 cm cube。
- **USDZ zip 嵌套深度**（解压炸弹防护）：待 USDZ 解码器接到闸口后再补；解码器不读的字节不假装校验。
- **阈值回调**：50k / 100k / 2.0 m 为防炸弹宽松起步，待接真实 Tripo / Rodin 返回样本后按实测量级回调。
- **非索引三角换算端到端夹具**：索引分支由 `tiny_glb` 覆盖；非索引三角数换算在解码器侧有非索引解码测试，闸口端到端非索引夹具留待真实样本。

## 五、验收

干净 worktree 从 `0387718` 顺序 apply 001 → … → 006 → 007，独立 `CARGO_TARGET_DIR` 复现：

| 项 | 结果 |
|---|---|
| `oriera-asset` | 51 passed（+5：多 mesh 拒 / 多 primitive 拒 / NaN·Inf 拒 / 索引非三倍数拒 / 顶点非三倍数拒） |
| `oriera-genai` | 41 passed（+3：顶点超限拒 / 半长超限拒 / 解码器拒绝折 Validation）；doctest 3 |
| `oriera-agent`（`json,rigid3d,cam,spec,urdf`） | 96 passed |
| `cargo test --workspace` | 592 passed / 0 failed / 4 ignored |
| clippy | `-D warnings` 零告警 |
| rustfmt | `--check` 干净 |
| JS 镜像 | 130 pass / 0 fail（本轮未改 JS） |

## 六、下一棒（评审后再动）

1. 闸 3 碰撞体标记枚举 + 资产标记字段（additive，`Option` 缺省）。
2. 闸 4 溯源：先定 fail closed 还是宽松打标；`GeneratedAsset` 溯源字段是否进 `providers` wire schema 单独评估向后兼容。
3. 接真实 Tripo / Rodin 样本后回调顶点 / 三角 / 半长阈值。
4. 多 primitive / 蒙皮 GLB 是否补解码器 scene 路径支持，按真实生成返回占比再议。
