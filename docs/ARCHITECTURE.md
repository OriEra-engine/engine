# OriEra 架构总览

> 一句话理解：**十一层是责任边界，接口契约是连接器，横切系统是全局仪表盘与刹车。**
> （《OriEra 世界引擎》技术篇 T3）

本文是仓库的架构地图：每层是什么、代码在哪、依赖规则是什么、怎么扩展。
层职责与接口契约的完整定义见书技术篇 T4–T15 与附录 B。

## 十一层与 crate 对照

| 层 | 职责 | crate | 状态 | 关键接口（附录 B） |
|---|---|---|---|---|
| L0 | 跨层共享类型 | `crates/oriera-common` | ✅ | `Timestamp` / `FrameIndex` |
| L1 | 平台抽象：图形/输入/存储/网络/XR 边界 | `crates/oriera-pal` | ✅ 骨架 | `QueryCapabilities` `CreateGfxBackend` `PollInputEvents` `OpenAssetStream` `RouteRemoteTask` |
| L2 | 核心系统：帧调度、确定性框架 | `crates/oriera-core` | ✅ 骨架 | `BeginFrame` `SubmitTask` `ReportBudget` |
| L3 | 对象模型：ECS、资源单例 | `crates/oriera-ecs` | ✅ 可用 | `Spawn` `Query` `Publish`(规划) `Serialize`(规划) |
| L4 | 资产管线：句柄注册表 | `crates/oriera-asset` | ✅ 骨架 | `ResolveAsset`(哈希即身份，规划) `Prefetch` `Evict` |
| L5 | 渲染内核：Pass 图、帧提交 | `crates/oriera-render` | ✅ 骨架 | `BuildFrameGraph` `AddPass` `SetQuality` |
| L6 | 子系统服务：物理/动画/音频/UI/网络同步 | `crates/oriera-subsys` | ✅ 物理 | `StepPhysics` `SampleAnimation`(规划) `SyncState`(规划) |
| L7 | AI 推理运行时：命令执行、端云路由 | `crates/oriera-infer` | ✅ 骨架 | `SubmitInference` `ReportInferenceMetrics` |
| L8 | 生成式 AI：资产生成管线 | `crates/oriera-genai` | ✅ 骨架（ADR 0003） | `RequestGeneration` `ValidateAsset` |
| L9 | Agent 层：意图→命令 | `crates/oriera-agent` | ✅ 规则版 | `InvokeTool`（v1 规则解析，后续 LLM） |
| L10 | AR/VR 交互：多模态感知 | — | 📋 2027 H2 | 手势/眼动经 L1 `SpatialInputEvent` 进来 |
| L11 | 应用与 UGC 创作生态 | `platforms/*` + `examples/*` | ✅ 壳 | 同源编辑器（规划） |
| 横切 | 观测/安全/计量/调试 | 各层埋点 | 📋 | `MeasureGPU`（L5 已有雏形） |

五端入口在 `platforms/`：`pc`（可运行）、`web`（Wasm）、`ar_glasses`、`bci_reserved`（仅预留 bit 位）。

## 依赖规则（T3 §3.3，CI 会守护）

1. **只准向下依赖**：上层只依赖下层的抽象接口。当前 Cargo 依赖图：
   `agent(L9) → infer(L7) → subsys(L6)/ecs(L3) → core(L2) → common(L0)`；
   `render(L5) → ecs(L3) → core(L2)`；`pal(L1)` 只被平台壳引用。
   **禁止**出现 `render → subsys` 这类倒置（v0.0.1 曾有，已修复）。
2. **禁止跨层共享可变全局状态**：v0.0.1 曾用 `thread_local! WORLD` 让 L5/L6
   偷看世界，已删除。现在的规则：**应用壳（platforms/*）持有 World，
   L2 调度器每帧把 `&mut World` 显式传给每个 System**；
   跨 System 共享状态走 L3 的 resource 单例（如 `Gravity`：L9 解析 →
   L7 写入 → L6 读取）。
3. **平台差异止步于 L1**：上层读 `Capabilities` 位图做降级，
   不写 `#[cfg(target_os)]`（ADR 0001）。
4. **接口稳定优于实现统一**：跨层契约优先落到
   `contracts/`（Protobuf / JSON Schema），破坏性变更必须升版本。

## 一帧的数据流（`cargo run -p oriera-pc`）

```
应用壳 (platforms/pc)
  │ 持有 World(L3) + AssetStore(L4)，构建 Scheduler<World>(L2)
  ▼ 每帧 sched.tick(&mut world)
InferSystem(L7)   执行命令队列 → 生成/清除实体、写 Gravity resource
PhysicsSystem(L6) for_each2_mut::<Position,Velocity> 积分 + 边界反弹
RenderSystem(L5)  统计可见 Position → draw_calls（真实 Pass 图在 ROADMAP）
  ▼
调度器回报 frame_ms（L2 观测埋点）
```

自然语言入口在 L9：`oriera_agent::parse("spawn 50 particles")` →
`Command::Spawn(50)` → `InferSystem::submit()` → 下一帧生效。
整条链路没有任何全局变量，回放确定性由 L3 generation 句柄与
L7 的固定种子 LCG 保证。

## 润滑剂契约（v0.2，2026-10-15「全层润滑剂」PR）

每层新增的加法式契约（详见 `docs/AI-NATIVE.md` 末节与各层 manifest）：

- L1 `route(caps, RouteHint) -> ExecutionSite`：端云路由在 PAL 一次决策
- L2 `Scheduler::system_timings()` / `with_budget_ns()`：逐系统耗时 + 平台预算曲线
- L3 `write_event` / `take_events` / `resource_or_default`：类型化事件、自愈资源
- L4 `insert_named` / `get_named` / `unload`：命名资产热替换（L8 落地点）
- L5 `Quality` + `RenderSystem::observe_frame`：迟滞画质调速
- L6 `TimeScale`：仿真节流资源（钳制 [0,8]）
- L7 帧预算命令队列（`deferred`）+ `UsageMeter` / `last_usage()` 计量
- L9 认知/导演自愈启动 + 否定语义解析

## 目录地图

```
crates/          L0–L9 各层实现（见上表）
platforms/       五端入口：pc / web / ar_glasses / bci_reserved
examples/        浏览器可玩的分层演示（01–09，见首页 index.html）
contracts/       跨语言接口契约（Protobuf，目前 pal/v1、genai/v1）
manifests/       每层的 LayerManifest（provides/requires/budgets/degrade）
docs/            架构文档、ADR、性能记录、release notes
```

## 治理：LayerManifest 与架构门禁（T3 §3.8.3）

每层在 `manifests/` 声明机器可读的清单：对外提供什么契约、
依赖哪些契约、性能预算、降级策略。CI 据此做门禁（规划中）：

- 某层 `requires` 了未声明 stable 的契约 → CI 失败；
- `provides` 的契约发生不兼容变更未升主版本 → CI 失败；
- 实测 P99 超预算 20% → 发布进入评审。

新增一层时：建 crate → 写 `manifests/Lxx-*.yaml` →
在 `contracts/` 落契约 → 在本表加一行 → CI 接入。

## 延伸阅读

- 书技术篇：T3 总览 / T4–T14 各层 / T15 横切 / T17 测试与基准
- [ADR 0001：五端一等公民与能力位图](adr/0001-five-platforms-capability-bitmap.md)
- [ROADMAP.md](../ROADMAP.md)：L8/L10/L11 的落地节奏
- [PERF.md](PERF.md)：基准怎么跑、当前参考数据
