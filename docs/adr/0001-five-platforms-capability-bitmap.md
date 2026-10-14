# ADR 0001: 五端一等公民接入与 PAL 能力位图

- 状态：已接受
- 日期：2026-10-14
- 决策者：架构组

## 背景

OriEra 需要同时支持 PC、移动、网页、AR 眼镜、脑机接口（预留）五类端。
早期方案是在代码各处写 `#[cfg(target_os = "...")]` 分支，但这会导致：

1. 上层模块直接感知 OS，分层被击穿；
2. 每加一个端就要改 N 个文件；
3. 能力差异（如 AR 眼镜有眼动、PC 没有）无法被上层统一处理。

## 决策

1. **五端都是一等公民**，不是"PC 主端 + 其他移植"。`platforms/` 下五个入口平级。
2. **PAL 启动时自报 `Capabilities` 位图**（`u64` bitflags），上层只读位图、不写 `cfg`。
3. **脑机接口只留 bit 位（40+）和 proto 消息**，v1 恒为 0，不进入帧预算，不接硬件。
4. **端云路由由下层自治**：L7 推理运行时根据位图 + 实时网络决定端侧还是云端执行，上层不感知。

## 后果

- ✅ 新增第六端只需在 `capability.rs` 加一个构造函数，上层代码零改动。
- ✅ 上层降级策略统一：查位图 → 无 `RT` 就走屏幕空间，无 `EYE` 就关闭注视点渲染。
- ⚠️ PAL 抽象开销必须 ≤5%（写在 L1 LayerManifest 预算里），否则热点路径要走 "bypass_pal_direct_backend" 降级。
- ⚠️ 能力位图是 `u64`，超过 64 个能力时要分块（当前预留到 bit 45，够用 5+ 年）。

## 参考

- `crates/oriera-pal/src/capability.rs`
- `manifests/L1-pal.yaml`
- `contracts/oriera/pal/v1/capability.proto`
