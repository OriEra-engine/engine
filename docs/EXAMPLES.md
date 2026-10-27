# 示例地图 — Examples Map

> demo ↔ 十一层 ↔ crate 三列对照。浏览器入口：https://oriera-engine.github.io/engine/
>（本表由 `scripts/check_numbers.py` 对拍实物目录，数字漂移 CI 即红。）

| Demo | 层 | 主 crate | 一句话 | 本地入口 |
|---|---|---|---|---|
| 01 Hello Triangle | L1+L5 | oriera-render | WebGPU 清屏三角形——渲染管线 Hello World | `examples/01-hello-triangle/` |
| 02 ECS Spawn | L3 | oriera-ecs | 20 万粒子 SoA ECS——数据驱动架构上限 | `examples/02-ecs-spawn/` |
| 03 Physics Sandbox | L6 | oriera-subsys | 重力+碰撞+鼠标拖拽——实时物理沙盒 | `examples/03-physics/` |
| 04 Agent Chat | L9→L3 | oriera-agent | 自然语言控制粒子世界（规则后端离线可跑） | `examples/04-agent-chat/` |
| 05 glTF Model | L4+L5 | oriera-asset | Three.js 加载带动画 3D 角色 | `examples/05-gltf-model/` |
| 06 Agent Physics | L9×L6 | oriera-agent | 自然语言指挥物理世界（「生成 20 个」「关掉重力」） | `examples/06-agent-physics/` |
| 07 WebGPU Particles | L5 | oriera-render | GPU 计算百万粒子流场 | `examples/07-webgpu-particles/` |
| 08 ECS Visualizer | L3 | oriera-ecs | 实体-组件-系统数据流可视化 | `examples/08-ecs-visualizer/` |
| 09 Full Architecture | L1-L11 | 全链 | 一次看懂分层协作全貌 | `examples/09-full-arch/` |
| 10 言灵世界 | L9 | oriera-agent | 文字驱动世界生成与演化（所说即所见） | `examples/10-word-spirit/` |
| 11 隅角咖啡馆 | L7-L11 旗舰 | oriera-agent+genai | 3D AI NPC：记忆/聊天/导演（配 key 后大模型驱动） | `examples/11-ai-cafe/` |
| 12 记忆图谱 | L9 | oriera-agent | MemoryStore 检索式记忆可视化（写入/衰减/召回） | `examples/12-memory-atlas/` |
| 13 生成式画廊 | L8+L4 | oriera-genai | 文生图/文生 3D + 命名热替换 | `examples/13-genai-gallery/` |
| 14 影游内核 | L9 | oriera-agent | 互动影游叙事原语的浏览器镜像（与 Rust 逐句对照） | `examples/14-film-game/` |

## 代码片段（库用户最小路径）

```rust
// L9 NPC 认知最小回路（rule 后端离线即可跑，无需任何 key）
use oriera_agent::cognition::CognitionSystem;
use oriera_infer::provider::{ModelProvider, RuleProvider};
use std::sync::Arc;

let provider = Arc::new(RuleProvider::new());
let mut cog = CognitionSystem::new(provider, 1); // 1 = 轮询间隔（tick 数，最小 1）
// cog.run(&ctx, &mut world); —— 每 tick 驱动一次 NPC 认知
```

## 离线验活

```bash
cargo run -p oriera-infer --example provider_smoke        # 供应商预设离线自检
ORIERA_SMOKE_PROVIDER=zhipu cargo run -p oriera-infer --example provider_smoke
node --test examples/11-ai-cafe/engine/kernel.test.mjs    # 浏览器内核 38 条
```
