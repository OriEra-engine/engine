# oriera-render — L5 渲染内核

> v0.1 先把「帧提交」这一层立住：数可见物体、按画质档合并 draw call、帧时间超了就自动降档。

书：T8 · 清单：[`manifests/L5-render.yaml`](../../manifests/L5-render.yaml)

## 层内职责

- `RenderSystem` 实现 `System<World>`，每帧把可见 `Position` 计数成一次提交量（真 Pass 图在后续版本，书 T8）。
- `Quality` 是这条链上最便宜的旋钮：`High` 不额外合并，`Medium` 4 个一 draw，`Low` 16 个一 draw。
- `observe_frame(avg_ms, budget_ms)` 做迟滞调节：连续 30 帧超预算降一档，连续 120 帧有余量升一档，中间地带两边计数都清零——避免在预算线附近来回跳。
- 只依赖 L2/L3，从不依赖 L6；渲染不该知道物理（层表见 ARCHITECTURE）。
- `render-wgpu` feature（默认关）提供第一个真 GPU 闭环：无头管线与 mesh 回读，和计数占位并存。

## 核心 API

| 名称 | 作用 |
|---|---|
| `RenderSystem::new` | 建渲染 system，初始画质 `High` |
| `quality` | 当前画质档（公开字段，观测与测试直接读） |
| `observe_frame` | 每帧调一次，喂滚动帧均值与预算，返回时可能已经换了档 |
| `gpu`（feature） | 无头 wgpu 管线，`render_mesh` 回读像素 |

## 10 秒上手

```rust
use oriera_render::{Quality, RenderSystem};

let mut render = RenderSystem::new();
assert_eq!(render.quality, Quality::High);

// 连续 30 帧超预算才降一档：这是迟滞，不是每帧都纠
for _ in 0..31 { render.observe_frame(20.0, 16.6); }
assert!(render.quality < Quality::High);
```

## 延伸

- [ARCHITECTURE · L5](../../docs/ARCHITECTURE.md) · [WASM-RENDERER.md](../../docs/WASM-RENDERER.md)
- 预算从哪来：[`oriera-core`](../oriera-core/README.md)
