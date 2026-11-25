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

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_common::TokenUsage;
use oriera_infer::{ChatMessage, InferError, InferJobs, ModelProvider};
use std::sync::Arc;

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
fn drain_all(jobs: &mut InferJobs<InstantBackend>) -> usize {
    let mut landed = 0;
    while jobs.inflight() > 0 {
        landed += jobs.poll().len();
        std::hint::spin_loop();
    }
    landed + jobs.poll().len()
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

criterion_group!(benches, bench_jobs);
criterion_main!(benches);
