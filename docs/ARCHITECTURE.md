# OriEra 技术架构文档

> 一句话理解：**十一层是责任边界，接口契约是连接器，横切系统是全局仪表盘与刹车。**
> （《OriEra 世界引擎》技术篇 T3）

本文是仓库的完整架构文档，对应 **main 分支**（写作时点 2026-11，工作区版本
0.10.0、具身五期能力已并入；逐版说明见 [release-notes/](release-notes/)——
版本声明用活链不用死数，本文不再随小版本手改）。
定位：读完这一篇，你应该能回答三个问题——每层是什么、代码在哪、为什么这样切。
层职责与接口契约的完整定义见书技术篇 T4–T15 与附录 B；书 ↔ 仓库对照见 [BOOK_MAP.md](BOOK_MAP.md)。

---

## 1. 这是什么

OriEra 不是又一个游戏引擎。它是一个 **AI 原生、虚实融合、可自演化的开放数字世界运行时**：

- **AI 原生**：AI 是引擎系统，不是应用代码。认知、长期记忆、AI 导演都是挂在调度器上的 `System`，应用层零 AI 代码（见 [AI-NATIVE.md](AI-NATIVE.md)）。
- **虚实融合**：PC / 手机 / 网页 / AR 眼镜五端一等公民，脑机接口 v1 只留 bit 位不接硬件。
- **可自演化**：用户用自然语言创造与改写世界内容（demo 10「言灵世界」、demo 11「隅角咖啡馆」两个范式）。

在线体验：**https://oriera-engine.github.io/engine/**（15 个浏览器可玩 demo，零安装；
含 14 号影游 `frame-hunter/` 与 15 号具身仿真台旗舰）。

## 2. 五条设计硬规则（CI 守护）

1. **只准向下依赖**：上层只依赖下层的抽象接口，禁止出现 `render → subsys` 这类倒置（v0.0.1 曾有，已修复）。
2. **禁止跨层共享可变全局状态**：v0.0.1 曾用 `thread_local! WORLD` 让 L5/L6 偷看世界，已删除。现在的规则：应用壳（`platforms/*`）持有 World，L2 调度器每帧把 `&mut World` 显式传给每个 System；跨 System 共享状态走 L3 resource 单例。
3. **平台差异止步 L1**：上层读 `Capabilities` 位图做降级，不写 `#[cfg(target_os)]`（[ADR 0001](adr/0001-five-platforms-capability-bitmap.md)）。加一个新端只改一个文件。
4. **接口稳定优于实现统一**：跨层契约优先落到 `contracts/`（Protobuf / JSON Schema），破坏性变更必须升版本。
5. **AI 是引擎系统**：模型调用统一经 L7 `ModelProvider` 边界，引擎对厂商零硬编码；Key 只存在于平台壳，永不进入引擎层、世界状态或仓库。

## 3. 十一层与 crate 对照

| 层 | 职责 | crate | 状态 | 关键接口（附录 B） |
|---|---|---|---|---|
| L0 | 跨层共享类型 | `crates/oriera-common` | ✅ | `Timestamp` / `FrameIndex` / `TokenUsage` / `ComputeGrant` |
| L1 | 平台抽象：图形/输入/存储/网络/XR 边界 | `crates/oriera-pal` | ✅ 骨架 | `QueryCapabilities` `CreateGfxBackend` `PollInputEvents` `OpenAssetStream` `RouteRemoteTask` |
| L2 | 核心系统：帧调度、算力分拨、确定性框架 | `crates/oriera-core` | ✅ 骨架 | `BeginFrame` `SubmitTask` `ReportBudget` `BudgetAllocator` |
| L3 | 对象模型：ECS、资源单例 | `crates/oriera-ecs` | ✅ 可用 | `Spawn` `Query` `Publish`(规划) `Serialize`(规划) |
| L4 | 资产管线：句柄注册表 | `crates/oriera-asset` | ✅ 骨架 | `ResolveAsset`(哈希即身份，规划) `Prefetch` `Evict` |
| L5 | 渲染内核：Pass 图、帧提交 | `crates/oriera-render` | ✅ 骨架+GPU 闭环* | `BuildFrameGraph` `AddPass` `SetQuality` `render_mesh`* |
| L5 | 高斯泼溅：EWA 投影、确定性前到背序、CPU 参考光栅 | `crates/oriera-splat` | ✅ 骨架（实验性，PLAN-splat v2 M0） | `decode_splat_ply`（L4 解码） `project` `depth_order` `rasterize` `SplatImage::fingerprint` |
| L6 | 子系统服务：物理/动画/音频/UI/网络同步 | `crates/oriera-subsys` | ✅ 物理（粒子 + `rigid`/`rigid3d` 刚体） | `StepPhysics` `MultibodySystem`(rigid3d multibody 关节臂) `HoldingCriterion`(TipRadius/Contact) `fingerprint`/`fingerprint_v2` `SampleAnimation`(规划) `SyncState`(规划) |
| L7 | AI 推理运行时：命令执行、端云路由 | `crates/oriera-infer` | ✅ 可用 | `SubmitInference` `ReportInferenceMetrics` |
| L8 | 生成式 AI：文生图 + 文生 3D + 文生视频 | `crates/oriera-genai` | ✅ 表达链路已通（RFC 0001 + demo 13，2028 做深） | `RequestGeneration` `ValidateAsset` |
| L9 | Agent 层：意图→命令（认知面） | `crates/oriera-agent` | ✅ 可用 | `InvokeTool`（v1 规则解析 + 认知系统）；`SpawnPrefab`（实体预制体，确定性声明序——Godot PackedScene 的 ECS 翻译，F1）；兼容层 re-export 具身面历史 API（ADR 0004） |
| L9 | 具身仿真运行时（ADR 0004 拆分） | `crates/oriera-embodied` | ✅ 可用 | `embodied_env`（`TableTopEnv` Gymnasium 契约/任务注册表/域随机化/相机）；`spec`（`SceneSpec`/`spec_hash`）；`urdf`+`meshres`（机器人导入）；`lerobot`（v2.1/v3 导出）；`eval`（`course@v1`/`paper@v1`）；`ik`（DLS）；`manifest`（run 溯源） |
| L10 | AR/VR 交互：多模态感知 | — | 📋 2027 H2 | 手势/眼动经 L1 `SpatialInputEvent` 进来 |
| L11 | 应用与 UGC 创作生态 | `platforms/*` + `examples/*` | ✅ 壳 | 同源编辑器（规划） |
| 横切 | 观测/安全/计量/调试 | 各层埋点 | 📋 | `MeasureGPU`（L5 已有雏形） |

