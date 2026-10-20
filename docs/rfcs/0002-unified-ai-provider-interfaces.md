# RFC 0002 — 统一 AI 供应商接口：槽位路由与世界模型会话

- 状态 / Status: **草案 / Draft**（2026-10-20，待 Discussions 讨论）
- 影响契约 / Contract: `oriera.infer.provider.v1`、`oriera.genai.v1`（新增 `worldmodel` 子契约草案）
- 性质 / Nature: 能力演进路线（流式/工具调用/结构化输出/注册表/槽位路由 + L8 第二 trait）
- 关联 / See also: ADR 0003、RFC 0001、[合作方视角评审](0002-unified-ai-provider-interfaces.review.md)、配套演示页 `studio` 仓库 vendor showcase

---

# RFC：AI 供应商统一接口——L7 模型注册表/任务槽位路由 与 L8 世界模型会话边界

> 按 `GOVERNANCE.md` 流程发在 Discussions → Ideas 区。
> 前置：RFC-1（L7 神经运行时不变量）、RFC-2 / ADR 0003（L8 生成式 AI 内生五条）。
> 回答两个问题：① 文本类 LLM 如何以统一接口接入、供用户选择；② 生成式 3D 世界模型如何以统一接口接入、供用户选择。

## 动机

"AI 原生、每一步由 AI 自主调控"的前提是：引擎拥有**两个统一边界**——文本模型边界（大脑）与生成模型边界（手），且选择权以**数据**形态交给用户。L7 `ModelProvider` 与 L8 `GenerationProvider` 已确立"引擎拥有模型访问、上层不见 HTTP/密钥/型号"的哲学；本 RFC 把它们从"能跑通"推进到"可供用户在多家供应商间按任务选择、可降级、可计量"。

红线不变：不绑定单一云厂商私有 API、密钥只在平台壳、引擎核心零 vendor SDK、demo 路径全离线可跑。

## 一、L7 文本模型：从单一 complete 到"注册表 + 槽位路由"

### 现状（不改动哲学，只补能力）

- `ModelProvider`（`oriera.infer.provider.v1`）：`id / complete / last_usage`；`RuleProvider` 离线兜底；kernel.js `createProvider` 与原生工厂 1:1。
- `ProviderConfig{kind, model, api_key, base_url}` + `ProviderKind{Zhipu, OpenAiCompat, Anthropic, Rule}`——选择已是数据。`OpenAiCompat` 一类覆盖 DeepSeek/Moonshot/Groq/OpenRouter/vLLM/Ollama（业界事实标准：OpenAI Chat Completions 格式），`Anthropic` 单列（system 顶层、tool 格式差异），智谱单列（V4 鉴权细节）。

### 缺口与补全（按优先级）

1. **流式**：`fn stream(&self, req) -> BoxStream<StreamEvent>`。SSE 解析留在 adapter，契约只有事件。帧循环侧每帧 drain 一批 chunk（`InferSystem` 已有容量调度位）。NPC 逐字出话、长生成进度条都靠它。
2. **工具调用（自主调控的机制底座）**：`ChatMessage` 扩展 `Tool` role 与 `tool_calls` 字段，`ToolSpec` 用 JSON Schema 表达。没有它，LLM 只能产文本不能操作引擎；"每一步由 AI 调控"= L9 Agent 通过 tool 协议调引擎能力。`DirectorSystem`（cognition.rs）是它的第一个消费者。
3. **结构化输出**：`ResponseFormat::JsonSchema`。引擎侧统一做"schema 校验 + 失败重试"循环；厂商不支持 json mode 时 adapter 降级为 prompt 注入 + 本地校验，对上层透明。关卡 JSON / 行为树 / beat 结构全依赖它。
4. **多模态输入**：content 从 `String` 升级为 `ContentPart::{Text, Image}`——为"渲染截图 → LLM 评审"的生成-评审回路留口。
5. **ProviderRegistry + manifest**：`register(id, factory)`；manifest 声明能力矩阵（context_len / tools / vision / json_mode / streaming）、价格档、延迟档、隐私（云/本地）。宿主（Studio）在装配期动态注册，核心零 SDK 不破。
6. **任务槽位路由表（"供用户选择"的正确粒度）**：不是全局选一个模型，而是按任务角色绑定：

   ```
   slot            → provider + fallback 链
   npc_dialog      → glm-4-flash → deepseek → Ollama → rule
   director        → glm-4.6 → claude → rule
   codegen         → deepseek → glm-4.6 → rule
   reviewer        → 与生成者异厂（生成-评审对抗）
   ```

   路由表是数据（配置/清单），引擎按槽解析 provider；Studio 已实测的 8 家预设 + 硬件体检双轨推荐就是它的 UI 雏形（`aicafe.llm.v1` 的语义升级版）。
7. **治理补全**：成本换算（$/1M token 表）、每槽预算（token + 金额）、语义缓存（prompt hash → response，`rule` 兜底即退化缓存）、每 provider 并发信号量与超时分级。`UsageMeter` 已有，补成本维度。

### 契约升级

`contracts/oriera` 增补 infer proto（消息/工具/流事件/槽位路由表），`ModelProvider` 本体保持最小——扩展走新版本号 `oriera.infer.provider.v2`，旧端点不动。

