# oriera-pal — L1 平台抽象层

> PAL 是引擎唯一的 I/O 边界：图形、输入、存储、网络、XR 都在这里止步。
> 上层永远不写 `#[cfg(target_os)]`，只读 `Capabilities` 位图。

书章节：T4 · 清单：[`manifests/L1-pal.yaml`](../../manifests/L1-pal.yaml) · 契约：[`contracts/oriera/pal/v1/`](../../contracts/oriera/pal/v1/)

## 核心类型

| 类型 | 作用 |
|---|---|
| `PlatformProfile` / `Capabilities` | 启动时探测一次的平台档案与能力位图（u64） |
| `GfxBackend` | 图形后端边界（`begin_frame`/`submit_frame`），附 `NullGfxBackend` |
| `SpatialInputEvent` / `InputBody` | 所有输入归一化的事件流（键/指针/手/眼/语音/锚点） |
| `Storage` | 分层持久化（Volatile/Local），附内存实现 |
| `NetTransport` | 端云传输边界（断网时上层自动本地降级） |
| `XrSession` | OpenXR 会话表面（头姿/重投影查询） |

## 10 秒上手

```rust
use oriera_pal::{detect_platform, Capabilities};

let profile = detect_platform();
if profile.caps.contains(Capabilities::INPUT_EYE_GAZE) {
    // 眼动可用：开注视点渲染
}
```

## 五端

PC / Mobile / Web / AR 眼镜 / BCI（预留）。新增第六端只需要在
`capability.rs` 加一个构造函数，上层零改动（ADR 0001）。

## 延伸

- [ADR 0001：五端一等公民与能力位图](../../docs/adr/0001-five-platforms-capability-bitmap.md)
- 其余 crate 的 README 模板以本篇为准（见 issue #15）
