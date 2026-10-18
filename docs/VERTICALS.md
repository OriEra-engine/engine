# OriEra 赛道垂直版矩阵 — AI 产业八赛道

> 一句话理解：**通用世界引擎的「通用」不是功能多，而是内核唯一——
> 八条赛道共用同一套 L0–L9 内核，垂直版 = 层配置 + provider 预设 +
> Studio 皮肤。**
> （《OriEra 世界引擎》技术篇 T3 §3.9「垂直版经济学」）

OriEra 不只是游戏引擎。世界引擎的本质产品是**一个会演化、可回放、
有预算意识的状态机**——这正是 AI 产业所有赛道共同缺的底座。本文给出
八条赛道的垂直版设计，每条回答四件事：赛道要什么、引擎怎么裁剪、
首版范围多大、缺口在哪。文末附组织者/社区建议（附录 A/B）与
三轮测试计划（附录 C）。

---

## 0. 垂直版的实现机制

垂直版**不 fork 引擎**。三个旋钮：

1. **层配置**：某层换实现（L6 物理换刚体）或关闭（L5 渲染在无头孪生里关）
   ——每层有 manifest，`provides/requires` 决定能不能安全摘除；
2. **provider 预设**：模型中心预置该赛道的模型行（陪伴赛道钉端侧小模型、
   影视赛道钉大模型批量）；
3. **Studio 皮肤**：向导五步的文案与默认值按赛道换（[STUDIO-PIPELINE.md](STUDIO-PIPELINE.md) P1）。

```
                ┌─ L0–L9 内核（唯一，crates/* 十一个 crate）
 垂直版 = 旋钮 ─┼─ 层配置（L6 换刚体 / L5 无头关闭 / L9 换规划器）
                ├─ provider 预设（端侧小模型 / 云端大模型 / 本地 ollama）
                └─ Studio 皮肤（向导文案、仪表盘卡片、默认世界规则）
```

组织者测试已验证「一内核三赛道」：`cargo run -p oriera-agent --example studio_e2e`
在同一进程里连跑游戏/陪伴/孪生三场景，全部断言通过（附录 C · R1）。

## 1. 八赛道矩阵总表

| # | 垂直版 | 赛道本质需求 | 内核主力层 | 裁剪 | 首版载体 |
|---|---|---|---|---|---|
| 1 | **Studio for Games** | 玩家改写世界、NPC 有戏 | L9+L7+L6 | — | demo 10/11 已在线 |
| 2 | **Sim for Embodied** | 物理保真+传感器+策略评测 | L6+L1+L7 | L5 降级 | 机器人沙盒 demo（v0.6） |
| 3 | **Forge for AIGC** | 文生资产流水线+校验 | L8+L4 | L9 认知可选 | `oriera-genai hotswap` 已有 |
| 4 | **Twin for Industry** | 确定性+回放+时间缩放 | L2+L3+L6 | L5 可无头 | 孪生回放 demo（v0.5） |
| 5 | **Tutor for Edu** | 安全沙盒+AI 导师+可评测 | L9+L6 | L8 可选 | 物理教学沙盒（v0.6） |
| 6 | **Companion for Social** | 长期记忆+情绪+端侧隐私 | L9 记忆+L1 route | L6 轻 | demo 11 变体（v0.5） |
| 7 | **Previz for Film** | 分镜→场景→确定性逐帧 | L8+L5+L6 | 时间轴驱动 | 分镜预演 demo（v0.7） |
| 8 | **Lab for Science** | 大规模实体+可重复实验 | L3+L2 | L5/L8 可关 | 参数扫描 demo（v0.6） |

## 2. 逐赛道设计

### 2.1 Studio for Games — 游戏（旗舰赛道）

- **要什么**：非程序员五步建游；NPC 认知/导演；自然语言改写物理法则；
  AI 生成资产热替换入世界。
- **层配置**：全开。L9 Cognition/Director + L7 八 provider + L8 生成
  + L4 命名热替换 + L5 迟滞画质 + L6 TimeScale（子弹时间玩法）。
- **首版**：即当前 main——demo 10「言灵世界」（语句→物理）、demo 11
  「隅角咖啡馆」（记忆/导演）双范式在线可玩。
- **缺口**：wgpu 真窗口（P2 工程计划）、同源编辑器（书 T14）。

### 2.2 Sim for Embodied — 具身智能

- **要什么**：机器人策略需要一个**便宜、可复位、可回放的物理世界**
  当训练场/评测场（world as benchmark）；传感器仿真（深度相机/碰撞）。