## 二、L8 生成式世界模型：两个物种，两个 trait

### 物种 A：一次性资产合成（Tripo / Seedance / Hunyuan3D / Trellis / Meshy）

`GenerationProvider` 的 **request → poll 任务制已是正确形状**（这些服务全是异步 create-and-poll）。补全：

1. `ProviderCapabilities` 扩展：多边形预算档、PBR 纹理、骨骼可绑定、reference-image 支持。
2. **风格一致性**：风格参考（图 / 场景 checkpoint）作为 L4 命名"风格资产"，请求自动携带——UGC 世界多模型产出不串风格的关键（Matrix-Game 的 sink/reference locking 的引擎化对应物）。
3. 深校验随 v0.2 资产解码器：网格合法性、LOD 链、碰撞体生成、meshopt 压缩（当前 `validate_asset` 只承诺魔数+尺寸，ADR 0003 已注明）。
4. 槽位化选择：角色 / 建筑 / 道具 / 载具各绑一家（同一张路由表思想）。

### 物种 B：交互式世界推演（Matrix-Game 4.0 / Genie 类 action-conditioned 实时生成）

长会话 + 逐步 step + 延迟上限——request/poll 形状不适用，需要**新 trait**：

```rust
/// L8 第二契约 oriera.genai.worldmodel.v1（草案）
pub trait WorldModelSession: Send + Sync {
    fn caps(&self) -> SessionCaps;  // fps、分辨率、动作空间、p99 延迟、style-checkpoint 支持
    async fn open(&self, seed: &WorldSeed) -> Result<SessionId, WorldModelError>;
    async fn step(&self, s: SessionId, act: Action, deadline: TickDeadline)
        -> Result<TickOutput, WorldModelError>;
    async fn close(&self, s: SessionId) -> Result<(), WorldModelError>;
}
// WorldSeed  = 风格资产 id + 首帧/参考图 + 世界文本描述
// TickOutput = 精化帧流 | Degrade(原生代理渲染)   // 降级是一等返回值，不是错误
```

工程结论（源自与 Matrix-Game 4.0 的对比研究，2026-10）：

1. **整数步 + backpressure 契约**：原生逻辑固定步长推进（120Hz），模型帧异步追赶；帧率不足 → 自动降级原生低模代理渲染。Matrix-Game 正是此架构（C++ 原生引擎跑逻辑/物理，DiT 只做神经渲染）。
2. **L5 落点**：`NeuralRefinePass` 进帧图；会话级滚动 KV cache 作为帧图 session-scoped 资源；切场景 = 热换 style checkpoint（L4）。
3. **端云路由**：A100/5090 级算力 → 只能云端 ExecutionSite 或本地旗舰；复用 L7 `route(caps, RouteHint)` 站点抽象；无 GPU 端 → 纯原生渲染，**接口不变**。
4. **三阶段落地**：① Python 推理 sidecar（localhost gRPC，契约先行）→ ② L5 Pass 集成 → ③ 内化为 L8 第二 trait + proto。传输三形态（in-process / localhost sidecar / cloud HTTPS）藏在 `Transport` 注入点后，与 tripo.rs 同模式。

## 三、自主调控闭环（把两个接口接起来）

- **大脑** = LLM（槽位路由），**手** = 生成模型（两个 trait），**桥** = 工具调用：把 `GenerationProvider.request_generation`、`WorldModelSession.step`、`validate_asset`、引擎参数注册为 LLM tools。
- **评价回路**：`validate_asset`（已有）→ 渲染冒烟 → 截图回 LLM 评审（多模态）→ 不合格自动改 prompt 重试；reviewer 槽用异厂模型形成对抗。
- **全链路预算**：帧预算（`InferSystem::with_frame_budget`）+ `GenAiLimits` + 每槽 token/金额预算，统一进 manifest 的 budgets/degrade（与机器可读清单治理一致）。

## 四、用户选择 UI（Studio）

现状：`{kind,key,model,base}` 单 provider 配置、8 家预设、硬件体检双轨推荐、keyring、热切换 reload。升级：单选 → **任务槽位矩阵**（每行 = 任务/推荐/已选/降级链/成本估计/云·本地标注）；3D 侧同构（角色/建筑/道具/世界推演槽）。引擎只定义路由表 schema，Studio 渲染。

## 分期

| 期 | 内容 | 层 |
|---|---|---|
| v0.5 | 流式 + 工具调用 + 结构化输出（infer proto v2）；槽位路由表数据化 | L7 |
| v0.5–0.6 | ProviderRegistry + manifest + 治理（成本/缓存/限流）；原生 HTTP adapter（ROADMAP v0.6 位） | L7 |
| v0.6 | 物种 A 能力扩展 + 风格资产；Studio 槽位矩阵 | L8/Studio |
| v0.7 | WorldModelSession sidecar（物种 B 契约先行） | L8 |
| 之后 | NeuralRefinePass 进帧图 → 内化 | L5/L8 |

## 想听到的意见

- 槽位路由表放引擎配置还是项目 manifest（每项目可覆盖）？
- `WorldModelSession` 的降级语义（Degrade 为一等返回值）是否进 RFC-1 的不变量清单？
- Seedance 退役式迁移（RFC-2）在槽位路由下的表达：槽绑 provider id，还是槽绑"能力档"由引擎解析？
