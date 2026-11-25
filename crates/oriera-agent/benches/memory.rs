//! L9 记忆检索微基准 — `MemoryStore::retrieve` 离线打分路径。
//!
//! 场景（单 agent，窗口满载，命中比固定）：
//! - `retrieve/local_n{n}_k{k}`：默认 `LocalMemoryBackend`，窗口 60/120/240，
//!   取 top-k 5/10；
//! - `retrieve/layered_s{short}_l{long}`：`LayeredMemoryBackend`，两层合并打分
//!   （short 60 / long 120 默认，及 short 120 / long 240 翻倍档）；
//! - `record/local_n60`：一次 `record`（写 + 窗口裁剪）的单条摊销成本。
//!
//! 只量离线关键词打分——语义档 `retrieve_embedded` 走 embedder，其成本在
//! 远程调用侧，不在此基准范围。
//!
//! 大窗口分位数诊断档：
//! ```sh
//! cargo bench -p oriera-agent --bench memory -- --nocapture largemem
//! ```
//! 中文夹具 240/1000/2000/5000/10000/20000 条报 p50/p95/p99，并在
//! n=1000 档跑英文短词同构对照（同条目数、同命中比、同测量方法）。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_agent::{
    ConsolidationPolicy, Episode, LayeredMemoryBackend, MemoryScoring, MemoryStore,
};
use std::time::Instant;

const TOPIC: &str = "咖啡馆 机器人 拿铁 订单 顾客";

fn episode(i: usize) -> Episode {
    // 每第 3 条与 TOPIC 同主题，保证固定命中比；其余用不重叠词避免假命中。
    let (about, text) = if i.is_multiple_of(3) {
        ("ada", format!("顾客{i}点了拿铁 机器人送单 咖啡馆 订单#{i}"))
    } else {
        ("bob", format!("库存{i} 清点 货架{i} 补货 标签{i}"))
    };
    Episode {
        turn: i as u64,
        day: 1,
        about: about.to_string(),
        text,
        importance: (i % 5 + 1) as u8,
    }
}

fn local_store(n: usize) -> MemoryStore {
    let mut store = MemoryStore::with_capacity(n.max(1));
    for i in 0..n {
        store.record("ada", episode(i));
    }
    store
}

fn layered_store(short: usize, long: usize) -> MemoryStore {
    use oriera_agent::MemoryBackend;
    // 巩固钩子挂在 backend trait 上，门面不暴露——按既有测试的驱动方式：
    // 先在裸后端上灌满并 consolidate，再注入门面。
    let mut backend = LayeredMemoryBackend::new(ConsolidationPolicy::default(), short, long);
    let total = short + long;
    for i in 0..total {
        backend.record("ada", episode(i));
    }
    backend.consolidate();
    MemoryStore::with_backend(short.max(1), MemoryScoring::default(), Box::new(backend))
}

// ── 英文短词同构对照夹具 ──────────────────────────────────────────
// 与中文夹具同条目数、同命中比（1/3）、同测量方法；差异只在词形：
// latin 连续字母 ≥3 整词成一个 gram，中文连续段拆相邻二元组。
// 用来把两形态的系数差归因到条目 gram 数 / 查询 gram 数，而不是字符长度。
const EN_TOPIC: &str = "forge market harbor gate";

fn en_episode(i: usize) -> Episode {
    let (about, text) = if i.is_multiple_of(3) {
        (
            "npc",
            format!("patron{i} at forge market harbor gate order {i}"),
        )
    } else {
        ("stall", format!("shelf{i} stock count restock label {i}"))
    };
    Episode {
        turn: i as u64,
        day: 1,
        about: about.to_string(),
        text,
        importance: (i % 5 + 1) as u8,
    }
}

fn en_store(n: usize) -> MemoryStore {
    let mut store = MemoryStore::with_capacity(n.max(1));
    for i in 0..n {
        store.record("npc", en_episode(i));
    }
    store
}

/// 样本分位数：`q ∈ [0,1]`，线性插值取最近秩（与既有 breakdown 同口径）。
fn percentile(samples: &mut [f64], q: f64) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((q * (samples.len() - 1) as f64).round() as usize).min(samples.len() - 1);
    samples[idx]
}