当前 Cargo 依赖图（方向即合法方向）：

```
agent(L9 认知) → embodied(L9 仿真运行时, ADR 0004) → subsys(L6) → ecs(L3) → core(L2) → common(L0)
agent(L9) → infer(L7) → subsys(L6)（具身 API 经兼容层 re-export，ADR 0004）
          → genai(L8) → asset(L4) / ecs(L3) / core(L2)（ADR 0003：生成不依赖推理）
                        infer(L7) → ecs(L3)
wasm(L9 壳) → embodied(L9)（ADR 0004：不再穿透 agent 拖认知层）
render(L5) → ecs(L3) → core(L2)
splat(L5) → asset(L4)（PLY 解码归资产层；零第三方依赖，不依赖 render/ecs）
splat(L5, `splat-wgpu` feature) → render(L5)（仅消费相机；同层依赖先例 agent→embodied，ADR 0004）
pal(L1)：只被平台壳（platforms/*）引用，不被上层 crate 直接依赖
```

五端入口在 `platforms/`：`pc`（可运行）、`web`（Wasm 壳）；`ar_glasses`、`bci_reserved` 为规划项（目录尚未创建，bit 位已预留）。

## 4. 逐层详解

### L0 · oriera-common — 共享词汇

只放所有层都要用的**值类型**，不放行为。`Timestamp`（确定性时钟，不是墙钟）、`FrameIndex`、
以及 v0.2 新增的 `TokenUsage` 统一 token 计量词汇——L7 产出、L9 累计、上层展示，
全链路一个结构体，谁也不许自己另造轮子。

### L1 · oriera-pal — 平台差异的唯一入口

五端（PC/移动/Web/AR/BCI 预留）的能力差异被压缩成一张 `Capabilities` 位图。
上层问「有没有 NEURAL_RENDER_3DGS？」而不是「是不是 visionOS？」。
v0.2 落地 `route(caps, RouteHint) -> ExecutionSite`：端云推理路由在 PAL 一次决策——
隐私敏感钉死端侧、NPU 吃小负载、紧延迟走边缘。换端不换代码的根据就在这层。

### L2 · oriera-core — 调度器与确定性

`Scheduler<World>` 每帧 `tick(&mut world)`，把世界显式传给每个 System。
v0.2 接入 `system_timings()`（逐系统毫秒观测）与 `with_budget_ns()`（平台预算曲线：
手机 30fps、AR 眼镜 90fps 各自的帧预算），AI 系统的认知/导演 tick 限时不超过帧预算。

### L3 · oriera-ecs — 对象模型

SoA 布局 ECS，20 万粒子参考实现在浏览器跑（demo 02）。
generation 句柄防悬垂引用。跨系统共享状态走 resource 单例（如 `Gravity`：L9 解析 → L7 写入 → L6 读取）。
v0.2 润滑剂：零分配单列迭代（去掉每系统每帧一次 O(n) 克隆）、`resource_or_default` 自愈
（忘注册资源不再 panic）、`write_event` / `take_events` 类型化事件总线（1024 封顶）。

### L4 · oriera-asset — 句柄注册表

哈希即身份：资产按内容寻址，`ResourceHandle` 稳定跨存档。
v0.2 落地 `insert_named` / `get_named` / `unload`：命名资产热替换，同句柄原位换芯——
这是 L8 生成式管线（2028）把 AI 生成资产灌进世界的落地点。

### L5 · oriera-render — 渲染内核

