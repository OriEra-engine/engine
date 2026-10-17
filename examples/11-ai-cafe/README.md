# 11 · 隅角咖啡馆 — AI 内嵌于引擎架构的 3D NPC 世界

> 认知、记忆、世界进程是**引擎系统**；应用只装配服务、渲染场景。
> 大模型通过 L7 `ModelProvider` 边界接入——应用代码里没有一次模型调用。

[▶ 本地运行](http://127.0.0.1:8123/examples/11-ai-cafe/)：`python -m http.server` 后打开本页。
右上 ⚙ 填入智谱 API Key（只存本浏览器 localStorage，仓库不含任何密钥）即点亮大模型；
不填则整条链路自动降级为规则引擎，功能完整。**同一 Key 还会点亮 L8 生成式墙面**：
AI 导演每推进两拍剧情，两窗之间的墙上插画就按「此刻的故事」换新
（无 Key / 非 images 厂商 → 确定性程序化装帧；生成失败 → 保持原画不崩）。

## 架构（这是本 demo 的全部意义）

```
engine/kernel.js（引擎层，与 Rust crate 一一对照）      Rust 侧
├─ ZhipuProvider    L7 模型边界（浏览器直连智谱）   ↔  crates/oriera-infer/src/provider.rs
├─ MemoryStore      L9 长期记忆（检索式，localStorage 持久化）
│                                              ↔  crates/oriera-agent/src/memory.rs
├─ CognitionSystem  L9 认知（每 tick 一位 NPC 说话） ↔  crates/oriera-agent/src/cognition.rs
├─ DirectorSystem   L9 AI 导演（推进世界 beat/日）  ↔  同上
├─ Scheduler        L2 调度                       ↔  crates/oriera-core
│
├─ StoryboardArtSystem  L8 分镜画（剧情拍点→墙面插画）  ↔  oriera-genai 消费端
│   （自 ../13-genai-gallery/engine/genai.js 引入，与 Rust L8 镜像同源）
└─ GenAiSystem/AssetStore/SlotBindings  L8→L4→L5 生成热替换链

index.html（应用层，443 行）——Three.js 咖啡馆、NPC 化身、UI、客人插话。
它 import 引擎服务，不 import 任何 fetch/Key/prompt。
```

Rust 侧 headless 验证：`cargo run -p oriera-agent --example cafe`——同一套
System 在引擎循环里驱动对话，应用零 AI 逻辑。

## 玩法

- 三个 NPC（老周/Aria/阿哲）在 3D 咖啡馆里自主对话，人设由 prompt 驱动，
  台词引用各自检索到的长期记忆
- 每 8 回合 AI 导演推进一次世界事件（LLM 生成），NPC 随后围绕事件聊
- **刷新页面记忆不丢**：episodic memory 存 localStorage，可导出/清空
- 你可以「以客人身份插话」，NPC 会回应你并记住你说的内容
- 点击右侧 NPC 卡片查看它的全部长期记忆

## 3D 资产

- NPC：three.js 官方 RobotExpressive（CC 协议，含 Idle/Wave/Dance/Sitting 动画），
  已本地化到 `vendor/`（不依赖 CDN）
- 场景：程序化建模（吧台/咖啡机/桌椅/暖色吊灯），零外部场景资产
