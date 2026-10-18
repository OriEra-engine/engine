# 开源底座矩阵 — Open Model Matrix

> 一句话理解：**每条赛道两排货架——开源底座拿来即用（认许可证），
> 大厂模型预留接口填 key 即接（认边界）。引擎两侧只认 L7/L8 两个
> 边界 trait，货架上放什么模型不关引擎的事。**
> （《OriEra 世界引擎》技术篇 T3「厂商零硬编码」的生态面）

本文是 [VERTICALS.md](VERTICALS.md) 八赛道的**模型供给侧**：逐赛道给出
经核实的开源底座（GitHub 仓库、许可证、接入层位）与大厂预留 API。
接入矩阵的机器可读形态 = L7 `PROVIDER_PRESETS`（13 行，与浏览器
kernel 一一对照）+ L8 `GenerationProvider` 适配器；冒烟一条命令：

```bash
cargo run -p oriera-infer --example provider_smoke                        # 离线行
ORIERA_SMOKE_PROVIDER=ollama cargo run -p oriera-infer --example provider_smoke   # 本地开源底座行
```

许可证速查：Apache-2.0 / MIT = 可商用可改造；社区许可（如腾讯混元 3D
Community License）= 可用但条款自读；GPL 工具（Blender）= 只做进程级
适配、不链接代码。

---

## 0. 总表：一屏看完

| 赛道 | 开源底座（首选） | 开源底座（次选） | 大厂预留 API |
|---|---|---|---|
| 1 游戏 | Qwen3（Apache-2.0，ollama/vLLM 起） | Stanford generative-agents（MIT，NPC 社会模拟） | 智谱 GLM / DeepSeek / Kimi（ProviderKind 已接） |
| 2 具身 | LeRobot（Apache-2.0，策略训练/评测） | Genesis（Apache-2.0，物理仿真）+ MuJoCo（Apache-2.0） | 端侧 NPU 推理 API（L1 route 预留） |
| 3 AIGC | TripoSR（MIT，图生 3D） | InstantMesh（Apache-2.0）· TRELLIS（MIT）· Hunyuan3D-2（社区许可） | Tripo / Seedance（L8 适配器已有） |
| 4 孪生 | ROS 2（Apache-2.0，设备桥） | OpenUSD（Apache-2.0，场景描述）· Eclipse Ditto（EPL-2.0，数字孪生服务） | AWS IoT TwinMaker / Azure Digital Twins（L1 net 适配） |
| 5 教育 | MiniCPM（Apache-2.0，端侧零合规） | Qwen3 本地行 | —（教育版钉死本地，合规即卖点） |
| 6 陪伴 | Mem0（Apache-2.0，记忆层） | Letta/MemGPT（Apache-2.0）· Zep/Graphiti（Apache-2.0） | 智谱 GLM API（zhipu 行已接） |
| 7 影视 | CogVideoX（代码 Apache-2.0，权重自定义） | OpenUSD + Blender（GPL，工具侧适配） | Seedance（L8 已有适配器） |
| 8 科学 | Mesa（MIT，Python ABM 互通） | — | —（科研自带模型，走 ModelProvider 自定义实现） |

## 1. 游戏 — NPC 的脑子开源化