- **层配置**：L6 换刚体物理（Jolt 对接口已在 ROADMAP）；L1 Capabilities
  扩传感位；L7 route 钉端侧（机器人为隐私+延迟最优场景）；L9 认知
  →策略 agent（「想」换成 policy 推理）；L5 降为无头或简化观测。
- **首版**：碰撞沙盒 + 固定种子复位（同种子两次逐位一致——R1 场景 C
  已验证此地基）+ 评测报告（world 状态断言）。
- **为什么是 OriEra**：确定性回放（书 T5）+ 预算分拨（v0.4）正是
  sim2real 评测场的两个硬前提。

### 2.3 Forge for AIGC — AIGC 内容生产

- **要什么**：文生 3D/贴图/动画的**流水线**：任务队列→生成→校验→
  入库→导出 GLB/USDZ；批量便宜。
- **层配置**：L8 主角（ADR 0003 六段式管线 2028 做深）；L4 内容寻址
  去重（同 hash 零成本复用）；L2 分拨让批量任务吃剩余预算；L9/L6/L5
  可全关（纯生产工具形态）。
- **首版**：`oriera-genai` 骨架已落（MockProvider/Tripo/Seedance 适配器、
  `hotswap` 示例）；v0.5 接原生 transport 后即出批量 demo。
- **为什么是 OriEra**：哈希即身份 + 校验入位 + 失败落 placeholder
  （ADR 0003 三准则）让生成结果**可审计**——这是 AIGC 产业交付的痛点。

### 2.4 Twin for Industry — 数字孪生

- **要什么**：把产线/园区/电网镜像进引擎，注入事件看演化，出结论
  必须可解释（「为什么」=回放）。
- **层配置**：L2/L3 全量（确定性核心）；L6 TimeScale（快进十年/慢放
  一秒）；L1 storage/net 接企业数据源；L5 可无头（报表形态）或全开
  （大屏形态）；L7/L9 可选接预测模型。
- **首版**：事件注入→回放 demo：`write_event` 注入 + 同种子重放
  + `WorldStory.beats` 时间线（R1 场景 C 的冻结注入即事件注入的雏形）。
- **为什么是 OriEra**：竞品孪生平台没有「确定性回放 + AI 预算」这两个
  引擎级原语，AI 接入后不可审计。

### 2.5 Tutor for Edu — 教育培训

- **要什么**：安全沙盒（物理规则可改不可崩）+ AI 导师（讲解+出题+
  判分）+ **可评测**（学习行为=世界状态，可回放可归因）。
- **层配置**：L9 DirectorSystem 变体当导师（推进「课程节奏」而非剧情）；
  L6 沙盒（TimeScale 慢放讲原理）；学生每次操作 = `WakeEvent` + 命令；
  评测 = 世界终态断言 + 回放。
- **首版**：物理实验教学（重力/速度/时间三个可改参数已是现成命令）。
- **合规红利**：rule provider 离线行让教育版**零 key 零云端**跑起来，
  进校无数据合规问题。

### 2.6 Companion for Social — 社交陪伴

- **要什么**：记得住（长期记忆跨会话）、有情绪连贯性、**隐私钉端侧**、
  省算力（没人看时降频不降质）。
- **层配置**：L9 MemoryStore 主角（检索式长期记忆已落地）+ Mood 组件
  →情绪状态机（v0.5 扩）+ L1 route 钉端侧（陪伴对话=隐私敏感）+
  注意力节拍器（v0.4 已落：安静时 ×(1/throttle) 降频，WakeEvent 即回）。
- **首版**：demo 11 换端侧小模型行（ollama/小参数量 API）+ 跨会话记忆
  持久化（localStorage 已有，v0.5 换 L1 storage 通道）。
- **为什么是 OriEra**：陪伴产品的两个死穴——记忆丢失与费钱——
  分别被 L3/L9 记忆架构与 L2 分拨正面解决。

### 2.7 Previz for Film — 影视动画预演

- **要什么**：剧本/分镜→快速生成场景→**确定性逐帧**（离线渲染=
  每帧可复现）→粗剪动画。
- **层配置**：L8 生成场景资产；L6 TimeScale 变时间轴驱动（v0.5 提供
  `with_fixed_dt` 确定性 tick 模式）；L5 帧导出（wgpu 落地后）；
  回放确定性（固定种子 LCG + generation 句柄）保证「导演改一镜重渲染
  其余不动」。
- **首版**：分镜文本→简单场景→固定种子逐帧（帧序列位图一致）。
- **为什么是 OriEra**：影视预演的工业痛点是「改一处、全部重渲」；
  确定性 tick + 内容寻址缓存正面命中。

### 2.8 Lab for Science — 科学仿真

- **要什么**：大规模实体（Agent-Based Model 十万级）、参数扫描
  （同模型 N 组种子批量）、结果可重复（论文级可复现）。
