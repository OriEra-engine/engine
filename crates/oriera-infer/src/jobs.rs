//! L7 异步推理作业 —— 把阻塞的模型调用搬出帧循环(RFC 0002 v0.5 第一步)。
//!
//! `ModelProvider::complete()` 是阻塞的:真 LLM 一次 1~5 秒,认知/导演
//! 系统在帧内同步调用会把整帧卡死(STABILITY 已知边界里的 P0)。本模块
//! 的答案是「提交-收割」两段式:
//!
//! - [`InferJobs::submit`] 在后台线程发起调用,**立刻**返回作业号;
//! - [`InferJobs::poll`] 每帧 `try_recv` 收割已完成的结果,同样非阻塞。
//!
//! 帧循环从此只做提交与收割,网络延迟从"卡住的帧"变成"晚一拍落地的
//! 对话"。默认每类调用方只挂一个在飞作业(如认知系统一拍一句),所以
//! 完成顺序的网路不确定性不影响世界语义;需要严格保序的调用方应当
//! 自己串行提交。
//!
//! 降级可见原则同样适用:作业失败通过 [`JobDone::result`] 原样上抛,
//! 由调用方降级并展示,绝不静默吞掉。

use crate::provider::{ChatMessage, InferError, ModelProvider};
use oriera_common::TokenUsage;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

/// One completed (or failed) inference job, drained by [`InferJobs::poll`].
#[derive(Debug, Clone, PartialEq)]
pub struct JobDone {
    /// The id returned by the matching `submit` call.
    pub id: u64,
    /// The provider answer, or its failure — callers degrade visibly.
    pub result: Result<String, InferError>,
    /// Token usage measured by the worker right after the call
    /// (`TokenUsage::ZERO` when the backend does not meter).
    pub usage: TokenUsage,
}

/// Async wrapper around any [`ModelProvider`]: submit from the frame
/// loop, poll results next frames. See the module docs for the pattern.
///
/// Threads: one short-lived OS thread per submitted job (LLM calls sit
/// at seconds of latency — thread spawn cost is noise). Bounded by
/// `max_inflight`; a submit over the cap fails fast with
/// [`InferError::Busy`] instead of queueing unbounded work.
pub struct InferJobs<P: ModelProvider> {
    provider: Arc<P>,
    tx: std::sync::mpsc::Sender<JobDone>,
    rx: Receiver<JobDone>,
    inflight: Arc<AtomicUsize>,
    next_id: u64,
    max_inflight: usize,
    /// Total jobs accepted (busy-refused submits do not count).
    pub submitted: u64,
    /// Total jobs drained with `Ok` results.
    pub completed: u64,
    /// Total jobs drained with `Err` results.
    pub failed: u64,
}

