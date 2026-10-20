# AI 原生能力 · 十一层内嵌图

> 原则：**AI 是引擎系统，不是应用代码**。每一层的 AI 能力都作为引擎服务
> 存在，经 L7 `ModelProvider` 边界调用大模型；创作者在装配期自选提供商
> （智谱 / OpenAI / DeepSeek / Moonshot / Groq / OpenRouter / Anthropic / 离线规则），
> 引擎对厂商零硬编码。Key 只存在于平台壳（浏览器 localStorage / 环境变量），
> 永不进入引擎层、世界状态或仓库。

选型入口：Rust `oriera_infer::{ProviderConfig, ProviderKind, create_provider}`；
浏览器 `engine/kernel.js :: createProvider(cfg) + PROVIDER_PRESETS`（v2 起 13 行逐行对齐，
命令通道 `parseIntent/CommandQueue` 镜像 Rust `parse/InferSystem`——七赛道实战反哺，见 release-notes/v0.6.0-browser-parity.md）。

| 层 | 内嵌的 AI 能力 | 现状 | 消费方式（创作者视角） |
|---|---|---|---|
| L1 PAL | 端云推理路由：按设备能力位图决定端侧/云端模型；传感器（眼动/语音）语义化预处理 | ✅ `route(caps, RouteHint)` v1 纯函数策略已落地 | 换端不换代码 |
| L2 Core | AI 任务的帧预算切片：认知/导演 tick 限时不超帧预算；确定性回放录制推理输入输出 | ✅ 调度器已驱动认知系统；逐系统耗时 + 平台预算已接入 | `Scheduler::add(cognition)` |
| L3 对象模型 | 记忆即组件：episodic memory 挂在实体上，`Serialize` 后随世界快照持久化 | ✅ MemoryStore（L9 服务）；快照见 #12 | 实体级记忆，无需自建存储 |
| L4 资产管线 | 生成式资产（文生 3D/文生图/音效）流水线，校验后命名热替换入库 | ✅ L8 供给 → `PendingSwaps` → `apply_pending` 原位换芯；浏览器镜像 `AssetStore.insertNamed`（demo 13 全链实跑） | `GenerateAsset(prompt, 约束, 预算)` |
| L5 渲染内核 | 神经渲染（3DGS/NeRF 混合几何）；AI 超分与注视点优化；生成资产的槽位消费 | 📋 神经渲染位图已含；✅ 槽位消费已落地（JS `SlotBindings`：命名资产→材质/网格，Rust 侧随 wgpu 窗口落地） | 绑定槽位名，落地自动换芯 |
| L6 子系统 | AI 动画（语音驱动口型/程序化动作选择）；空间音频场景理解 | ✅ 动作选择已由认知输出 action 驱动 | NPC 输出 `wave/dance/...` 自动映射 |
| L7 推理运行时 | **模型生命周期唯一入口**：提供商注册表、请求队列、重试、指标、降级链 | ✅ `ModelProvider` + `create_provider` + 预设 13 行（离线 rule 1 + 云端 8 + 本地开源 4，`PROVIDER_PRESETS` 有测试钉死行数）+ 浏览器 Http/Anthropic 实现 | 装配期一行选厂商 |
| L8 生成式 AI | 世界内容生成：文生图（天空/地面/海报/精灵图）+ 文生 3D 道具 + 文生视频（NPC 台词归 L9） | ✅ 五种类类齐全（RFC 0001 增 `text_to_image`）：Rust `GenerationProvider` 边界 + CogView/Tripo/Seedance 适配器 + 按格式匹配的占位降级；离线确定性后端 Rust 名 `MockProvider`、浏览器镜像名 `ProceduralProvider`（同一角色）；浏览器镜像 `engine/genai.js`（demo 13 实跑） | 装配期选供应商，`ArtDirectorSystem` 一句话主题装帧世界 |
| L9 Agent | **认知/长期记忆/AI 导演**：`CognitionSystem`、`MemoryStore`、`DirectorSystem` 全部是 `System<World>` | ✅ Rust + kernel.js 双侧同构 | 注册系统即得会思考的 NPC |
| L10 交互层 | 多模态入言：语音/眼动/手势转成自然语言意图进 L9 | 📋 SpatialInputEvent 已就绪；层 manifest 随实现补齐（当前实装 L0–L9 共 10 份，见 `manifests/`） | 说话即操作 |
| L11 应用/UGC | 自然语言创作：言灵世界（法则即语句）与咖啡馆（对话即内容）两个范式 | ✅ demo 10 / demo 11 | 创作者写剧本=写提示词 |
| 横切 | 推理可观测（calls/errors/降级计数）、Token 成本计量、内容安全护栏 | ✅ 指标已埋；计量见 SECURITY.md 边界 | 全层只读指标 |

