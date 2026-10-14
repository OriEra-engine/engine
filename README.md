# OriEra 世界引擎

> AI 原生、虚实融合、可自演化的开放数字世界运行时。
> 十一层分层架构 + 横切治理，五端一等公民（PC / 移动 / 网页 / AR 眼镜 / 脑机预留）。

**🌐 在线体验 Live Demos: https://oriera-engine.github.io/engine/**

## 设计哲学

- **分层解耦**：每层对上提供稳定接口，跨层走事件总线，禁止跨层直读私有字段。
- **契约先行**：跨语言接口用 Protobuf / JSON Schema，CI 做兼容性门禁。
- **局部自治**：每层自己的调度、缓存、降级策略；下层自主决定端侧还是云端执行。
- **负反馈控熵**：帧超时→降负载，显存超限→驱逐神经 LOD，推理慢→降级小模型。
- **五大约束**：延迟（人机闭环 <20ms / AR 帧 <11.1ms）、功耗、内存（端侧 8GB）、算力、合规。

## 十一层架构

```
L11  应用层 / UGC 创作生态 / 同源编辑器
L10  AR/VR 交互层：多模态感知（手/眼/语音/体/脑机预留）
L9   AI Agent 层：认知闭环 / 记忆 / MCP 工具 / AI 导演
L8   生成式 AI 层：文生 3D / 空间约束生成 / 校验修复
L7   AI 推理运行时：端云协同调度 / 模型生命周期 / Trace
L6   子系统服务：物理 / 动画 / 空间音频 / UI / 网络同步
L5   渲染内核：Render Graph / 混合几何（网格 + 3DGS + NeRF）
L4   资源资产管线：ResourceHandle / 六段式 AI 生成管线
L3   对象模型层：ECS / WorldObject / Scene / Signal 总线
L2   核心系统层：调度器 / 分层内存 / 确定性回放
L1   平台抽象层 PAL：OriGFX / 输入 / 存储 / 网络 / OpenXR
     横切：观测 Trace · 安全护栏 · 资源计量 · 调试回放
```

## 五端接入

| 端 | 目录 | 状态 |
|---|---|---|
| PC (Win/macOS/Linux) | `platforms/pc/` | 首发 |
| Mobile (iOS/Android) | `platforms/mobile/` | 首发 |
| Web (Wasm + WebGPU) | `platforms/web/` | 首发 |
| AR 眼镜 (visionOS / 骁龙 / WebXR) | `platforms/ar_glasses/` | 重点适配 |
| 脑机接口（预留） | `platforms/bci_reserved/` | 仅 trait，v1 不实现 |

## 目录速览

```
oriera/
├── contracts/      跨语言契约（Protobuf + JSON Schema），单源真理
├── manifests/      每层 LayerManifest.yaml（架构门禁自动读）
├── crates/         Rust 引擎主代码（L1–L7 实时层）
├── platforms/      五端入口薄壳
├── services/       云端/边缘服务（vLLM、文生3D 路由、同步）
├── ai/             Python 离线/近线 AI 组件
├── sdk/            对外 TS/Python SDK + MCP 工具集
├── editor/         同源编辑器（TS + WebGPU）
├── crosscutting/   观测/安全/计量/调试
├── examples/       逐步 demo（01 → 07）
├── docs/           ADR、C4 架构图、Runbook、基准
├── tests/          一致性/混沌/回放/E2E
└── tools/          架构 lint、契约生成、资产烘焙
```

## 快速开始

### Web 最小 demo（无需 Rust，浏览器直接打开）

```bash
cd examples/01-hello-triangle
# 用任意静态服务器起一下
npx serve .
# 浏览器打开 http://localhost:3000
```

或者直接双击 `index.html`（需要支持 WebGPU 的 Chrome/Edge 113+）。

### Rust 引擎（需要 Rust 1.80+）

```bash
cargo run -p oriera-pc           # PC 端清屏窗口
cargo test --workspace            # 跑全部单元测试
cargo clippy --workspace -- -D warnings
```

## 开源协议

Apache License 2.0 — 见 [LICENSE](./LICENSE)。
