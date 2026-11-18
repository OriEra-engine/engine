# oriera-infer — L7 推理运行时

> 所有层的模型调用都从这里出去，也只能从这里出去：应用不持有 key、不认识厂商 URL。

书：T10 · 清单：[`manifests/L7-infer.yaml`](../../manifests/L7-infer.yaml) · 契约：[`contracts/oriera/genai/v1/`](../../contracts/oriera/genai/v1/)（L8 侧的生成契约）

## 层内职责

- `ModelProvider` 是唯一模型边界：`RuleProvider` 离线规则、HTTP 供应商在平台壳里装配，引擎对厂商零硬编码。
- `InferSystem` 是命令队列：每帧执行 `Command`（spawn / 清场 / 设重力 / 传送 / 计数 / 改速度 / 单实体施加冲量），并记账 `processed` / `rejected` / `deferred` / `coalesced`。
- 自然语言解析不在这里——那是 L9 的事。这层只执行拿到手的命令。
- `health` 做熔断与降级：供应商挂了自动落到下一个，状态可查。

## 核心 API

| 名称 | 作用 |
|---|---|
| `InferSystem::submit` | 塞一条 `Command`，相邻可合并的会合并（合并是确定性等价的） |
| `processed` / `rejected` | 已执行 / 未识别（`Unknown` 只计数，不执行） |
| `ModelProvider` + `RuleProvider` | 模型边界与离线实现，测试与 demo 都用后者的 |
| `PROVIDER_PRESETS` | 供应商预设表，单一来源是 `contracts/golden/presets.json` |
| `FallbackProvider` / `ProviderHealth` | 熔断、降级与健康状态 |

## 10 秒上手

```rust
use oriera_infer::{Command, InferSystem, RuleProvider};

let mut infer = InferSystem::new();
infer.submit(Command::Spawn(100));
infer.submit(Command::SetGravity(9.8));
assert_eq!(infer.rejected, 0);
assert_eq!(infer.processed, 0); // 命令要等下一次 scheduler tick 才落进世界

let _offline = RuleProvider::new(); // 不联网、不要 key，先跑通链路
```

## 延伸

- [OPEN-MODEL-MATRIX.md](../../docs/OPEN-MODEL-MATRIX.md)：预设表与各家的取舍
- [ARCHITECTURE · L7](../../docs/ARCHITECTURE.md)
