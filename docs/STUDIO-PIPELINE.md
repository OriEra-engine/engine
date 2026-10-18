# OriEra Studio ↔ 十一层管线衔接设计

> 一句话理解：**Studio 是世界引擎的工作台皮肤，不是另一个引擎——
> 界面上的每一个按钮，都必须能沿着层栈追到一行引擎代码。**
> （《OriEra 世界引擎》技术篇 T3「垂直版 = 同一内核 + 赛道皮肤」）

本文回答三件事：Studio v0.1 设计稿的六个界面区各自接什么管线；
每条管线经过哪些层、走什么契约、现在落在哪个 crate；缺口留给 v0.5 什么。
赛道垂直版（游戏/具身智能/AIGC/数字孪生/教育/陪伴/影视/科学）见
[VERTICALS.md](VERTICALS.md)；算力分拨拨杆的内部机制见
[COMPUTE-BUDGET.md](COMPUTE-BUDGET.md)。

---

## 0. Studio 是什么、不是什么

Studio 已是实物：[OriEra-engine/studio](https://github.com/OriEra-engine/studio)
（Windows 桌面端，v0.3.0 发布版，双击即用、内置模型推荐、反馈直达
本仓库 issues）。它是一个**桌面/网页壳**：本地起一个服务
（如 `127.0.0.1:62105`），内置一份引擎快照（v0.3.0 全家 11 crate），
让非程序员用「新建游戏五步向导 + 模型中心 + 命令面板」把一个世界从
零跑到有。它和 `platforms/pc` 是**同一物种**：应用壳持有 World(L3)
与 Scheduler(L2)，自己不写一行 AI 逻辑——这不是巧合，是架构规则的
验收（ARCHITECTURE.md 硬规则 5：AI 是引擎系统）。
本文即该壳与引擎层栈之间的**管线契约**：studio 仓库按此对齐，
新界面元素一律沿本文的管线进引擎，追不到管线的按钮不许上屏。

```
OriEra Studio（壳，等价 platforms/* 的角色）
 ├─ 持有 World(L3) + AssetStore(L4) + Scheduler(L2)
 ├─ 模型中心 → L7 ProviderConfig（key 只在壳层）
 ├─ 命令面板 → L9 parse() → L7 InferSystem::submit()
 └─ 仪表盘   ← L2/L5/L6/L7 的横切读数
```

## 1. 六区总览：界面 → 管线 → 契约

| Studio 区 | 界面元素 | 主管线 | 走过的层 | 契约落点 |
|---|---|---|---|---|
| 仪表盘 | 四张状态卡 | **P4 观测管线** | L2/L5/L6/L7 → 壳 | `system_timings` `allocator().load` `UsageMeter` `idle_frames` `quality` |
| Demo 演示厅 | 11 个内置 demo | P4（只读回放） | 全层（examples/* 静态页） | 每个 demo 只动它那一层 |
| 新建游戏 | 五步向导 | **P1 建游管线** | L9→L7→L6→L3(+L4) | `parse` `Command` `Gravity/TimeScale` `Spawn` `insert_named` |
| 模型中心 | provider 列表 + key | **P2 模型管线** | 壳→L1 route→L7 | `ProviderConfig` `ModelProvider` `route(caps,hint)` `last_usage` |
| 我的项目 | 项目卡 + 存档 | P1 的持久化半边 | L3/L4 | generation 句柄稳定（T6）；`insert_named/get_named` |
| 设置 | 平台画像 | L1 能力位图 | `Capabilities` `frame_budget_us` `detect_platform` |

四条主管线（P1–P4）下面逐一展开。**方向铁律不变**：UI 事件永远从壳
进引擎，读数永远从引擎出壳，任何一条 UI 线不许穿过两层直接摸世界。

## 2. P1 建游管线：五步向导 → 世界从零到有

向导每一步产出一个**引擎侧可执行物**，不是配置文件里的死数据：

| 步骤 | 界面问什么 | 引擎落到哪 | 现有落点 |
|---|---|---|---|
| ① 世界类型/种子 | 「粒子沙盒 / 咖啡馆 / 空白」 | 世界参数资源 + 固定种子 LCG | `InferSystem` 种子；`World::new()` |
| ② 角色与目标 | 名字/性格/角色卡 | L3 组件：`Persona` `Mood` + L9 `Goal`（v0.5） | `oriera-agent/cognition.rs` |
| ③ 世界规则 | 重力/时间流速 | L6 资源：`Gravity(f32)` `TimeScale(f32)` | `oriera-subsys`（钳制 [0,8]） |
| ④ 模型 | 给世界配哪个脑子 | L7 `ProviderConfig`（见 P2） | 八家 provider + rule 离线行 |
| ⑤ 开场脚本 | 自然语言几句话 | L9 `parse()` 逐句 → L7 `submit()` 队列 | 否定语义/合并已在 v0.4 |

数据流（一次「新建」）：

```
向导[完成] ─→ 壳: World + 资源 + 组件注册
          ─→ 开场脚本 parse() → InferSystem 队列（洪峰被 coalesce/预算吸收）
          ─→ Scheduler::tick 第一帧：allocate → infer/physics/render → observe
          ─→ 仪表盘四卡从 -- 变实数
```

**「我的项目」**：项目卡 = 存档索引。L3 generation 句柄 + L4 内容寻址
句柄跨存档稳定（书 T6），所以「另开一个世界再回来」是资源序列化问题，
不是引擎问题。v0.5 落 `World::serialize`（manifest 已列）。

## 3. P2 模型管线：模型中心 → 端云一次决策 → L7 边界

模型中心的每一行 = 一份 `ProviderConfig { kind, model, api_key, base_url }`。
key 的旅行路线**只有一条**：浏览器 localStorage / 壳进程环境 →
`create_provider(&cfg)` → provider 内部。引擎层、世界状态、日志、
仓库永不接触（CI 有 grep 守护）。

```
模型中心选中「智谱 glm-4-flash」
  → 壳层 ProviderConfig{Zhipu, "glm-4-flash", key}
  → create_provider()（L7 工厂，八家 + rule）
  → L1 route(caps, RouteHint)：隐私敏感钉端侧 / NPU 吃小负载 / 紧延迟走边缘
  → CognitionSystem/InferSystem 经 ModelProvider::complete 调用
  → last_usage() → UsageMeter（仪表盘「AI 模型」卡的成本行）
  → 帧预算超订时，该系统的 throttle 拨杆自动下调（v0.4 分拨）
```

**免费版矩阵**（模型中心首批预置行）：见 `crates/oriera-infer/examples/provider_smoke.rs`
内的 FREE_TIER_MATRIX——离线 rule 行永远可跑，七家远程行待厂商认领
（Round 3，VERTICALS.md 附录 C）。原生 HTTP 已落地（#68，feature
`native-http`，PR #79）：Rust 侧 complete 即真实往返；不开 feature 时
真实网络腿仍由浏览器 kernel（demo 11）承载。

**接一个新模型 = 三步，引擎层零改动**（COMPUTE-BUDGET.md §4）：
实现 `ModelProvider`（含如实的 `last_usage`）→ 壳层 `ProviderConfig` →
挂上调度器。计量、节流、合并、降级自动获得。

## 4. P3 运行时管线：命令面板 / 世界 tick 的每一帧

命令面板是 Studio 的「玩家通道」，和 demo 10 言灵世界同一管线：

```
输入「spawn 50 particles」
  → L9 parse（否定语义先于意图：「别清空」不执行）
  → L7 InferSystem::submit（相邻同型可证安全合并）
  → 下一帧 tick：
      L2 BudgetAllocator.allocate（先分拨）
      → infer:  队列预算 ×throttle（下限 1 条/帧）执行命令写世界
      → genai:  生成任务限量轮询（L8，缓而不降级）
      → physics: 恒等无操作整趟跳过（idle_frames++）
      → render: 统计可见性 + 迟滞画质档
      → L2 allocator.observe（观测回灌，闭环）
```

一帧之内 UI 永远不直接触发同步工作——**一切经队列、一切有预算**。
这是「AI 突发负载不撕开界面」的结构保证，不是 UI 框架的努力。

## 5. P4 观测管线：仪表盘四张卡的真实出处

| 界面卡 | 显示 | 引擎读数（一行代码） |
|---|---|---|
| 本地服务 | 端口/运行中 | 壳自身（无引擎依赖） |
| 引擎快照 | v0.3.0 已内置 | 壳 Cargo 元数据 |
| AI 模型 | provider / 未配置 / 成本 | `provider.id()` + `world.resource::<UsageMeter>()` |
| 我的游戏 | 数量/天数 | `world.len()` + `WorldStory.day` |

扩展位（v0.5 仪表盘二期）：帧负载曲线（`allocator().load`）、
省了多少（`coalesced` + `idle_frames`）、画质档（`quality`）、
逐系统毫秒（`system_timings()`）。PC 壳 `platforms/pc` 已全量接线示范。

## 6. 缺口清单（v0.5 排期输入）

| 缺口 | 阻塞的界面 | 现状 | 计划 |
|---|---|---|---|
| ~~原生 HTTP provider 适配器~~ | 模型中心远程行 | **已落地**（#68 → PR #79，feature `native-http`，四分类错误+脱敏） | 剩余：#64 厂商认领真网验证 |
| `World::serialize` | 我的项目存档 | 句柄已稳定 | v0.5，L3 manifest 已列 |
| L9 `Goal` 组件 + 规划器 | 向导②「角色目标」 | `WorldStory.beats` 顶位 | v0.5–v0.6 |
| Studio 壳本体 | 全部六区 | **已上线** github.com/OriEra-engine/studio（v0.3.0） | 按本文管线契约对齐 + 补契约测试 |
| wgpu 真窗口 | 仪表盘画面区 | 统计式渲染 | P2 工程计划（growth/PLAN-engineering-nov） |

一句话收束：**界面是层栈的索引——Studio 的每个按钮都能沿管线追到
一个 crate、一个契约、一行测试；追不到的，就是缺口清单的下一行。**