Pass 图（BuildFrameGraph/AddPass）为目标形态，当前 PC 端为统计式渲染系统。
v0.2 落地 `Quality` + `observe_frame` 迟滞调速器：AI 一重就掉帧时自动降 draw batching（1/4/16），
有余力再缓慢升档——上层以为画质是恒定的。能力位图已含 3DGS 神经渲染位（未来混合几何）。经典 EWA 泼溅（`oriera-splat`，见下）是确定性传统光栅化，
**不读**此位——该位留给未来的神经精修 Pass（ADR 0002 的 passthrough 语义）。

v0.3 落地 **wgpu 最小无头闭环**（feature `render-wgpu`，按 GOVERNANCE 默认关）：
`gpu::HeadlessRenderer` 走 真 GPU pass（WGSL 顶点渐变着色）→ 索引绘制 → 像素回读，
配合 L4 `oriera_asset::glb::decode_glb_mesh`（GLB POSITION/索引最小解码，深校验）打通
「GLB 文件 → 真实 mesh → GPU 光栅化 → BMP 出图」全链路。
v0.3 追加顶点材质光照：GLB 的 NORMAL + baseColorFactor 解码后 CPU 烘焙
兰伯特顶点色（无材质走旧位置渐变，公式不变），`render_mesh_shaded` 承接。
2026-11 落地 **#152 首刀：相机矩阵 + 深度缓冲**——`camera::Camera`（GL 式
y-up/-z-forward，**WebGPU NDC z∈[0,1]**，列向量行主序，WGSL 上传时转置为列主序）
+ `gpu::HeadlessRenderer::render_mesh_view`：世界坐标网格经 MVP uniform 渲染，
`Depth32Float` 深度测试（Less+write）——**近面遮蔽远面与绘制顺序无关**（GPU 实测）。
相机模块是纯数学，不受 feature 门控；这是原生 glTF（本 issue 后续刀次）与 splat GPU
路径（PLAN-splat M1）的共享底座。
2026-11 落地 **#152 第二刀：原生 glTF 场景加载与纹理渲染**——L4 `decode_glb_scene`
（全 mesh×primitive + 节点图 TRS/matrix + 材质摊位含 PNG 纹理字节，真实文件的
accessor byteOffset/交错布局支持；Duck 为 SCEA 许可不入库，验收用 CC0 WaterBottle）
+ L5 `render_scene_views`（同 pass 逐 primitive 绘制，纹理管线 sRGB 采样 × 兰伯特，
深度测试与首刀共享）。demo 05 切 three 移除留第三刀（走 M2 的 JS 镜像+金样路线）。注意：windows-gnu（MinGW）工具链下 wgpu DX12/Vulkan
适配器枚举实测崩溃，默认后端被 cfg 钳制为 GL（`WGPU_BACKEND` 环境变量可覆盖）；
MSVC 工具链不受此限。

2026-11 落地**高斯泼溅 CPU 参考内核**（独立 crate `oriera-splat`，PLAN-splat v2 M0，**实验性**，
随下一个 minor 发布）：L4 `oriera_asset::ply::decode_splat_ply` 按属性名解码标准 3DGS PLY——字段存激活值，
语义对齐 Khronos 已批准的 KHR_gaussian_splatting（线性尺度、线性不透明度、xyzw 单位四元数），
先验长度后分配，压缩 PLY 等不支持的布局一律显式报错 → `project`（EWA 局部仿射投影，剔除即 compaction，
包围框取 3σ 椭圆的轴对齐外接矩形）→ `depth_order`（深度位型与下标打包成 u64 键，任何排序算法都得到同一全序）
→ `rasterize`（前到背透射率累积，受 `RasterBudget` 约束，超限返回 `OverBudget`，绝不静默降级）。
同一输入逐字节同输出，`SplatImage::fingerprint` 与 `World::state_hash` 同种子，可进回放审计。
真扫描实测出图见 [docs/media/splat-cpu-dragon.png](media/splat-cpu-dragon.png)（CC0-1.0 样本
spatialdragon-3dgs，10,330 splat，SH 2 阶，属性顺序非 INRIA 布局；960×540 出图 92 ms）。
2026-11 落地 **GPU 路径**（PLAN-splat M1，feature `splat-wgpu` 默认关）：确定性数学
（投影/排序）仍在 CPU，GPU 只做实例化四边形展开 + 后到前 over 混合（**fp32 目标**，
unorm8/fp16 的逐层混合舍入会在热点像素累积超差——实测踩过）；相机消费 #152 首刀的
`camera::Camera`（`SplatCamera::from_view` 换算，闭式单测钉死），同层依赖先例
agent→embodied（ADR 0004）。与 CPU 参考按评审门槛互证：99.9% 像素每通道 ≤2/255 且
RMS ≤1/255、倾斜 splat 交叉项 ≤2、单 splat 逐位一致（本机 Intel GL 实测）。
数学出处为 Kerbl et al. 2023 与 Zwicker et al. 2001；排序键打包借鉴 World Labs Spark/Radix（MIT）公开文档，
实现为原创。CPU 路径非实时（实测见 [STABILITY.md](STABILITY.md) §三）。

### L6 · oriera-subsys — 子系统服务

