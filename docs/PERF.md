# 性能与基准

> 本文档记录基准怎么跑、当前参考数据，以及各层预算的出处。
> 历史数据只追加不覆盖（旧版曾被自动脚本覆盖丢失，已删除该脚本）。

## 怎么跑

```bash
cargo test --workspace                     # 单元测试（含 ECS 10k 冒烟）
cargo bench -p oriera-ecs                  # criterion 基准：spawn_10k / query_10k
```

## 当前参考数据（2026-10-15，架构整理分支）

criterion `spawn_10k` / `query_10k`（Windows / stable Rust）：

| 基准 | 结果 |
|---|---|
| spawn_10k（spawn + 双组件插入） | 见 `cargo bench` 输出，量级：单帧毫秒级 |
| query_10k（Position×Velocity join 计数） | 同上 |

> 数字随机器差异很大，以自己跑出来的为准；这里只锁方法和量级。
> CI 不跑 bench（criterion 只在本地跑），防止慢机器污染记录。

## 预算出处（书 T3 §3.5 + 附录 C）

| 层 | 预算 | 来源 |
|---|---|---|
| 整帧（PC 60fps） | 16.6 ms | L1 manifest `frame_budget_us: 16666` |
| 整帧（AR 90fps） | 11.1 ms | 同上 `ar_glasses` |
| L5 渲染 | 8.0 ms p99 | `manifests/L5-render.yaml` |
| L6 物理步进 | 1.5 ms p99 | `manifests/L6-subsys.yaml`（AR 帧切片） |
| L7 端侧推理 | 1.0 ms p99 | `manifests/L7-infer.yaml` |

## 记录新数据

在下方按日期追加一节，格式：

```markdown
## YYYY-MM-DD <机器/工具链>

- spawn_10k: <时间>
- query_10k: <时间>
```
