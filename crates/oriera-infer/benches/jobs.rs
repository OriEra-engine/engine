//! L7 推理编排微基准 — `InferJobs` 提交/收割路径。
//!
//! 量的是**帧循环真正支付的成本**，不是模型耗时：
//! - `jobs/poll_empty`：空轮询（每帧都调，无 job 时的固定开销）；
//! - `jobs/submit_drain`：单 job 全周期（线程 spawn + channel 往返 + 落地）；
//! - `jobs/burst_n{n}_cap{c}`：并发梯度——一次提交 n 个、全部收割的总耗时，
//!   反映线程 spawn 风暴与池容的关系。
//!
//! 后端用立即返回的 `InstantBackend`，所以数字里不含任何模型/网络时间，
//! 纯是编排层自身开销。含 OS 线程 spawn，**跨机器/跨内核不可直接比**，
//! 只作同机回归基线。
//!
//! 对 #13 步 C 的意义：包装层接 deadline 截断时，Realtime 档的 per-frame
//! deadline 必须覆盖这里的提交开销，否则"超时即降级"会先被自己的
//! 编排成本吃掉预算。
//!
//! 尾延迟分位数诊断档：
//! ```sh
//! cargo bench -p oriera-infer --bench jobs -- --nocapture tails
//! ```
//! 并发 1/4/16，3 轮 × 60 样本，报 p50/p95/p99 与轮间 p50 极差；
//! 离线 RuleProvider，不接远程。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_common::TokenUsage;
use oriera_infer::{ChatMessage, InferError, InferJobs, ModelProvider, RuleProvider};
use std::sync::Arc;
use std::time::Instant;

/// 立即返回的离线后端——把模型耗时从测量里彻底剔除。
struct InstantBackend;

impl ModelProvider for InstantBackend {
    fn id(&self) -> &str {
        "instant-bench"
    }
    fn complete(&self, _messages: &[ChatMessage]) -> Result<String, InferError> {
        Ok("ok".into())
    }
    fn last_usage(&self) -> TokenUsage {
        TokenUsage::ZERO
    }
}

fn one_message(i: usize) -> Vec<ChatMessage> {
    vec![ChatMessage::user(format!("m{i}"))]
}

/// 提交后自旋收割直到池空；返回落地的 job 数。
///
/// worker 先 `send` 再 `fetch_sub`，所以 `inflight == 0` 时所有结果都已
/// 入队，但 `poll` 可能还没取走——退出循环后必须再 poll 一次收尾。
fn drain_all<P: ModelProvider + 'static>(jobs: &mut InferJobs<P>) -> usize {
    let mut landed = 0;
    while jobs.inflight() > 0 {
        landed += jobs.poll().len();
        std::hint::spin_loop();
    }
    landed + jobs.poll().len()
}

/// 样本分位数：`q ∈ [0,1]`，线性取最近秩。
fn percentile(samples: &mut [f64], q: f64) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((q * (samples.len() - 1) as f64).round() as usize).min(samples.len() - 1);
    samples[idx]
}

fn bench_jobs(c: &mut Criterion) {
    let mut group = c.benchmark_group("infer_jobs");

    // ── 空轮询：帧循环每帧的固定税 ──
    group.bench_function("poll_empty", |b| {
        let mut jobs = InferJobs::new(Arc::new(InstantBackend), 4);
        b.iter(|| black_box(jobs.poll()))
    });

    // ── 单 job 全周期：spawn + channel 往返 + 收割 ──
    group.bench_function("submit_drain", |b| {
        b.iter_batched(
            || InferJobs::new(Arc::new(InstantBackend), 1),
            |mut jobs| {
                jobs.submit(one_message(0)).expect("cap 1 accepts one job");
                black_box(drain_all(&mut jobs))
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // ── 并发梯度：n ≤ cap，一次提交全部再收割 ──
    for (n, cap) in [(1usize, 1usize), (4, 4), (8, 8), (8, 4), (16, 8)] {
        group.bench_with_input(
            BenchmarkId::new(format!("burst_n{n}"), cap),
            &(n, cap),
            |b, &(n, cap)| {
                b.iter_batched(
                    || InferJobs::new(Arc::new(InstantBackend), cap),
                    |mut jobs| {
                        // n > cap 时多出的提交按 Busy 拒绝，不计入 landed。
                        let mut accepted = 0;
                        for i in 0..n {
                            if jobs.submit(one_message(i)).is_ok() {
                                accepted += 1;
                            }
                        }
                        let landed = drain_all(&mut jobs);
                        debug_assert_eq!(landed, accepted);
                        black_box(landed)
                    },
                    criterion::BatchSize::SmallInput,
                )
            },
        );
    }

    group.finish();
}

/// 尾延迟分位数诊断档：`-- --nocapture tails` 触发。
///
/// submit+drain 全路径（spawn + channel 往返 + 收割），并发档 1/4/16，
/// 3 轮 × 60 样本；报整体 p50/p95/p99 与轮间 p50 的 max−min 波动。
/// 后端用离线 `RuleProvider`，不接远程；池在每轮内复用（稳态口径，
/// 不含每轮新建池的冷启动成分）。
fn bench_tails(c: &mut Criterion) {
    if !std::env::args().any(|a| a == "tails") {
        let mut g = c.benchmark_group("tails_noop");
        g.sample_size(10);
        g.bench_function("noop", |b| b.iter(|| black_box(1u64)));
        g.finish();
        return;
    }

    const ROUNDS: usize = 3;
    const SAMPLES: usize = 60;
    const WARMUP: usize = 10;

    println!("\n────── InferJobs submit+drain 尾延迟（RuleProvider 离线，复用池，µs）──────");
    println!(
        "{:>6} {:>10} {:>10} {:>10} {:>12} {:>12}",
        "并发", "p50", "p95", "p99", "轮间p50极差", "每job p50"
    );

    for cap in [1usize, 4, 16] {
        let mut round_p50: Vec<f64> = Vec::with_capacity(ROUNDS);
        let mut all: Vec<f64> = Vec::with_capacity(ROUNDS * SAMPLES);

        for _ in 0..ROUNDS {
            let mut jobs = InferJobs::new(Arc::new(RuleProvider::new()), cap);
            for _ in 0..WARMUP {
                let _ = jobs.submit(one_message(0));
                drain_all(&mut jobs);
            }
            let mut v: Vec<f64> = Vec::with_capacity(SAMPLES);
            for i in 0..SAMPLES {
                let t = Instant::now();
                let mut accepted = 0;
                for j in 0..cap {
                    if jobs.submit(one_message(i * cap + j)).is_ok() {
                        accepted += 1;
                    }
                }
                let landed = drain_all(&mut jobs);
                debug_assert_eq!(landed, accepted);
                v.push(t.elapsed().as_secs_f64() * 1e6);
            }
            let p50 = percentile(&mut v.clone(), 0.50);
            round_p50.push(p50);
            all.extend(v);
        }

        let p50 = percentile(&mut all.clone(), 0.50);
        let p95 = percentile(&mut all.clone(), 0.95);
        let p99 = percentile(&mut all.clone(), 0.99);
        let spread = round_p50.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - round_p50.iter().copied().fold(f64::INFINITY, f64::min);
        println!(
            "{cap:>6} {p50:>10.1} {p95:>10.1} {p99:>10.1} {spread:>12.1} {:>12.1}",
            p50 / cap as f64
        );
    }
    println!("注：内部基线，含 OS 线程 spawn，跨机器不可直接比；未标定硬件前不对外。\n");
}

criterion_group!(benches, bench_jobs, bench_tails);
criterion_main!(benches);