## 两个范式的定位

- **言灵世界（demo 10）**：AI 原生*玩法*——玩家语句经 L9 规则解析变为 L7 命令，
  改写 L6 物理法则。展示「自然语言 → 引擎命令」通道。
- **隅角咖啡馆（demo 11）**：AI 原生*世界*——认知/记忆/导演是引擎系统，
  应用零推理管线（⚙ 面板的一次连通性 ping 与 key 存取属壳层配置职责，
  prompt 拼装与对话循环都在 kernel.js）。展示「引擎内嵌 AI」的完整形态。

两者共用同一条 L7 边界；把咖啡馆的 `CognitionSystem` 换成言灵的
`parse()`，就是创作一个新的 AI 原生玩法——**这就是降低创作门槛的含义**。

## v0.2 润滑剂升级（2026-10-15，PR「全层润滑剂」）

目标：让 AI 原生应用/游戏跑在引擎上像加了润滑剂的发动机——每层去掉一个
AI 场景下最疼的摩擦点。全部为加法式变更，现有 demo 的 API 语义不变。

| 层 | 摩擦点 | 润滑剂 |
|---|---|---|
| L0 | 各层各造 token 计量轮子 | `TokenUsage` 统一词汇（L7 产出 / L9 累计 / 上层展示） |
| L1 | 每个上层自己掰扯「端还是云」 | `route(caps, RouteHint)` 一次决策：隐私钉死端侧、NPU 吃小负载、紧延迟走边缘 |
| L2 | 帧爆了不知道是谁干的；60fps 曲线 grading 一切设备 | `system_timings()` 逐系统毫秒 + `with_budget_ns`（AR 眼镜按 90fps 曲线） |
| L3 | 每系统每帧一次 O(n) 克隆；忘注册资源 = 帧中 panic；跨系统信号无标准通道 | 零分配单列迭代 / `resource_or_default` 自愈 / `write_event`·`take_events` 类型化事件 |
| L4 | 生成的新资产进来要重建实体引用 | `insert_named`/`get_named` 按名热替换（同句柄原位换芯）+ `unload`——L8 生成管线的落地点 |
| L5 | AI 一重就掉帧，画质不动 | `observe_frame` 迟滞调速器：过载降档 batching（1/4/16），有余力缓慢升档 |
| L6 | 慢动作/暂停要手改每个速度 | `TimeScale` 资源，物理步长按它缩放，钳制 [0,8] 防幻觉值炸世界 |
| L7 | 命令洪峰一帧全执行 → 帧尖刺；调用成本不可见 | 帧预算队列（`deferred` 计数，洪峰摊到后续帧）+ `last_usage()`/`UsageMeter` 计量 |
| L9 | 应用忘预注册资源直接 panic；「别清空」被执行成清空 | 认知/导演自愈启动（裸世界 boot 是测试契约）；否定先于意图 → 不执行 |
| L11 | — | PC 壳全量接线示范：平台预算、画质调速、逐系统耗时报告、资产热替换、TimeScale |

一句话：**AI 的输出是概率性的、突发的、贵的；引擎的职责是把这些毛刺
吸收掉——计量、限流、降级、自愈，让上层以为世界永远是平滑的。**

## v0.4 算力分拨（2026-10-16「先分后花」）

润滑剂让每层各自吸收毛刺；算力分拨让**预算在帧开始前就有出处**：
L2 仲裁器按各系统的平滑实测成本切分帧预算（带 5% 保底），经
`FrameCtx::grants` 把 `{share_ms, throttle}` 拨杆下发到每个系统。
查无此名 = 未纳管 = 满速——**植入新模型不需要先改仲裁器**。

| 微模型 | 层 | 治什么 |
|---|---|---|
| `BudgetAllocator` 分拨仲裁 | L2 | 挤兑：超订帧全体按 share/cost 比例减速，小系统保满速，下限 0.1 |
| `ComputeGrant`/`Grants` 词汇 | L0 | 各层各造轮子：一个结构体全引擎通用，借用下发零分配 |
| 命令合并 coalesce | L7 | 重复：相邻同型命令可证安全合并（Spawn 求和/重力取末/Count 取末） |
| 空转跳过 idle skip | L6 | 空转：冻结或完全静止的积分是恒等无操作，整趟跳过（位级等价） |
| 注意力节拍器 | L9 | 没人看还在聊：压力下说话间隔拉长（封顶 8×），`WakeEvent` 一碰即回满速 |

与润滑剂的关系：仲裁治**挤兑**，消费端微模型治**空转与重复**；
预算之内不折腾——省不省是消费端的事，压不压是仲裁器的事。
完整地图：[COMPUTE-BUDGET.md](COMPUTE-BUDGET.md)。