impl<P: ModelProvider + 'static> InferJobs<P> {
    /// New job pool over `provider`, allowing at most `max_inflight`
    /// concurrent in-flight calls (per pool, not per provider).
    pub fn new(provider: Arc<P>, max_inflight: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            provider,
            tx,
            rx,
            inflight: Arc::new(AtomicUsize::new(0)),
            next_id: 0,
            max_inflight: max_inflight.max(1),
            submitted: 0,
            completed: 0,
            failed: 0,
        }
    }

    /// Kick off one completion on a background thread. Returns the job
    /// id immediately — never blocks on the network.
    pub fn submit(&mut self, messages: Vec<ChatMessage>) -> Result<u64, InferError> {
        if self.inflight.load(Ordering::Acquire) >= self.max_inflight {
            return Err(InferError::Busy);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.submitted += 1;
        self.inflight.fetch_add(1, Ordering::AcqRel);

        let provider = Arc::clone(&self.provider);
        let tx = self.tx.clone();
        let inflight = Arc::clone(&self.inflight);
        std::thread::spawn(move || {
            let result = provider.complete(&messages);
            // Meter whatever the backend measured, success or failure —
            // usage is per-call state, safe to read immediately after.
            let usage = provider.last_usage();
            let _ = tx.send(JobDone { id, result, usage });
            inflight.fetch_sub(1, Ordering::Release);
        });
        Ok(id)
    }

    /// Drain every result that has arrived since the last poll. Never
    /// blocks: an empty Vec means "nothing landed yet", not "all done".
    pub fn poll(&mut self) -> Vec<JobDone> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(done) => {
                    match done.result {
                        Ok(_) => self.completed += 1,
                        Err(_) => self.failed += 1,
                    }
                    out.push(done);
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    /// Jobs submitted but not yet drained.
    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Offline backend that blocks for `delay` then answers `reply`
    /// (or fails). Lets tests prove the frame path never waits.
    struct Sleepy {
        delay: Duration,
        fail: bool,
    }

    impl ModelProvider for Sleepy {
        fn id(&self) -> &str {
            "sleepy-test"
        }
        fn complete(&self, _messages: &[ChatMessage]) -> Result<String, InferError> {
            std::thread::sleep(self.delay);
            if self.fail {
                Err(InferError::Provider("test: provider down".into()))
            } else {
                Ok("（擦着杯子）豆子是埃塞的。等它凉半分钟再喝。".into())
            }
        }
        fn last_usage(&self) -> TokenUsage {
            TokenUsage {
                prompt: 8,
                completion: 8,
            }
        }
    }

    #[test]
    fn submit_returns_immediately_and_result_lands_later() {
        let mut jobs = InferJobs::new(
            Arc::new(Sleepy {
                delay: Duration::from_millis(80),
                fail: false,
            }),
            4,
        );
        let t0 = Instant::now();
        let id = jobs.submit(vec![ChatMessage::user("聊聊咖啡豆")]).unwrap();
        assert!(
            t0.elapsed() < Duration::from_millis(20),
            "submit must not wait on the network"
        );

        assert!(jobs.poll().is_empty(), "80ms job cannot be done instantly");
        assert_eq!(jobs.inflight(), 1);

        // Frames keep ticking (polls are cheap) until the job lands.
        let mut landed = None;
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(10));
            if let Some(done) = jobs.poll().into_iter().next() {
                landed = Some(done);
                break;
            }
        }
        let done = landed.expect("job lands within ~2s of simulated latency");
        assert_eq!(done.id, id);
        assert!(done.result.unwrap().contains("埃塞"));
        assert_eq!(
            done.usage,
            TokenUsage {
                prompt: 8,
                completion: 8
            }
        );
        assert_eq!(jobs.completed, 1);
        assert_eq!(jobs.failed, 0);
        assert_eq!(jobs.inflight(), 0);
    }

    #[test]
    fn provider_failures_arrive_as_errors_never_silently_dropped() {
        let mut jobs = InferJobs::new(
            Arc::new(Sleepy {
                delay: Duration::from_millis(10),
                fail: true,
            }),
            4,
        );
        jobs.submit(vec![ChatMessage::user("x")]).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        let done = jobs.poll().pop().expect("failure lands");
        assert!(done.result.is_err());
        assert_eq!(jobs.failed, 1);
        assert_eq!(jobs.completed, 0);
    }

    #[test]
    fn concurrency_cap_fails_fast_with_busy() {
        let mut jobs = InferJobs::new(
            Arc::new(Sleepy {
                delay: Duration::from_millis(120),
                fail: false,
            }),
            2,
        );
        jobs.submit(vec![ChatMessage::user("a")]).unwrap();
        jobs.submit(vec![ChatMessage::user("b")]).unwrap();
        assert_eq!(
            jobs.submit(vec![ChatMessage::user("c")]).unwrap_err(),
            InferError::Busy
        );
        assert_eq!(jobs.submitted, 2, "refused submits do not count");
        // After the cap frees, submits work again.
        std::thread::sleep(Duration::from_millis(200));
        let drained = jobs.poll();
        assert_eq!(drained.len(), 2, "both in-flight jobs complete");
        assert!(jobs.submit(vec![ChatMessage::user("d")]).is_ok());
    }

    #[test]
    fn many_jobs_all_drain_eventually() {
        let mut jobs = InferJobs::new(
            Arc::new(Sleepy {
                delay: Duration::from_millis(30),
                fail: false,
            }),
            8,
        );
        let ids: Vec<u64> = (0..8)
            .map(|i| {
                jobs.submit(vec![ChatMessage::user(format!("m{i}"))])
                    .unwrap()
            })
            .collect();
        assert_eq!(jobs.inflight(), 8);
        let mut got = Vec::new();
        for _ in 0..300 {
            std::thread::sleep(Duration::from_millis(10));
            got.extend(jobs.poll().into_iter().map(|d| d.id));
            if got.len() == 8 {
                break;
            }
        }
        let mut sorted = got;
        sorted.sort_unstable();
        assert_eq!(sorted, ids, "every submitted id drains exactly once");
        assert_eq!(jobs.completed, 8);
    }
}
