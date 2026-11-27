# 第21刀 · InferJobs 尾延迟分位数离线计量

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

补丁 **021**。基于 `0387718`（v0.13.0），顺序叠在 001–020 之后。纯 bench，零生产代码改动。

---

## 一、动机

既有 InferJobs 基准只有中位数与三次 n=1 的单次读数，p99 缺失。线程池 RFC 是否立项、Realtime deadline 余量留多少，共同卡在这个输入上：中位数好看不代表尾部可控。这一刀把 submit+drain 全路径的 p50/p95/p99 量出来，并发档 1/4/16，n≥3 轮，同时报轮间波动。

后端用离线 `RuleProvider`，不接远程；数字里不含任何模型/网络时间，纯编排层自身开销。

---

## 二、改动面

只动 `crates/oriera-infer/benches/jobs.rs`：

- `drain_all` 泛型化到 `<P: ModelProvider + 'static>`，不再绑死 `InstantBackend`；
- 新增分位数 helper `percentile`；
- 新增诊断档 `bench_tails`，`-- --nocapture tails` 触发；既有 criterion 档（poll_empty / submit_drain / burst）一行未动。

没有改 `src/` 生产代码，没有改 Cargo.toml（bench 目标 011 已注册）。

---

## 三、实测数字

跑法：release 构建，独立 `CARGO_TARGET_DIR`，离线 RuleProvider，池在每轮内复用（稳态口径，不含每轮新建池的冷启动），3 轮 × 60 样本，预热 10 轮。

```sh
cargo bench -p oriera-infer --bench jobs -- --nocapture tails
```

| 并发 | p50 µs | p95 µs | p99 µs | 轮间 p50 极差 µs | 每 job p50 µs |
|---|---|---|---|---|---|
| 1 | 31.9 | 59.2 | 89.3 | 8.4 | 31.9 |
| 4 | 100.5 | 500.4 | 1106.6 | 8.4 | 25.1 |
| 16 | 349.9 | 896.1 | 1284.7 | 22.7 | 21.9 |

读数怎么读：

- p50 每 job 22–32 µs，随并发略摊薄，总量线性增长；
- 尾延迟在并发 4/16 明显抬升，p99 到 1.1–1.3 ms，是 OS 线程 spawn 竞争的尾方差，不是吞吐问题；
- 轮间 p50 极差：1/4 并发 8.4 µs，16 并发 22.7 µs——高并发档轮次波动同步放大。

这批证据供两个待拍板项引用：线程池 RFC 要不要立项（收益主要是收敛尾方差，不是省 CPU），Realtime deadline 下限按尾部而非中位数留余量。

---

## 四、诚实边界

- 测量的是 submit + drain 全路径（spawn + channel 往返 + 收割），worker 内 `complete` 由 RuleProvider 立即返回；真实远程后端的网络等待不在内。
- 含 OS 线程 spawn，跨机器、跨内核不可直接比，只作同机回归基线。
- p99 受调度抖动支配，单次跑的尾部位置会漂移；轮间极差已单列，不要把 p99 当稳态常数。
- 复用池口径；每轮新建池的冷启动成分在 018 整机对照里量过，两者中位数接近，本刀不复测。
- 全部数字为内部基线，未标定硬件前不对外。

---

## 五、验收

| 项 | 结果 |
|---|---|
| `cargo test --workspace` | 656 passed / 0 failed / 4 ignored（本刀无新增测试） |
| clippy 全 feature `-D warnings` | 零告警 |
| 默认 feature 新增 bench | 零告警 |
| `cargo fmt --all -- --check` | 干净 |
| JS 镜像 | 130 pass / 0 fail |

补丁规模：1 文件，+85/-4。

## 六、依赖

叠在 001–020 之后；只依赖 011 注册的 `jobs` bench 目标与 005 的 `InferJobs` 结构，无其它文件耦合。