物理先行（`StepPhysics`，边界反弹积分），动画/音频/网络同步在 ROADMAP。
v0.2 落地 `TimeScale` 仿真节流资源：物理步长按它缩放，钳制 [0,8] 防幻觉值炸世界；
AI NPC 的 `wave/dance` 动作选择由 L9 认知输出直接驱动。

v0.11 具身线给 L6 加了两个刚体后端（feature 门控，与粒子积分器同契约：
Gravity/TimeScale/边界，垂直版二选一、回放中途不许换）：`rigid`（rapier2d）与
`rigid3d`（rapier3d **multibody 约简坐标**关节臂，纯 Rust/Apache-2.0/wasm32
一等公民；Jolt 因无 reduced-coordinate articulation 评估后不做，见
[ROADMAP](../ROADMAP.md)）。入口 `MultibodySystem::from_robot(RobotSpec, Scene3D)`
（真 fixture 走 `from_robot_fixtures`）；抓取标签 `HoldingCriterion` 两档：
`TipRadius`（默认启发式）与 `Contact`（逐 tick 法向力判据，opt-in）。
这是「层配置」旋钮的首个实装（[VERTICALS.md](VERTICALS.md) §0）。

### L7 · oriera-infer — 模型生命周期唯一入口

**厂商零硬编码**的 `ModelProvider` trait + `ProviderConfig/ProviderKind/create_provider`：
zhipu / openai-compat / deepseek / moonshot / groq / openrouter / anthropic / rule（离线规则）八家。
浏览器侧 `engine/kernel.js :: createProvider(cfg) + PROVIDER_PRESETS` 与 Rust 一一对照。
v0.2 落地帧预算命令队列（`deferred` 计数，命令洪峰摊到后续帧，消灭帧尖刺）
与 `UsageMeter` / `last_usage()` 计量；20s 超时防挂死。
v0.3 落地**异步推理作业**（RFC 0002 v0.5 第一步）：`InferJobs` 把阻塞的模型调用
搬到后台线程（提交即返回、逐帧 try_recv 收割），认知/导演系统 `with_async()`
切换后帧循环不再等网络——真 LLM 的 1~5 秒延迟从「卡住的帧」变成「晚一拍落地的对话」；
`complete_stream()` 流式表面（RFC-3）就位，默认单块诚实表示「此后端不支持流式」。
v0.3 同轮落地**工具调用与真流式**（RFC-3 P0 全清）：`complete_with_tools` +
`ToolDef/ToolCall/ToolOutcome` 工具词表（HttpProvider 走 OpenAI `tools` /
Anthropic `input_schema` 双方言 wire；RuleProvider 用 `spawn {"count":N}` 标记
确定性模拟，离线可测）；HttpProvider `complete_stream` 原生 SSE 增量流
（逐 delta 分块、终帧计量 usage，双方言事件）。
**Key 只存在平台壳**（浏览器 localStorage / 环境变量），永不入仓。
v0.7（DeepSeek 升级第一批）：`TokenUsage.cached` 前缀缓存计量 +
`CostMeter` 成本账本（缓存折扣显示成钱）+ `response_format: json_schema`
（非法 schema 零网络 fail-fast）+ `ReasoningEffort` 经 deepseek-recipe-core
规范枚举（MIT，仅 native-http feature，wasm 零增量）+ 预设行 caps/price
能力与牌价元数据；smoke 加缓存双连发验收腿。生态位：**OriEra 不做推理
引擎，做推理引擎的好客户端**（见 AI-NATIVE.md v0.7 节）。

### L8 · oriera-genai — 生成式 AI（表达链路已通，ADR 0003 + RFC 0001）

**生成是独立边界，不走 L7 推理**（ADR 0003）：`GenerationProvider` trait + `MockProvider`
+ `validate_asset` 校验；CogView（文生图，OpenAI images 方言）/ Tripo（文生 3D）/ Rodin（Hyper3D 文生 3D）
Seedance（文生视频）协议适配器随 `providers` feature 门控
（只定义 JSON shape，零 HTTP client——transport 由壳层注入，wasm 体积可控）。
`text_to_image` 种类是 RFC 0001 的加法式契约扩展——小 3D 游戏的表达主力
（天空/地面/海报/精灵图）。占位降级按种类匹配格式：失败的照片落
真实可解码的 1×1 PNG，失败的网格落**可见单三角 GLB**（ADR 0003 准则 3 升级：
断电也有看得见的占位，不再是空场景）。
v0.3 生成→渲染全链闭环：MockProvider 生成 → `validate_asset` 守门 → L4 命名落地
→ `glb::decode_glb_mesh` → GPU 光栅化出图，一条命令自证
（`cargo run -p oriera-render --example glb_headless --features render-wgpu -- --from-l8 out.bmp`）。
引擎环集成：`GenAiSystem` 限量轮询生成任务 → 校验 → `PendingSwaps` + `AssetLanded` 事件，
壳层 `hot_swap::apply_pending` 把结果落入 L4 命名资产（demo：`cargo run -p oriera-genai --example hotswap`）。
浏览器侧镜像 `examples/13-genai-gallery/engine/genai.js`（边界词汇一一对照，
fetch Promise settling 取代轮询节拍）+ `ArtDirectorSystem` 美术导演（提示词形状归引擎）
+ demo 13 全链实跑。契约落在 `contracts/oriera/genai/v1`。深管线（六段式生成→修复）2028 做。

