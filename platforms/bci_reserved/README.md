# BCI 预留说明（v1 不实现）

脑机接口在 OriEra 中定位为 **L10 远期高阶意图通道**，对应系统演化路线图的
**阶段三（2031–2035）：物理-数字统一世界模型期**。

## 当前状态

- [x] 在 `oriera-pal::capability::Capabilities` 预留 bit 位（`BCI_EEG_LSL`, `BCI_NON_INVASIVE`）
- [x] 在 `contracts/oriera/pal/v1/spatial_input.proto` 预留 `BciIntentEvent` 消息
- [ ] v1 不接入任何真实 BCI 硬件
- [ ] v1 不进入 11.1ms 帧预算（脑机数据是低频、高延迟异步通道）

## 合规边界（写死）

| 类型 | 监管路径 | v1 是否碰 |
|---|---|---|
| 侵入式 BCI | NMPA 三类医疗器械（2026-06-30 指导原则） | **绝不** |
| 非侵入消费级 EEG | 松绑，但需符合《个保法》《GDPR》 | 远期 |

## 未来接入（阶段三）

- 驱动：BrainFlow + LSL（Lab Streaming Layer）做时间同步
- 数据流：EEG 采样 → 特征提取 → 意图解码 → `BciIntentEvent` → L10 交互总线
- 与语音同级：作为多模态意图的一路输入，不替代手/眼/语音