/// 大窗口 + 同构对照诊断档：`-- --nocapture largemem` 触发，裸 `Instant`
/// 采样报 p50/p95/p99；不干扰 criterion 的统计输出。
fn bench_large_window(c: &mut Criterion) {
    if !std::env::args().any(|a| a == "largemem") {
        // 占位 group，保持 criterion_main 结构完整。
        let mut g = c.benchmark_group("largemem_noop");
        g.sample_size(10);
        g.bench_function("noop", |b| b.iter(|| black_box(1u64)));
        g.finish();
        return;
    }

    const SAMPLES: usize = 100;
    const WARMUP: usize = 10;
    const K: usize = 10;

    let run = |store: &MemoryStore, agent: &str, about: &str, query: &str| -> [f64; 3] {
        let mut v: Vec<f64> = Vec::with_capacity(SAMPLES + WARMUP);
        for f in 0..WARMUP + SAMPLES {
            let t = Instant::now();
            black_box(store.retrieve(
                black_box(agent),
                black_box(about),
                black_box(query),
                black_box(10_000 + f as u64),
                black_box(K),
            ));
            v.push(t.elapsed().as_secs_f64() * 1e6);
        }
        v.drain(0..WARMUP);
        [
            percentile(&mut v, 0.50),
            percentile(&mut v, 0.95),
            percentile(&mut v, 0.99),
        ]
    };

    println!("\n────── 中文夹具大窗口检索（k={K}，离线关键词打分）──────");
    println!(
        "{:>8} {:>10} {:>10} {:>10} {:>10}",
        "n", "p50 µs", "p95 µs", "p99 µs", "µs/条"
    );
    for n in [240usize, 1_000, 2_000, 5_000, 10_000, 20_000] {
        let store = local_store(n);
        let [p50, p95, p99] = run(&store, "ada", "ada", TOPIC);
        println!(
            "{n:>8} {p50:>10.1} {p95:>10.1} {p99:>10.1} {:>10.2}",
            p50 / n as f64
        );
    }

    println!("\n────── 同构对照：中文 vs 英文短词（n=1000，k={K}，同命中比 1/3）──────");
    let zh = local_store(1_000);
    let en = en_store(1_000);
    let [zh50, zh95, zh99] = run(&zh, "ada", "ada", TOPIC);
    let [en50, en95, en99] = run(&en, "npc", "npc", EN_TOPIC);
    println!(
        "{:>6} {:>10} {:>10} {:>10} {:>10}",
        "形态", "p50 µs", "p95 µs", "p99 µs", "µs/条"
    );
    println!(
        "{:>6} {zh50:>10.1} {zh95:>10.1} {zh99:>10.1} {:>10.2}",
        "中文",
        zh50 / 1_000.0
    );
    println!(
        "{:>6} {en50:>10.1} {en95:>10.1} {en99:>10.1} {:>10.2}",
        "英文",
        en50 / 1_000.0
    );
    println!(
        "系数比（中/英，p50）：{:.2}×",
        (zh50 / 1_000.0) / (en50 / 1_000.0)
    );
    println!("注：内部基线，未标定硬件与构建形态前不对外。\n");
}

fn bench_retrieve(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_retrieve");

    // ── LocalMemoryBackend：窗口 × k ──
    for n in [60usize, 120, 240] {
        let store = local_store(n);
        for k in [5usize, 10] {
            group.bench_with_input(BenchmarkId::new(format!("local_n{n}"), k), &k, |b, &k| {
                b.iter(|| {
                    let hits = store.retrieve(
                        black_box("ada"),
                        black_box("ada"),
                        black_box(TOPIC),
                        black_box(10_000),
                        black_box(k),
                    );
                    black_box(hits)
                })
            });
        }
    }

    // ── LayeredMemoryBackend：两层合并 ──
    for (short, long) in [(60usize, 120usize), (120, 240)] {
        let store = layered_store(short, long);
        group.bench_with_input(
            BenchmarkId::new(format!("layered_s{short}_l{long}"), 10usize),
            &10usize,
            |b, &k| {
                b.iter(|| {
                    let hits = store.retrieve(
                        black_box("ada"),
                        black_box("ada"),
                        black_box(TOPIC),
                        black_box(10_000),
                        black_box(k),
                    );
                    black_box(hits)
                })
            },
        );
    }

    group.finish();
}

fn bench_record(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_record");

    // 单条 record（含窗口裁剪）摊销：预填满 60，再逐条迭代写。
    for n in [60usize, 240] {
        group.bench_with_input(BenchmarkId::new("local", n), &n, |b, &n| {
            b.iter_batched(
                || local_store(n),
                |mut store| {
                    store.record(black_box("ada"), episode(999_999));
                    black_box(store.len_total())
                },
                criterion::BatchSize::SmallInput,
            )
        });
    }

    group.finish();
}

criterion_group!(benches, bench_retrieve, bench_record, bench_large_window);
criterion_main!(benches);