### L9 · oriera-agent — 意图到命令

两个入口形态：
- **规则解析**：`oriera_agent::parse("spawn 50 particles")` → `Command::Spawn(50)` → L7 执行
  （言灵世界的通道）；
- **认知系统**：`CognitionSystem` / `DirectorSystem` / `MemoryStore`（检索式长期记忆）
  全部是 `System<World>` 挂 L2 调度器（咖啡馆的形态），headless 验证
  `cargo run -p oriera-agent --example cafe`；
- **工具桥**（v0.3，feature `json`）：`EngineTools::definitions()` 把引擎命令词表
  暴露给任意厂商模型，`dispatch` 把模型的工具调用翻译成 `Command` 提交 L7——
  模型从「会说」到「会做」，坏参数返回自愈说明永不 panic。
  `run_tool_agent` 把循环装成运行时：完成 → 调工具 → 结算一帧 → 结果回话 →
  再完成，直到纯文本收敛或超轮显式报错（L7 双方言 wire 携带 tool_calls
  历史 / Tool 帧满足 OpenAI/Anthropic 协议）。离线闭环演示：
  `cargo run -p oriera-agent --example tool_loop --features json`。

v0.2 润滑剂：认知/导演自愈启动（裸世界 boot 是测试契约）+ 否定语义解析
（「别清空」先于意图判断，不被执行成清空）。

v0.11 具身线在 L9 加了第二个入口形态——**具身训练场**（详见 §6）：
`SceneSpec`（一份 JSON，`spec_hash` 规范化哈希 = 实验身份）× `RobotSpec`
（N 自由度，URDF 直读）× `Task` trait 注册表（`reach/pick_place/push/stack/
bin_pick/drawer`，判据修订只走新 id）× `EvalProtocol`（`course@v1` 12 集
90% 硬闸 / `paper@v1` ≥100 集只记录）× `lerobot` 导出（LeRobotDataset
v2.1/v3.0 + npz 裸出口）× `run_manifest.json` 全出处。

### L10 — 多模态交互（规划）

手势/眼动/语音经 L1 `SpatialInputEvent` 语义化后转自然语言意图进 L9。2027 H2。

### L11 — 应用与 UGC

`platforms/pc` 是全量接线示范（平台预算、画质调速、逐系统耗时、资产热替换、TimeScale）。
`examples/01–16` 是分层演示：每个 demo 只动它那一层，浏览器直接玩（见首页 index.html；
14 号 `frame-hunter/` 在仓库根目录同源发行）。

## 5. 一帧的数据流

### PC 端（`cargo run -p oriera-pc`）

```
应用壳 (platforms/pc)
  │ 持有 World(L3) + AssetStore(L4)，构建 Scheduler<World>(L2)
  ▼ 每帧 sched.tick(&mut world)   ← tick 顶部：BudgetAllocator 先分拨本帧份额
InferSystem(L7)   执行命令队列（预算 ×throttle）→ 生成/清除实体、写 Gravity resource
GenAiSystem(L8)   限量轮询生成任务 → 校验 → PendingSwaps + AssetLanded 事件
                  （壳层随后 hot_swap::apply_pending 把结果落入 L4 命名资产）
PhysicsSystem(L6) for_each2_mut::<Position,Velocity> 积分 + 边界反弹
RenderSystem(L5)  统计可见 Position → draw_calls（真 GPU 光栅化已在 render-wgpu feature 闭环，Pass 图在 ROADMAP）
  ▼
调度器回报 frame_ms + system_timings()（L2 观测埋点）
```

### AI 原生帧（demo 11 隅角咖啡馆）

```
玩家输入 ─→ L9 CognitionSystem（限时不超帧预算）
          │   ├─ MemoryStore 检索长期记忆（localStorage: aicafe.mem.v1）
          │   └─ 组 prompt → L7 ModelProvider（厂商自选）
          ▼
L7 帧预算队列摊平命令洪峰 → UsageMeter 计量
          ▼
L9 DirectorSystem 调度 NPC 行为 → L6 动作/物理 → L5 画质迟滞调速兜底
```

自然语言入口在 L9：`parse("spawn 50 particles")` → `Command::Spawn(50)` →
`InferSystem::submit()` → 下一帧生效。整条链路没有任何全局变量，
回放确定性由 L3 generation 句柄与 L7 的固定种子 LCG 保证。

## 6. 具身智能仿真层（world as benchmark）

> 一句话：**不重造物理引擎、不重造数据集格式**——rapier3d multibody 当训练场，
> LeRobotDataset 当数据出口；「一份 JSON = 一次实验的身份（`spec_hash`），
> 每集一张逐位回执（指纹）」。管线全景见
> [EMBODIED-PIPELINES.md](EMBODIED-PIPELINES.md)，实验操作见
> [LAB-GUIDE.md](LAB-GUIDE.md)，与 ManiSkill/Isaac 的保真对照见
> [POSITIONING.md](POSITIONING.md)。

### 6.1 两个层位