## 生成式表达内生（2026-10-17，RFC 0001 + demo 13）

小 3D AI 原生游戏「渲染很差」的根因不是渲染器，而是**生成能力没长在
引擎里**：世界视觉只能用手工作坊的固定资产。这次把表达链路补成引擎器官：

| 层 | 落地 | 治什么 |
|---|---|---|
| L8 | `text_to_image` 种类（RFC 0001，契约加法）+ CogView 适配器（OpenAI images 方言：智谱默认，任意兼容端点是数据不是代码） | 天空穹顶/地面/海报/精灵图——小 3D 游戏表达的主力 |
| L8 | 占位降级按种类匹配格式：图片槽失败落**真实可解码的 1×1 PNG**，网格槽落空场景 GLB（Rust `tiny_png`/JS 同构，CRC/Adler 经真实解码器验证） | 降级不再「假装成功」也不崩——降级也是表达 |
| L8 | 浏览器内核镜像 `engine/genai.js`：边界词汇（kind/caps/校验/计量/占位）与 Rust 一一对照；fetch Promise settling 取代轮询节拍 | 所有 3D demo 跑的浏览器侧从此有 L8，不再零生成能力 |
| L8→L9 | `ArtDirectorSystem`：提示词形状是引擎知识，应用只说主题 | prompt 拼装不许漏进应用层（AI 原生纪律） |
| L4 | `AssetStore.insertNamed` + `applyPending`（JS 镜像）：同名原位换芯 | 再生成不重建实体、不刷新页面 |
| L5 | `SlotBindings` 槽位绑定：命名资产 → 材质贴图 / GLB 网格 | 生成物有标准落位通道，「渲染很差」从管线层面消失 |
| L11 | demo 13 生成式画廊：一句话主题装帧世界；「表达对比」开关直接展示纯色→生成的差别 | 病因与解药可视化 |
| L8→L9→L11 | demo 11 咖啡馆加入同一条生成链：`StoryboardArtSystem` 观察 `world.story.beats`，每两拍为 `wall.art` 槽位装帧「此刻的故事」；同一 Key 经 `imageProviderFromChatConfig` 从 L7 聊天选型映射为 L8 图像端点 | 世界活着=墙面跟着剧情活 |

一句话：**世界的外观也是生成系统的输出**——引擎负责让这份输出
可控（校验）、可负担（计量/降级）、可落位（热替换），创作者只说
「我要一个什么样的世界」。

## v0.7 缓存·成本·结构化输出（2026-10-21，PLAN-deepseek-upgrade 第一批）

借 DeepSeek 开源双仓库（DeepInfer 借思想 / deepseek-recipe 直接用）的
第一批落地。生态位声明也在此立字：

> **OriEra 不做推理引擎，做推理引擎的好客户端。** 供给侧（把模型跑快
> 跑稳）交给 vLLM / DeepInfer / SGLang；OriEra 把模型输出当作世界模拟
> 的一种**预算内资源**来调度——这回答了「为什么引擎里没有 CUDA kernel」。

| 层 | v0.7 契约 | 意义 |
|---|---|---|
| L0 | `TokenUsage.cached`：缓存命中 prompt tokens（⊆ prompt） | 前缀缓存折扣首次成为全链路词汇 |
| L7 | `CostMeter`：成本账本（µ 货币累积器），`saved_micro` 把缓存省钱显示成钱 | 「缓存命中 ≈ 1/50 价」从营销话术变成仪表盘读数 |
| L7 | `ProviderOptions.response_schema` → `response_format: json_schema`（非法 schema 零网络 fail-fast） | 模型输出形状的请求权交到引擎手里（per-call 等 B1） |
| L7 | `ReasoningEffort` 经 deepseek-recipe-core 规范枚举上线 | DeepSeek 官方协议类型层首次进依赖（MIT，仅 native-http，wasm 零增量） |
| L7 | `PROVIDER_PRESETS.caps/price` 能力与牌价元数据 | 厂商事实从代码注释升格为带日期的登记数据 |
| L9 | **稳定前缀惯例**：system 消息对 NPC 恒定（人设+格式），世界日/剧情拍/记忆/对话全走 user 消息（Rust+JS 双侧，字节稳定是测试契约） | 咖啡馆的认知调用从「缓存零命中」变「每拍命中」——引擎替模型管住 prefill |
| L9 | 注入用例集：玩家输入是不可信数据；三道防线 = schema/形状校验（B6）+ L2 预算帽 + 否定语义 | 对抗语料钉进测试，防回归 |

一句话：**AI 的输出是概率性的，但它的账单必须是确定性的**——
计量的归计量（cached/total），算钱的归算钱（saved/unpriced），
形状的归形状（schema/校验），谁也不许含糊。
