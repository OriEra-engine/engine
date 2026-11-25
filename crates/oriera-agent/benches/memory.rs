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

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_agent::{
    ConsolidationPolicy, Episode, LayeredMemoryBackend, MemoryScoring, MemoryStore,
};

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

criterion_group!(benches, bench_retrieve, bench_record);
criterion_main!(benches);