- **Qwen3**（Apache-2.0，[QwenLM](https://github.com/QwenLM)）：当前本地
  开权重事实标准；`ollama pull qwen3:8b` 后 L7 preset `ollama` 行直连。
  NPC 对话量大（每 tick 都在说），本地推理把边际成本压到电费——
  分拨拨杆（v0.4）管住它别吃帧。
- **generative-agents**（MIT，[joonspk-research/generative-agents](https://github.com/joonspk-research/generative-agents)）：
  斯坦福「小镇」原始实现。它的记忆流/反思/规划循环正好是 L9
  `MemoryStore + CognitionSystem + DirectorSystem` 三件套的学术对照——
  v0.6 可按它的论文补「反思（reflection）」原语。
- **大厂预留**：智谱 glm-4-flash（免费档）/ DeepSeek / Kimi——
  云端行适合「开场剧本生成」这类低频高质调用。

## 2. 具身智能 — 世界的另一头是机器人

- **LeRobot**（Apache-2.0，[huggingface/lerobot](https://github.com/huggingface/lerobot)）：
  端到端机器人学习（VLA 模型 pi0.5、数据集、真机控制）。接入位：
  OriEra 当它的**仿真评测场**——世界状态经 L1 导出为 LeRobot 数据集
  片段，固定种子保证「同一策略同一世界可复现评测」（R1 场景 C 的
  逐位一致就是为此准备的）。
- **Genesis**（Apache-2.0，[Genesis-Embodied-AI/Genesis](https://github.com/Genesis-Embodied-AI/Genesis)）：
  自称比 Isaac Gym/MJX 快 10–80× 的物理平台。接入位：L6 的**第二物理
  后端**——`PhysicsSystem` trait 化后按 manifest 换芯（与 Jolt 计划
  同一条路）；Rust↔Python 经 L1 `RouteRemoteTask` 走进程边界。
- **MuJoCo**（Apache-2.0，google-deepmind/mujoco）：刚体物理金标准，
  学术策略的通用语。接入位：同上，L6 后端候选。
- **大厂预留**：端侧 NPU 推理 API（高通/联发科/Apple）——L1
  `route(caps, RouteHint)` 已把「隐私敏感钉端侧」写成一次决策，厂商
  SDK 以 ModelProvider 实现接入，不进引擎层。

## 3. AIGC — 生成流水线的开源与商用货架

- **TripoSR**（MIT，[VAST-AI-Research/TripoSR](https://github.com/VAST-AI-Research/TripoSR)）：
  单图→3D、秒级、MIT 最干净。接入位：L8 `GenerationProvider` 的
  **本地后端**（transport 注入本地推理进程，ADR 0003 模式）——
  「离线也能生成」是 Forge 垂直版的招牌行。
- **InstantMesh**（Apache-2.0，TencentARC/InstantMesh）与
  **TRELLIS**（MIT，microsoft/TRELLIS）：同为图/文生 3D 候选，
  质量取向不同，都适配做 L8 本地后端。
- **Hunyuan3D-2**（[Tencent-Hunyuan/Hunyuan3D-2](https://github.com/Tencent-Hunyuan/Hunyuan3D-2)）：
  两段式（裸网格→贴图）质量最强之一；**权重是社区许可非 MIT**，
  商用前自读条款——矩阵里明确标注，这是对贡献者的诚实。
- **大厂预留**：Tripo / Seedance 的 API 适配器已在 L8 落
  （feature 门控、零 HTTP、transport 注入）；CogView 已由社区 #57 接入。

## 4. 数字孪生 — 开源协议栈当输入输出

- **ROS 2**（Apache-2.0）：设备态经 ROS topic → L1 `net` 边界进世界；
  孪生「写回」命令反向同路。真机器人世界与镜像世界共用一套 Command
  词汇（L7）。
- **OpenUSD**（Apache-2.0）：孪生场景的层级/材质/时间轴描述事实标准；
  与 L4 内容寻址资产互转（USD 引用=句柄，prim hash=内容 hash）。
- **Eclipse Ditto**（EPL-2.0）：设备影子（digital twin）服务，
  接入位同 ROS 2——L1 storage/net 适配器。
- **大厂预留**：AWS IoT TwinMaker / Azure Digital Twins——孪生赛道
  企业客户的存量投资，经 L1 适配器接入，不成为引擎依赖。

## 5. 教育 — 开源底座就是合规声明

- **MiniCPM**（Apache-2.0，[openbmb/minicpm](https://github.com/openbmb/minicpm)）：
  端侧小模型（面壁出品），4–8B 档在学生笔记本上可跑。教育版**钉死
  本地行**（L1 route 隐私位）：零 key、零出网、零合规负担——
  「数据永不离开教室」不是承诺，是架构。
- **Qwen3 本地行**同理由可用（`lmstudio`/`ollama` 预设行）。
- **大厂预留**：无。教育赛道的卖点恰是**不接**大厂 API。

## 6. 陪伴 — 记忆层开源三杰

- **Mem0**（Apache-2.0，[mem0ai/mem0](https://github.com/mem0ai/mem0)）：
  记忆层（检索+摘要+持久化，框架无关 API）。接入位：L9
  `MemoryStore` 的**可选后端**——引擎内建记忆保留（确定性、零依赖），
  跨设备云端记忆走 Mem0 自托管，两者经 L1 storage 切换。
- **Letta/MemGPT**（Apache-2.0，[letta-ai/letta](https://github.com/letta-ai/letta)）：
  有状态 agent 运行时（记忆分页/自我编辑）。接入位：重型陪伴 agent
  的整宿托管选项；OriEra 的轻量内建路径（v0.4 注意力节拍器）覆盖
  不需要 OS 级记忆管理的场景。
- **Zep/Graphiti**（Apache-2.0）：图谱记忆（时间知识图谱），
  「角色关系随时间演化」的升级位。
- **大厂预留**：智谱 GLM API（zhipu 行已接）——云端高质量人设
  语音/文本；端侧 Qwen3-4B 走 ollama 行保隐私。

## 7. 影视预演 — 生成的下一站是镜头

- **CogVideoX**（[THUDM/CogVideoX](https://github.com/THUDM/CogVideoX)）：
  开源视频生成（代码 Apache-2.0，权重自定义许可）。接入位：L8
  `GenerationProvider` 的 video kind（Seedance 适配器已定义的 JSON
  shape 可直接复用给本地 CogVideoX）。
- **Blender**（GPL 工具）：只做进程级适配（导出 USD/GLB 给 L4），
  引擎不链接其代码——GPL 边界干净。
- **确定性逐帧**：改一镜只重渲那一镜依赖 L2 固定种子 tick +
  L4 内容寻址缓存（`with_fixed_dt` 在 v0.5 缺口清单）。
- **大厂预留**：Seedance（L8 适配器已有）——预演终稿的商用高质量行。

## 8. 科学 — 互通优先于替代

- **Mesa**（MIT，[projectmesa/mesa](https://github.com/projectmesa/mesa)）：
  Python ABM 标准库。接入位：**互通不替代**——OriEra 世界快照
  （L3 → JSON）可作 Mesa 的初始种群；Mesa 跑完的结果可作 OriEra 的
  世界事件回放（确定性链路两头咬合）。大规模实体性能走 OriEra
  （demo 02 的 20 万粒子），分析生态走 Mesa。
- **大厂预留**：无——科研模型自带（自训练 policy、统计模型），
  经 `ModelProvider` 自定义实现进 L7，引擎不挑食。

## 9. 接入矩阵的三个铁律

1. **开源底座进本地行，大厂进预留行**——表里每行要么 🔌 要么 ☁️，
   不存在「既是开源又必须出网」的含糊行；教育/陪伴的隐私承诺靠
   这条分类成立。
2. **引擎只认两个边界 trait**——L7 `ModelProvider`（文本/认知）与
   L8 `GenerationProvider`（资产生成）。货架换血不需要引擎发版。
3. **许可证写进矩阵**——社区许可/自定义权重许可明确标注「非 MIT，
   商用自读」；矩阵是给贡献者的尽调起点，不是法律意见。

> 本文的行目会随生态漂移，维护方式：PR 改 `PROVIDER_PRESETS`
> （L7）与本文表格，冒烟 harness 自动验证预设行可解析。