- **层配置**：L3 大规模（demo 02 已验证 20 万粒子浏览器跑）；L2
  `AllocatorConfig::off()` 保严格确定；L5/L8 可关；参数扫描 =
  Studio 建游向导①「种子」的批量形态（v0.6 批处理器）。
- **首版**：扩散/流行病 ABM demo（ Persona→易感态，Memory→接触史，
  `WorldStory`→疫情时间线）。
- **为什么是 OriEra**：科研用户要的不是画面，是「同一份世界定义，
  别人跑出同一个结果」——确定性是 OriEra 的第一性设计。

## 附录 A · 组织者建议（按优先级）

1. **v0.5 主线：原生 HTTP provider 适配器**（transport 注入，ADR 0003
   模式）——解锁模型中心七条远程行，这是所有赛道共用的最大缺口。
2. **确定性 tick 模式** `Scheduler::with_fixed_dt`——影视/科学/孪生
   三条赛道的共同地基，改动只在 L2。
3. **`World::serialize`**——「我的项目」存档与所有赛道的会话恢复。
4. **Studio 壳独立仓库**启动（按 STUDIO-PIPELINE.md 的管线契约做，
   壳只许依赖契约不许依赖 crate 内部）。
5. **Goal/规划器进 L9**——向导②与具身/教育赛道的「目标」语义。
6. **品牌层声明**：README/官网把「通用世界引擎，游戏是第一条赛道
   而非唯一定位」写成显式叙事（当前外部认知=游戏引擎）。

## 附录 B · 社区极客建议（接模型 / 接 agent 具体步骤）

**接一个模型（三步，引擎层零改动）**：
1. 实现 `oriera_infer::ModelProvider`（`complete` + 如实的 `last_usage`）；
   OpenAI 兼容厂商只需换 `base_url`；
2. 壳层构造 `ProviderConfig{kind, model, api_key}`，key 只进环境/
   localStorage，永不入仓；
3. 用 `cargo run -p oriera-infer --example provider_smoke` 自检——
   在 FREE_TIER_MATRIX 给你的厂商行打 ✅ 并发 PR。

**接一个 agent（三步）**：
1. 把 agent 写成 `System<World>` 挂 L2 调度器（参考 CognitionSystem：
   认知循环是引擎原语，不是编排骨子）；
2. 模型调用只走 L7 边界；世界读写只经 `&mut World` 参数；
3. 想要节流保护就读一行拨杆 `ctx.grants.throttle_of(self.name())`；
   想被玩家唤醒就消费 `WakeEvent`——两个都是一行代码的事。

**good-first-issue 候选**（难度从低到高）：
- 给 FREE_TIER_MATRIX 补一家厂商行并跑通冒烟；
- 给某个垂直版写一个 examples/ HTML demo（纯 JS，照 _template）；
- Mood→情绪状态机扩展（L9，一个组件一个转移表）；
- 扩散 ABM demo（L3/L9 组装，无新引擎代码）；
- `with_fixed_dt` 确定性 tick（L2，~50 行 + 测试）。

## 附录 C · 三轮测试计划

| 轮次 | 谁 | 测什么 | 载体 | 判定 |
|---|---|---|---|---|
| R1 组织者 | 维护者 | 一内核三赛道 e2e + 全仓门禁 | `examples/studio_e2e.rs`（agent crate） | 断言全过 + CI 绿 |
| R2 社区伙伴 | 开源贡献者 | 六区管线按清单手测 + 各赛道 demo 体验 | GitHub discussion 清单 + good-first-issue | 清单条目有人回 ✅ |
| R3 模型厂商 | 厂商/社区 | 免费版接入冒烟（七行矩阵） | `examples/provider_smoke.rs`（infer crate） | 矩阵行打 ✅ 发 PR |

- **R1 已执行**（2026-10-18）：三场景断言全过——游戏（合并 40+20→60、
  否定拒执行、Count 回读、分拨仪表盘）、陪伴（压力 10 句+唤醒 10 句、
  记忆 20 条、计量 20 次）、孪生（两次运行逐位一致、冻结注入 30 帧
  空转跳过）。
- **R2 已发起**：discussion「Studio 管线 × 垂直版 · 社区测试计划」
  挂六区清单，社区按条回帖打勾。
- **R3 已发起**：issue「模型厂商免费版接入测试邀请」挂矩阵与认领表，
  离线行 ✅，七条远程行待认领（原生网络腿 v0.5 解锁，浏览器通道先行）。

一句话收束：**八条赛道不是八个产品，是同一台发动机的八个档位——
内核不动，档位换来的是面对不同产业的扭矩曲线。**
