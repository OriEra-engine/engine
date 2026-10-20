# OriEra 技术架构文档

> 一句话理解：**十一层是责任边界，接口契约是连接器，横切系统是全局仪表盘与刹车。**
> （《OriEra 世界引擎》技术篇 T3）

本文是仓库的完整架构文档，对应 **v0.3.0**（2026-10）的 main 分支。
定位：读完这一篇，你应该能回答三个问题——每层是什么、代码在哪、为什么这样切。
层职责与接口契约的完整定义见书技术篇 T4–T15 与附录 B；书 ↔ 仓库对照见 [BOOK_MAP.md](BOOK_MAP.md)。

---

## 1. 这是什么

OriEra 不是又一个游戏引擎。它是一个 **AI 原生、虚实融合、可自演化的开放数字世界运行时**：

- **AI 原生**：AI 是引擎系统，不是应用代码。认知、长期记忆、AI 导演都是挂在调度器上的 `System`，应用层零 AI 代码（见 [AI-NATIVE.md](AI-NATIVE.md)）。
- **虚实融合**：PC / 手机 / 网页 / AR 眼镜五端一等公民，脑机接口 v1 只留 bit 位不接硬件。
- **可自演化**：用户用自然语言创造与改写世界内容（demo 10「言灵世界」、demo 11「隅角咖啡馆」两个范式）。

在线体验：**https://oriera-engine.github.io/engine/**（11 个浏览器可玩 demo，零安装）。

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
| L6 | 子系统服务：物理/动画/音频/UI/网络同步 | `crates/oriera-subsys` | ✅ 物理 | `StepPhysics` `SampleAnimation`(规划) `SyncState`(规划) |
| L7 | AI 推理运行时：命令执行、端云路由 | `crates/oriera-infer` | ✅ 可用 | `SubmitInference` `ReportInferenceMetrics` |
| L8 | 生成式 AI：文生图 + 文生 3D + 文生视频 | `crates/oriera-genai` | ✅ 表达链路已通（RFC 0001 + demo 13，2028 做深） | `RequestGeneration` `ValidateAsset` |
| L9 | Agent 层：意图→命令 | `crates/oriera-agent` | ✅ 可用 | `InvokeTool`（v1 规则解析 + 认知系统） |
| L10 | AR/VR 交互：多模态感知 | — | 📋 2027 H2 | 手势/眼动经 L1 `SpatialInputEvent` 进来 |
| L11 | 应用与 UGC 创作生态 | `platforms/*` + `examples/*` | ✅ 壳 | 同源编辑器（规划） |
| 横切 | 观测/安全/计量/调试 | 各层埋点 | 📋 | `MeasureGPU`（L5 已有雏形） |

当前 Cargo 依赖图（方向即合法方向）：

