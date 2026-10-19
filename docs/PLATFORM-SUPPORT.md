# 多平台适配与验证边界

## 当前能力

| 平台 | 当前入口 / 能力 | 验证方式 |
|---|---|---|
| Windows / macOS / Linux | PC 无窗口运行闭环；按 OS 选择图形 API 候选 | CI 各系统运行 Rust 测试、构建、Clippy |
| 浏览器 | Wasm 启动入口、异步能力探测；HTML/JS demo 独立运行 | Wasm 构建、Chrome clock/host smoke test、JS 降级测试 |
| Android / iOS | 平台档案与引擎库；尚无原生 App 宿主 | 目标交叉 `cargo check`，不等于设备运行 |
| visionOS / Android AR | 平台档案；跟踪等能力由实际会话提供 | 当前无设备或 SDK 验证，不宣称可运行 |
| WASI | unknown 档案，绝不误判为浏览器 | 引擎库交叉 `cargo check` |
| BCI | 预留位，默认关闭 | 单元测试 |

## 能力与预算

`detect_platform()` 只选择 OS 基线，不能证明驱动或硬件可用。
原生 graphics 位表示该 OS 的 API 候选，创建后端时仍须处理失败；
Windows 为 DX12/Vulkan，macOS/iOS/visionOS 为 Metal，Linux/Android 为 Vulkan。
不再从 OS 推断 NPU TOPS、显存容量、眼动、手势、注视点渲染或权限。
平台壳必须在实际后端/会话成功后更新相关能力位，才能供上层使用。

`vram_budget_mb` 是可调整的应用分配预算，不是物理显存检测结果。
默认 PC 为 1024 MB、手机/Web/AR 为 512 MB。手机默认 30fps 预算，
PC/Web 60fps、AR 90fps；这只是调度目标，并不承诺设备能达到该帧率。

浏览器默认能力为空。`boot_detected()` 会检查安全上下文、触屏、精细指针、
WebTransport API 及 WebGPU adapter。GPU 请求异常、空结果或 1.5 秒超时均关闭
WebGPU 位。不会请求麦克风、XR 等权限。WebTransport 位不代表远端可连接；
WebGPU 位不代表 device 创建成功，渲染器仍需处理 device loss。

## 本地验证

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
node --test platforms/web/src/probe.test.mjs
rustup target add wasm32-unknown-unknown aarch64-linux-android aarch64-apple-ios wasm32-wasip1
cargo build -p oriera-web --target wasm32-unknown-unknown
cargo check --workspace --exclude oriera-pc --exclude oriera-web --features oriera-genai/providers,oriera-agent/json,oriera-agent/rigid --target aarch64-linux-android
# 同样检查 aarch64-apple-ios / wasm32-wasip1 / wasm32-unknown-unknown。
# native-http 仅原生宿主测试，不进入可移植目标检查。
wasm-pack test --headless --chrome platforms/web
```

## 使用 Wasm 入口

```sh
wasm-pack build platforms/web --target web --out-dir pkg
python3 -m http.server 8000
```

在通过 localhost/HTTPS 提供的 ES module 中：

```js
import init, { boot_detected } from "/platforms/web/pkg/oriera_web.js";
await init();
const profile = JSON.parse(await boot_detected());
const caps = BigInt(profile.capabilities); // 保留 u64 全部精度
```

旧的同步 `boot()` 保留，但返回保守基线。新的探测 API 尚未自动替换所有
独立 HTML demo 的平台展示/渲染逻辑。共享时钟使用 `web-time`，避免浏览器
`std::time::SystemTime::now()` panic；原生平台仍使用标准库实现。帧间隔与每个系统的耗时使用单调 `Instant`
（浏览器为 `performance.now()`），避免墙钟调整干扰调度预算。

## 迁移说明

本次纠正默认能力/预算会改变旧行为：`web()` 不再默认打开 WebGPU，
`mobile()` 不再保证 NPU/8GB 显存，AR 不再保证跟踪权限；手机帧预算改为 30fps。
依赖旧默认值的宿主应在实际探测/初始化成功后显式设置 `caps`，或通过浏览器
`boot_detected()` / PAL `web_with_capabilities()` 获取结果。若业务已有设备测量
数据，可显式配置 `frame_budget_us` 和 `vram_budget_mb`，不要把分配预算当作硬件容量。
固定步长 `with_fixed_dt()` 的仿真语义保持不变，只有真实耗时统计改用单调时钟。
