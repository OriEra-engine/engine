# oriera-agent — L9 Agent 层（认知面）

> NPC 为什么说这句话、世界为什么推进到下一天，都是引擎系统在跑，不是应用层临时拼 prompt。

书：T12 · 清单：[`manifests/L9-agent.yaml`](../../manifests/L9-agent.yaml)

## 层内职责

- `CognitionSystem`：每 N 个 tick 让 NPC 说一次话，prompt 由长时记忆和当前 world beat 拼出来，经 L7 的 `ModelProvider` 出去。
- `DirectorSystem`：从对话推进世界故事（beat / day）。
- `MemoryStore`：按 agent 分开的片段记忆，关键词 + 新近度 + 重要度检索，每 agent 容量可调（默认 60）。
- `parse`：规则式意图解析（v1），把一句话翻译成 L7 的 `Command`——LLM 路线接进来之前，命令面先立住。
- 具身仿真面已按 ADR 0004 拆到 `oriera-embodied`，这里只留兼容层 re-export。

## 核心 API

| 名称 | 作用 |
|---|---|
| `parse` | 一句话 → `oriera_infer::Command`，解析不了给 `Unknown` |
| `CognitionSystem` / `DirectorSystem` | 认知与导演，注册进 L2 调度器即可 |
| `MemoryStore::record` / `retrieve` | 记一笔 / 按查询取最相关的几笔 |
| `spawn_prefab` / `PrefabSpec` | 预制体声明式生成（Godot PackedScene 的 ECS 翻译） |
| `embodied_env` / `spec` / `eval` | 具身面的兼容 re-export（真身在 `oriera-embodied`） |

## 10 秒上手

```rust
use oriera_agent::parse;
use oriera_infer::InferSystem;

let mut infer = InferSystem::new();
infer.submit(parse("spawn 10 particles"));
infer.submit(parse("把重力设为 9.8"));

assert_eq!(infer.processed, 0); // 下一个 scheduler tick 才执行
assert_eq!(infer.rejected, 0);   // 两句都认得
```

## 延伸

- [ADR 0004：认知面与仿真面拆分](../../docs/adr/0004-l9-oriera-embodied-split.md)
- [ARCHITECTURE · L9](../../docs/ARCHITECTURE.md)
