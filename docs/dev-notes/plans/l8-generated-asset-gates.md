# L8 生成资产四道闸 — 现状调研与落地方案（待评审）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

- 日期：2026-11-23
- 范围：`crates/oriera-genai`（L8）+ `crates/oriera-asset`（L4 解码器）
- 性质：能力补全（生成资产入闸校验），非破坏；契约扩展部分待拍板
- 关联：ADR 0003（L8 供应商中立边界）、RFC 0003 同套"单一钳制入口 + 幻觉参数回退默认"纪律

## 一、既有闸口现状

生成资产在 L8 系统层（`system.rs`）poll 成功后，统一经 `validate_asset(&GeneratedAsset, &ValidationLimits)` 入闸；失败走占位网格降级（`degraded` 计量），不崩溃。当前 `validate_asset` 只做两道：

1. **尺寸上限**：`max_bytes`，默认 64 MiB，硬顶 1 GiB，`effective()` 单点钳制（零/越界回退默认）。
2. **格式 magic / 容器自洽**：GLB `glTF` magic + 版本 2 + 总长一致 + JSON chunk 长度；USDZ `PK` 前缀；MP4/WebM/PNG/JPEG/MP3/WAV 签名嗅探。

解码器 `oriera-asset::glb::decode_glb_mesh` 已实现深度网格校验，但 **genai 闸口没有接它**——头合法、网格坏掉的 GLB 目前能过闸。

## 二、四道闸逐项盘点

### 闸 1 · 网格合法性（GLB 深校验）

| 项 | 结论 |
|---|---|
| 现状 | 解码器已校验：三角拓扑、POSITION/NORMAL 计数一致、索引不越界、componentType 白名单、bufferView 越界、交错布局拒绝；闸口只跑 header sane |
| 缺口 | 头合法但网格坏（缺 POSITION、非三角、索引越界、计数不符）的资产可过闸 |
| 落点 | `validate_asset` 对 `AssetFormat::Glb` 调 `decode_glb_mesh`，解码 `Err` 折成 `GenAiError::Validation`；genai 已依赖 oriera-asset，无新依赖 |
| 风险 | 解码器当前只支持单 primitive / 单 mesh 最小网格；多 primitive、蒙皮、变形目标会被 `Unsupported` 诚实拒绝（不假装支持）。既有适配器夹具均为 `tiny_glb()` 单三角网格，解码必过，不误杀 |
| 骨架可否落 | ✅ 可落 |

### 闸 2 · 尺度与面数上限

| 项 | 结论 |
|---|---|
| 现状 | `GlbMesh` 实算 `min/max` 包围盒；无尺度、顶点数、三角形数阈值 |
| 缺口 | 巨型（坐标 ~1e9）、微型（~1e-9）、超高模（百万三角）资产无闸拦截 |
| 落点 | `ValidationLimits` 扩 `max_vertices: u32` / `max_triangles: u32` / `max_half_extent: f32`（米制，引擎单位为米）；decode 后核对包围盒半长与面数；阈值走 `effective()` 同一钳制哲学，幻觉参数回退默认 |
| 待拍板 | 默认阈值（建议起步：half_extent ≤ 2.0 m、vertices ≤ 50k、triangles ≤ 100k，评审定） |
| 骨架可否落 | ✅ 可落（阈值待拍板） |

### 闸 3 · 碰撞体

| 项 | 结论 |
|---|---|
| 现状 | embodied 碰撞体来自 URDF `<collision>` 或 spec 默认 5 cm cube；生成 GLB 无碰撞体，仓库无网格→碰撞体派生（凸包 / 盒近似）几何库 |
| 缺口 | 可交互场景里生成资产无碰撞体；自动派生是重几何算法，骨架阶段做不了，**也不能假装做了** |
| 骨架落点 | 只做**声明 / 标记**：资产元数据标碰撞体状态（渲染-only / 待派生 / 已就绪）；闸口不强制"必须带碰撞体"，否则卡死所有渲染-only 资产。真正凸包 / AABB 派生留后续几何工具 |
| 待拍板 | 标记枚举口径（建议 `RenderOnly / NeedsCollider / ColliderReady`） |
| 骨架可否落 | ⚠️ 仅元数据标记，不派生 |

### 闸 4 · 许可溯源

| 项 | 结论 |
|---|---|
| 现状 | `GeneratedAsset` 有 `provider_id / model / usage(tokens, cost_micros)`；无 license、署名、seed / prompt 溯源字段 |
| 缺口 | 生成资产未记录许可来源与署名，合规审计无据 |
| 落点 | `GeneratedAsset` 加溯源元数据（license 声明、来源 prompt 摘要 / seed；provider/model 已有）。闸口校验口径待拍板：fail closed（缺溯源拒绝）还是宽松记录（缺溯源打标 / 警告，不拦） |
| 契约兼容 | `GeneratedAsset` 当前无 serde derive；加字段不破坏内存构造，`providers` feature 的 wire schema 需同步评估 |
| 待拍板 | 必填 vs 宽松；license 取值词表 |
| 骨架可否落 | ✅ 元数据 + 记录；强制口径待拍板 |

## 三、分级落地建议（对齐 ADR 骨架小步、离线可测纪律）

- **第 1 步（纯校验增强，不动系统路径）**：闸 1 网格深校验 + 闸 2 尺度 / 面数上限，接既有 `validate_asset` 单入口；阈值走 `effective()` 钳制；mock / 适配器夹具回归钉。
- **第 2 步（元数据 additive 扩展）**：闸 4 溯源字段 + 闸 3 碰撞体标记；先记录不强制。
- **留后**：碰撞体几何派生；USDZ zip 嵌套深度（解压炸弹防护）；强制溯源闸。

## 四、待拍板清单

1. 尺度默认阈值：half_extent / vertices / triangles 上限取值。
2. 溯源闸口径：fail closed 拒绝，还是宽松记录打标。
3. 碰撞体标记枚举口径。
4. `GeneratedAsset` 加字段是否进 `providers` wire schema（serde 兼容）。
5. 多 primitive / 蒙皮 GLB 被解码器拒绝，是否接受为诚实拒绝（而非补支持）。

## 五、纪律与红线核对

- 单一钳制入口：新阈值与 `max_bytes` 同走 `ValidationLimits::effective()`，不散落应用层。
- 诚实深度：解码器不支持的特性显式拒绝，不假装解析；闸口只承诺已实现的校验深度（ADR 0003 红线）。
- 厂商中立：闸口不含厂商事实、Key 不入引擎；溯源只记录 provider/model 标识。
- 不越对外口径：生成资产以占位 / 降级口径表述，不写"资产开箱即用"。
- 离线可测：全部闸口可由 `MockProvider` / `tiny_glb` 夹具离线回归，不依赖真实远程后端。
