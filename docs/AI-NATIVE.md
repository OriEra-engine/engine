# AI 原生能力 · 十一层内嵌图

> 原则：**AI 是引擎系统，不是应用代码**。每一层的 AI 能力都作为引擎服务
> 存在，经 L7 `ModelProvider` 边界调用大模型；创作者在装配期自选提供商
> （智谱 / OpenAI / DeepSeek / Moonshot / Groq / OpenRouter / Anthropic / 离线规则），
> 引擎对厂商零硬编码。Key 只存在于平台壳（浏览器 localStorage / 环境变量），
> 永不进入引擎层、世界状态或仓库。

选型入口：Rust `oriera_infer::{ProviderConfig, ProviderKind, create_provider}`；
浏览器 `engine/kernel.js :: createProvider(cfg) + PROVIDER_PRESETS`（一一对应）。

| 层 | 内嵌的 AI 能力 | 现状 | 消费方式（创作者视角） |
|---|---|---|---|
| L1 PAL | 端云推理路由：按设备能力位图决定端侧/云端模型；传感器（眼动/语音）语义化预处理 | 📋 路由策略随 T10 落地；能力位图已就绪 | 换端不换代码 |
| L2 Core | AI 任务的帧预算切片：认知/导演 tick 限时不超帧预算；确定性回放录制推理输入输出 | ✅ 调度器已驱动认知系统 | `Scheduler::add(cognition)` |
| L3 对象模型 | 记忆即组件：episodic memory 挂在实体上，`Serialize` 后随世界快照持久化 | ✅ MemoryStore（L9 服务）；快照见 #12 | 实体级记忆，无需自建存储 |
| L4 资产管线 | 生成式资产（文生 3D/纹理/音效）六段式流水线，校验修复后入库 | 📋 L8 供给（ROADMAP 2028）；接口在附录 B 表 B-4 | `GenerateAsset(prompt, 约束, 预算)` |
| L5 渲染内核 | 神经渲染（3DGS/NeRF 混合几何）；AI 超分与注视点优化 | 📋 能力位图已含 NEURAL_RENDER_3DGS 位 | Pass 图里声明式启用 |
| L6 子系统 | AI 动画（语音驱动口型/程序化动作选择）；空间音频场景理解 | ✅ 动作选择已由认知输出 action 驱动 | NPC 输出 `wave/dance/...` 自动映射 |
| L7 推理运行时 | **模型生命周期唯一入口**：提供商注册表、请求队列、重试、指标、降级链 | ✅ `ModelProvider` + `create_provider`（8 家）+ 浏览器 Http/Anthropic 实现 | 装配期一行选厂商 |
| L8 生成式 AI | 世界内容生成：NPC 台词之外的道具/剧情/地图种子 | 📋 ROADMAP 2028 | 同 L4 |
| L9 Agent | **认知/长期记忆/AI 导演**：`CognitionSystem`、`MemoryStore`、`DirectorSystem` 全部是 `System<World>` | ✅ Rust + kernel.js 双侧同构 | 注册系统即得会思考的 NPC |
| L10 交互层 | 多模态入言：语音/眼动/手势转成自然语言意图进 L9 | 📋 SpatialInputEvent 已就绪 | 说话即操作 |
| L11 应用/UGC | 自然语言创作：言灵世界（法则即语句）与咖啡馆（对话即内容）两个范式 | ✅ demo 10 / demo 11 | 创作者写剧本=写提示词 |
| 横切 | 推理可观测（calls/errors/降级计数）、Token 成本计量、内容安全护栏 | ✅ 指标已埋；计量见 SECURITY.md 边界 | 全层只读指标 |

## 两个范式的定位

- **言灵世界（demo 10）**：AI 原生*玩法*——玩家语句经 L9 规则解析变为 L7 命令，
  改写 L6 物理法则。展示「自然语言 → 引擎命令」通道。
- **隅角咖啡馆（demo 11）**：AI 原生*世界*——认知/记忆/导演是引擎系统，
  应用零 AI 代码。展示「引擎内嵌 AI」的完整形态。

两者共用同一条 L7 边界；把咖啡馆的 `CognitionSystem` 换成言灵的
`parse()`，就是创作一个新的 AI 原生玩法——**这就是降低创作门槛的含义**。