```
agent(L9) → infer(L7) → subsys(L6) → ecs(L3) → core(L2) → common(L0)
          → genai(L8) → asset(L4) / ecs(L3) / core(L2)（ADR 0003：生成不依赖推理）
                        infer(L7) → ecs(L3)
render(L5) → ecs(L3) → core(L2)
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
有余力再缓慢升档——上层以为画质是恒定的。能力位图已含 3DGS 神经渲染位（未来混合几何）。

v0.3 落地 **wgpu 最小无头闭环**（feature `render-wgpu`，按 GOVERNANCE 默认关）：
`gpu::HeadlessRenderer` 走 真 GPU pass（WGSL 顶点渐变着色）→ 索引绘制 → 像素回读，
配合 L4 `oriera_asset::glb::decode_glb_mesh`（GLB POSITION/索引最小解码，深校验）打通
「GLB 文件 → 真实 mesh → GPU 光栅化 → BMP 出图」全链路。
注意：windows-gnu（MinGW）工具链下 wgpu DX12/Vulkan 适配器枚举实测崩溃，
默认后端被 cfg 钳制为 GL（`WGPU_BACKEND` 环境变量可覆盖）；MSVC 工具链不受此限。

### L6 · oriera-subsys — 子系统服务

物理先行（`StepPhysics`，边界反弹积分），动画/音频/网络同步在 ROADMAP。
v0.2 落地 `TimeScale` 仿真节流资源：物理步长按它缩放，钳制 [0,8] 防幻觉值炸世界；
AI NPC 的 `wave/dance` 动作选择由 L9 认知输出直接驱动。

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

### L8 · oriera-genai — 生成式 AI（表达链路已通，ADR 0003 + RFC 0001）

**生成是独立边界，不走 L7 推理**（ADR 0003）：`GenerationProvider` trait + `MockProvider`
+ `validate_asset` 校验；CogView（文生图，OpenAI images 方言）/ Tripo（文生 3D）/
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
  模型从「会说」到「会做」，坏参数返回自愈说明永不 panic。离线闭环演示：
  `cargo run -p oriera-agent --example tool_loop --features json`。

v0.2 润滑剂：认知/导演自愈启动（裸世界 boot 是测试契约）+ 否定语义解析
（「别清空」先于意图判断，不被执行成清空）。

### L10 — 多模态交互（规划）

手势/眼动/语音经 L1 `SpatialInputEvent` 语义化后转自然语言意图进 L9。2027 H2。

### L11 — 应用与 UGC

`platforms/pc` 是全量接线示范（平台预算、画质调速、逐系统耗时、资产热替换、TimeScale）。
`examples/01–11` 是分层演示：每个 demo 只动它那一层，浏览器直接玩（见首页 index.html）。

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

## 6. v0.2 润滑剂契约（2026-10-15「全层润滑剂」PR #32/#35）

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

## 7. 治理：LayerManifest 与架构门禁（T3 §3.8.3）

每层在 `manifests/` 声明机器可读的清单（L0–L9 共 10 份 YAML，含 L08-genai）：
对外提供什么契约（provides）、依赖哪些契约（requires）、性能预算（budgets）、降级策略（degrade）。
CI 门禁分两步走——**静态部分已落地**（`scripts/check_manifests.py`，随每个 PR 运行），性能部分规划中：

- ✅ 某层 `requires` 了无人提供、或未声明 stable 的契约 → CI 失败（已落地）；
- ✅ `provides` 的 `proto/` 契约引用文件缺失、`rust:`/`proto:` 之外的引用形式 → CI 失败（已落地）；
- 📋 `provides` 的契约发生不兼容变更未升主版本 → CI 失败（规划）；
- 📋 实测 P99 超预算 20% → 发布进入评审（规划；CI 已跑 `cargo bench -p oriera-ecs` 留档基线数据）。

新增一层时：建 crate → 写 `manifests/Lxx-*.yaml` → 在 `contracts/` 落契约 → 在上表加一行 → CI 接入。

## 8. 目录地图

```
crates/          L0–L9 各层实现（见上表，含 L8 oriera-genai）
platforms/       五端入口：pc / web（ar_glasses、bci_reserved 规划中，目录未建）
examples/        浏览器可玩的分层演示（01–11 + _template，见首页 index.html）
contracts/       跨语言接口契约（Protobuf：pal/v1、genai/v1）
manifests/       每层的 LayerManifest（provides/requires/budgets/degrade）
docs/            架构文档、ADR、AI 原生图、性能记录、release notes
assets/          logo、hero GIF 等品牌资产
```

## 9. 测试、CI 与性能

- 全仓 **279 个测试**（CI 实测口径：`cargo test --workspace` 172 + genai `providers` 50 + agent `json` 32 + subsys `rigid` 14 + infer `native-http` 11 + kernel node 测试 12，2026-10-19 核对），CI 在 Linux 上跑 fmt / build / test / clippy（`-D warnings`）。
- 每个版本在 `docs/release-notes/` 落一份双语说明（v0.1.0 / v0.2.0 / v0.3.0）。
- 基准怎么跑、当前参考数据见 [PERF.md](PERF.md)。

## 10. 常见问题

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

## 11. 延伸阅读

- [AI-NATIVE.md](AI-NATIVE.md)：十一层逐层 AI 内嵌图 + 润滑剂详解
- [COMPUTE-BUDGET.md](COMPUTE-BUDGET.md)：算力分拨管线地图（五个微模型 + 植入模型三步）
- [STUDIO-PIPELINE.md](STUDIO-PIPELINE.md)：Studio 六区 ↔ 十一层衔接管线（P1 建游 / P2 模型 / P3 运行时 / P4 观测）
- [VERTICALS.md](VERTICALS.md)：AI 产业八赛道垂直版矩阵 + 接模型/接 agent 指南 + 三轮测试计划
- [OPEN-MODEL-MATRIX.md](OPEN-MODEL-MATRIX.md)：八赛道开源底座 × 大厂预留 API 矩阵（L7 PROVIDER_PRESETS 13 行）
- [ADR 0001](adr/0001-five-platforms-capability-bitmap.md)：五端一等公民与能力位图
- [ADR 0002](adr/0002-neural-rendering-runtime-invariants.md)：神经渲染运行时不变量
- [ADR 0003](adr/0003-l8-native-generative-ai.md)：L8 原生生成式 AI 边界
- [ROADMAP.md](../ROADMAP.md)：L8/L10/L11 的落地节奏（三阶段演化到 2031+）
- [PERF.md](PERF.md)：基准怎么跑、当前参考数据
- 书技术篇：T3 总览 / T4–T14 各层 / T15 横切 / T17 测试与基准（[BOOK_MAP.md](BOOK_MAP.md)）
