# 书 ↔ 仓库映射表

《OriEra 世界引擎——AGI 时代的软件 infra 层》（产业生态·技术架构与工程验证合编本）
是本仓库的架构规范来源。这张表把书的章节接到仓库的具体位置，读完一章就知道去看哪个目录。

## 技术篇（工程实现线）

| 书章节 | 主题 | 仓库位置 | 浏览器 demo |
|---|---|---|---|
| T2 | 系统论/控制论基础 | 全仓库的组织方式（层=责任边界） | — |
| T3 | 十一层总览 | [docs/ARCHITECTURE.md](ARCHITECTURE.md) | [09-full-arch](../examples/09-full-arch/index.html) |
| T4 | L1 平台抽象层 PAL | `crates/oriera-pal`、`contracts/oriera/pal/v1` | [01-hello-triangle](../examples/01-hello-triangle/index.html) |
| T5 | L2 核心系统层 | `crates/oriera-core`（调度器；内存/回放见 ROADMAP） | [02-ecs-spawn](../examples/02-ecs-spawn/index.html) |
| T6 | L3 对象模型 ECS | `crates/oriera-ecs` | [08-ecs-visualizer](../examples/08-ecs-visualizer/index.html) |
| T7 | L4 资产管线 | `crates/oriera-asset`（生成式管线属 L8） | [05-gltf-model](../examples/05-gltf-model/index.html) |
| T8 | L5 渲染内核 | `crates/oriera-render` | [07-webgpu-particles](../examples/07-webgpu-particles/index.html) |
| T9 | L6 子系统服务 | `crates/oriera-subsys`（物理） | [03-physics](../examples/03-physics/index.html) |
| T10 | L7 推理运行时 | `crates/oriera-infer` | [09-full-arch](../examples/09-full-arch/index.html) |
| T11 | L8 生成式 AI | 📋 ROADMAP 2028 | — |
| T12 | L9 Agent 层 | `crates/oriera-agent` | [04-agent-chat](../examples/04-agent-chat/index.html)、[06-agent-physics](../examples/06-agent-physics/index.html) |
| T13 | L10 AR/VR 交互 | 📋 ROADMAP 2027 H2 | — |
| T14 | L11 应用与 UGC | `platforms/*`、`examples/*` | 全部 |
| T15 | 横切：观测/安全/计量 | 各层埋点、[SECURITY.md](../SECURITY.md) | — |
| T16–T17 | 全局动力学/测试基准 | `docs/PERF.md`、CI | — |
| T18–T20 | 演化路线/风险/统筹 | [ROADMAP.md](../ROADMAP.md) | — |

## 产业篇（生态协同线）

| 书章节 | 主题 | 与仓库的关系 |
|---|---|---|
| 第 3 章 | 十一层与中国 AI 产业链映射 | README 的架构图即其工程投影 |
| 第 8 章 | PAL 作为硬件-软件解耦层 | ADR 0001（能力位图五端接入） |
| 第 12 章 | MCP 协议与 Agent 生态 | `crates/oriera-agent` 的 v1 规则解析 → LLM 路线 |
| 第 14 章 | UGC 与创作者经济 | L11 同源编辑器（ROADMAP） |

## 附录

| 附录 | 主题 | 仓库位置 |
|---|---|---|
| B | 各层接口契约速查 | `contracts/`（proto）+ `manifests/`（每层清单） |
| C | 性能基准与延迟预算 | `manifests/*.yaml` 的 budgets 段、`docs/PERF.md` |
| F | 增量实施路线与验收清单 | `ROADMAP.md` |

## 一句话

> 书定义「应该是什么样」，仓库实现「现在是什么样」，
> ROADMAP 记录「差距还剩多少」。三者的偏差本身就是 bug。