- **L6 `rigid3d`**（`crates/oriera-subsys`，feature 门控）：rapier3d
  multibody 约简坐标关节臂。判据面：抓取标签 `HoldingCriterion`
  （`TipRadius` 启发式 / `Contact` 逐 tick 法向力，lab 变体 12 集对照零翻转
  后仍在 opt-in）；身份面：`fingerprint()`（v1，状态 ⊕ kp/kd ⊕ mass_scale）、
  `fingerprint_v2()`（状态 ⊕ `robot_identity_hash` ⊕ spec_hash——机器人
  N 关节 kp/kd/armature/limits/axis/anchor、逐 link 几何与质量、夹爪摘要全部
  入 hash）。
- **L9 `embodied_env`**（`crates/oriera-embodied`，ADR 0004 拆分；umbrella + scene/tasks/env/cameras 四子模块）：
  `SceneSpec` → `spec_hash()` 规范化哈希（`meta` 不入 hash、字段拼错即报错，
  改一个字段 = 新实验）→ `TableTopEnv`；任务 = `Task` trait 实现 +
  `task_registry` 按 id 注册（`pick_place@v1` 冻结、修订走 `@v2`）；
  评测 = `EvalProtocol` + `evaluate()`（种子带/退化重抽/闸全在这一处，
  CLI / Python / wasm 三入口同一个口径）；导出 = LeRobotDataset v2.1/v3.0
  全文件树 + npz 裸出口 + `run_manifest.json`（git SHA+dirty、Cargo.lock
  hash、rustc/rapier、features、OS/arch、命令行——半年后复现就靠它）。

### 6.2 一套物理，四个入口

同一份 `TableTopEnv` Rust 代码（物理、奖励、判定、随机化、观测、指纹全在
Rust 侧），四个壳（架构图与逐位验收结论见
[WASM-RENDERER.md](WASM-RENDERER.md)）：

```
                    TableTopEnv（crates/oriera-embodied，一份 Rust 物理）
                    /            |             |              \
        浏览器独立页          Studio /sim       Rust CLI         Python/Gymnasium
  examples/15-embodied-sim   postMessage 换装   embodied_sim_    PyO3 wheel
  oriera-wasm→wasm+three.js  录制+一键导出      collect/baseline (oriera_sim，
  六预设卡片墙，点卡片换装   （发版后可用）     指纹口径源、金标  SB3/lerobot-train）
                                                采集、评测协议
```

浏览器页的物理**不是 JS 镜像**——kernel.js 退役为纯渲染器（three.js 每帧读
`sim.poses()` 搬 mesh），物理来自编译到 wasm32 的同一份代码。契约口径：
**同目标逐位一致（浏览器内双跑逐位），native↔wasm 统计一致**（libm 末位
1 ulp；三平台指纹矩阵 CI 出统计报告）。

### 6.3 指纹契约（可审计训练场的机械凭据）

- **每集三口径**：`episode_state_hashes`（纯状态）/ `episode_fingerprints_v1`
  （旧回执口径，旧键名读兼容）/ `episode_fingerprints_v2`（完整身份）——
  写进 receipt（`lerobot-v21/meta/oriera_receipt.json`）与 `eval_report.json`。
- **金标**：`contracts/golden/embodied_fingerprints.v2.json`（12 集三口径）。
  改物理必须走 `physics-behavior-change` 流程重锚（
  `scripts/reanchor_golden.py` 固化流程），金标漂移 = CI 红。
- **防抄袭**：`python -I scripts/fingerprint_dupcheck.py <runA> <runB>`
  读两个 run 的逐集指纹对表——全部相同 = 指纹级重复（同一份实验冒充两次
  采集的实锤），用法与判定见 [EMBODIED-PIPELINES.md](EMBODIED-PIPELINES.md) §7。
- **spec 库治理**：`crates/oriera-embodied/specs/official/`（官方六预设金标，
  改字段 = 换 hash = 新实验身份，旧身份留 `results/reference/<hash8>/` 档案）+
  `specs/lab/`（`extends` 只写差异）+ `INDEX.json`（同名异 hash 报错）。

### 6.4 官方预设与基线

六个官方预设（桌面 4 轴抓放 / Panda 7 轴 / 叠块 / 料箱 / 抽屉 / 移动底盘
`mobile-arm-4dof`）各带任务专属基线闸（`eval::baseline_gate`：桌面 ≥90%、
Panda/叠块 ≥75%、料箱/抽屉 ≥60%），逐项数字与出处见
[POSITIONING.md](POSITIONING.md) §2 保真卡。

## 7. v0.2 润滑剂契约（2026-10-15「全层润滑剂」PR #32/#35）

每层新增的加法式契约（详见 [AI-NATIVE.md](AI-NATIVE.md) 末节与各层 manifest）：

- L0 `TokenUsage`：统一 token 计量词汇
- L1 `route(caps, RouteHint)`：端云路由一次决策
- L2 `system_timings()` / `with_budget_ns()`：逐系统耗时 + 平台预算曲线
- L3 `write_event` / `take_events` / `resource_or_default`：类型化事件、自愈资源
- L4 `insert_named` / `get_named` / `unload`：命名资产热替换
- L5 `Quality` + `observe_frame`：迟滞画质调速
- L6 `TimeScale`：仿真节流（钳制 [0,8]）
- L7 帧预算命令队列（`deferred`）+ `UsageMeter` / `last_usage()`
- L9 认知/导演自愈启动 + 否定语义解析

一句话：**AI 的输出是概率性的、突发的、贵的；引擎的职责是把这些毛刺吸收掉——
计量、限流、降级、自愈，让上层以为世界永远是平滑的。**

### v0.4 算力分拨契约（2026-10-16「先分后花」）

润滑剂是每层各自刹车；算力分拨升级为**一个账本 + 中央仲裁 + 各层拨杆**——
帧预算先分后花，浪费在进帧之前消掉。完整地图见 [COMPUTE-BUDGET.md](COMPUTE-BUDGET.md)。

- L0 `ComputeGrant`/`Grants`：分拨词汇；查无此名 = 未纳管 = 满速
- L2 `BudgetAllocator`：EWMA 成本 → 带保底的份额 → 迟滞节流（快降慢升），
  经 `FrameCtx::grants` 借用下发（零分配）；`AllocatorConfig::off()` 逐位回退
- L7 自适应队列：命令预算 ×throttle（下限 1 条/帧）+ submit 时可证安全合并
- L6 空转跳过：冻结/完全静止且无越界实体的积分是恒等无操作，整趟跳过（位级等价）
- L9 注意力节拍器：压力下说话间隔 ×(1/throttle)（封顶 8×），
  `WakeEvent` 玩家一碰世界立即回满速
- L8 `GenAiLimits` 拨杆感知：in-flight 上限与轮询间隔随压力缩放（缓不降级）

### v0.7 缓存·成本·结构化输出契约（2026-10-21「好客户端」）

- L0 `TokenUsage.cached`：缓存命中 prompt tokens（⊆ prompt，`billed_prompt()` 钳制谎报）
- L7 `CostMeter`：µ 货币成本账本，`saved_micro` 显示缓存省钱；跨币种不求和
- L7 `response_format: json_schema` + `ReasoningEffort`（recipe-core 规范枚举）
- L7 `ProviderPreset.caps/price`：14 行能力与牌价登记（未核实 = 留空，不发明厂商事实）
- L9 **稳定前缀惯例**：system 对 NPC 恒定、易变走 user（Rust+JS 双侧，字节稳定是测试契约）+ 注入用例集
- 完整地图：AI-NATIVE.md v0.7 节 / OPEN-MODEL-MATRIX.md §0.5-0.6

### v0.8 韧性与可复现契约（2026-10-21「永不下线」）

- L0 `DeterminismLevel`：strict / pinned / recorded 三档回放承诺词汇
- L7 `health.rs`：熔断器（连续 3 败开闸、30s 冷却半开探测；5xx/429/408/Network/Timeout/Parse 计入，4xx 配置错不计）+ `FallbackProvider` 降级链（云端→本地→rule，「咖啡馆永不下线」）+ `degradation_note` 降级可见
- L7 `InferenceLedger` + `prompt_fingerprint`/`response_fingerprint`：#67 哈希族的「双指纹」审计——世界状态哈希管语义路径，台账管模型路径；`DeterminismLevel` 从 provider 自报
- L9 `fit_budget`（Rust+JS 对镜）：易变 user 消息字符预算有界化，最旧对话先删、头部必保、同输入同裁剪（B4；#70 JSON 持久化上游已落地）
- L7 SSE 每行 1MB / 整流 16MB 帽（安全审计 P2 回归钉）；tools 数值参数量程设界（1e30→inf→物理 NaN 的路被堵死）
- 已知边界：熔断失败率与 L2 `BudgetAllocator` 的 EWMA 联动留作 follow-up（信号已由 `health_report()` 暴露）；完整地图见 AI-NATIVE.md v0.8 节

## 8. 治理：LayerManifest 与架构门禁（T3 §3.8.3）

每层在 `manifests/` 声明机器可读的清单（L0–L9 共 10 份 YAML，含 L08-genai）：
对外提供什么契约（provides）、依赖哪些契约（requires）、性能预算（budgets）、降级策略（degrade）。
CI 门禁分两步走——**静态部分已落地**（`scripts/check_manifests.py`，随每个 PR 运行），性能部分规划中：

- ✅ 某层 `requires` 了无人提供、或未声明 stable 的契约 → CI 失败（已落地）；
- ✅ `provides` 的 `proto/` 契约引用文件缺失、`rust:`/`proto:` 之外的引用形式 → CI 失败（已落地）；
- 📋 `provides` 的契约发生不兼容变更未升主版本 → CI 失败（规划）；
- 📋 实测 P99 超预算 20% → 发布进入评审（规划；CI 已跑 `cargo bench -p oriera-ecs` 留档基线数据）。

新增一层时：建 crate → 写 `manifests/Lxx-*.yaml` → 在 `contracts/` 落契约 → 在上表加一行 → CI 接入。

## 9. 目录地图

```
crates/          L0–L9 各层实现（见上表，含 L8 oriera-genai 与 L5 实验性 oriera-splat）
platforms/       五端入口：pc / web（ar_glasses、bci_reserved 规划中，目录未建）
examples/        浏览器可玩的分层演示（01–16 + _template，见首页 index.html；
                 15 号具身仿真台是旗舰，含 vendor/oriera 的 wasm 内核）
frame-hunter/    14 号影游 demo（跑酷答题 + BKT 知识追踪，Pages 同步发行）
specs/           具身实验身份库（官方金标 official/ + lab 覆盖层 + INDEX.json，
                 位于 crates/oriera-embodied/specs/）
contracts/       跨语言接口契约（Protobuf：pal/v1、genai/v1）+ 具身指纹金标 golden/
manifests/       每层的 LayerManifest（provides/requires/budgets/degrade）
docs/            架构文档、ADR、AI 原生图、性能记录、release notes
assets/          logo、hero GIF 等品牌资产
```

## 10. 测试、CI 与性能

- 全仓测试数以 CI 实测为准（2026-10-25 口径：workspace 238 + genai `providers` 59 + agent `json` 60 + subsys `rigid` 15 + infer `native-http` 97 + kernel node 35，含 v0.8 韧性批次、#98 golden fixture、#109/#110 评审批次与 #111-#114 生成供应商批次；分项含少量 doctest；此后 PLAN-splat M0 新增 41 项：asset `ply` 19 + splat 单元 21 + 金样 1；
#152 首刀新增 camera 10 项（另有 2 项 GPU 实测 `#[ignore]`，本机跑绿）），CI 在 Linux 上跑 fmt / build / test / clippy（`-D warnings`）。
- 每个版本在 `docs/release-notes/` 落一份双语说明（v0.1.0 至 v0.12.0 共 14 份；
  `scripts/check_release_versions.py` 守版本号同步，发版前必跑）。
- 基准怎么跑、当前参考数据见 [PERF.md](PERF.md)。

## 11. 常见问题

**为什么不用 Unity/Unreal 加个 AI 插件？**
因为插件只能挂在应用层，AI 进不了调度、预算、降级这些引擎内脏。OriEra 的判断是：
AI 时代的引擎差异在「引擎怎么吸收 AI 的不确定性」，这必须改架构，不是加 SDK。

**为什么主语言是 Rust？**
实时层（L1–L7、L9）要确定性、零成本抽象和 Wasm 一等公民；Rust 三者都给。
JS 侧保留在 demo 与未来编辑器/SDK（pnpm workspace 届时引入）。

**和 LangChain / 游戏 AI 框架什么关系？**
L9 的 Agent 是引擎系统（挂调度器、吃帧预算、有世界状态），不是编排骨子。
工具调用走 MCP 协议（规划），认知循环是引擎原语。

**我不会 Rust，能参与吗？**
能。文档、翻译、demo（纯 HTML/JS）、体验反馈都是贡献。见 [开源参与指南](OPEN_SOURCE_GUIDE.md)。

## 12. 延伸阅读

- [AI-NATIVE.md](AI-NATIVE.md)：十一层逐层 AI 内嵌图 + 润滑剂详解
- [COMPUTE-BUDGET.md](COMPUTE-BUDGET.md)：算力分拨管线地图（五个微模型 + 植入模型三步）
- [EMBODIED-PIPELINES.md](EMBODIED-PIPELINES.md)：具身仿真管线全景（生态调研 × L6/L9 接入 × 数据出口 × 指纹防抄袭）
- [LAB-GUIDE.md](LAB-GUIDE.md)：具身台学生实验指导书（跑基线 → 写策略 → 交报告）
- [POSITIONING.md](POSITIONING.md)：具身台定位与保真卡（六预设 × MuJoCo 交叉验证）
- [WASM-RENDERER.md](WASM-RENDERER.md)：一套物理四个入口的 wasm 化架构与逐位验收
- [STUDIO-PIPELINE.md](STUDIO-PIPELINE.md)：Studio 六区 ↔ 十一层衔接管线（P1 建游 / P2 模型 / P3 运行时 / P4 观测）
- [VERTICALS.md](VERTICALS.md)：AI 产业八赛道垂直版矩阵 + 接模型/接 agent 指南 + 三轮测试计划
- [OPEN-MODEL-MATRIX.md](OPEN-MODEL-MATRIX.md)：八赛道开源底座 × 大厂预留 API 矩阵（L7 PROVIDER_PRESETS 13 行）
- [ADR 0001](adr/0001-five-platforms-capability-bitmap.md)：五端一等公民与能力位图
- [ADR 0002](adr/0002-neural-rendering-runtime-invariants.md)：神经渲染运行时不变量
- [ADR 0003](adr/0003-l8-native-generative-ai.md)：L8 原生生成式 AI 边界
- [ROADMAP.md](../ROADMAP.md)：L8/L10/L11 的落地节奏（三阶段演化到 2031+）
- [PERF.md](PERF.md)：基准怎么跑、当前参考数据
- 书技术篇：T3 总览 / T4–T14 各层 / T15 横切 / T17 测试与基准（[BOOK_MAP.md](BOOK_MAP.md)）
