//! L8 engine-loop integration — generation as a scheduled system.
//!
//! [`GenAiSystem`] is what makes generation *engine-native* instead of
//! app code: it drains a request queue, submits tasks through the
//! [`GenerationProvider`] boundary, polls in-flight tasks on a bounded
//! tick slice, validates results with [`crate::validate_asset`], and
//! hands landed assets to the shell through the [`PendingSwaps`]
//! resource plus typed events (`AssetLanded`/`AssetFailed`) — the same
//! channels cognition uses (book T12). The world is passed in by the
//! L2 scheduler; nothing reaches into globals.
//!
//! Degrade path (ADR 0003 criterion 3): a failed task lands a
//! **placeholder mesh** when [`GenAiLimits::placeholder_on_failure`]
//! is set, so an outage costs visual fidelity, never a crash.

use crate::{
    validate_asset, AssetFormat, GenAiError, GeneratedAsset, GenerationProvider, GenerationRequest,
    TaskStatus, ValidationLimits,
};
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use std::sync::Arc;

/// Metering resource: one place the UI reads "what did generation cost
/// and land". Appears on its own (self-healing) the first time the
/// system ticks — worlds without generation stay meter-free.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GenAiMeter {
    pub submitted: u64,
    pub succeeded: u64,
    pub failed: u64,
    /// Tasks that failed *and* landed a placeholder instead.
    pub degraded: u64,
    /// Validated bytes that reached [`PendingSwaps`].
    pub bytes_in: u64,
    /// Vendor-reported generation tokens landed with assets (#122):
    /// Seedance returns ~108.9k completion tokens per 5s@720p clip —
    /// the same magnitude as an hour of L7 chat, and invisible until
    /// this counter exists.
    pub tokens_in: u64,
    /// Micro-yuan cost folded from vendor metering (`usage.cost_micros`
    /// summed; unpriced vendors contribute 0 — never a guess).
    pub cost_micros_in: u64,
}

/// Frame-budget guardrails. All clamped through [`GenAiLimits::effective`]
/// — a hallucinated `u64::MAX` from a model must not become policy
/// (same philosophy as L6 `TimeScale`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenAiLimits {
    pub max_bytes: u64,
    pub max_in_flight: usize,
    pub poll_interval_ticks: u32,
    /// Land a placeholder mesh when a task fails (ADR 0003 criterion 3).
    pub placeholder_on_failure: bool,
}

impl Default for GenAiLimits {
    fn default() -> Self {
        Self {
            max_bytes: crate::DEFAULT_MAX_ASSET_BYTES,
            max_in_flight: 4,
            poll_interval_ticks: 1,
            placeholder_on_failure: true,
        }
    }
}

/// Upper clamps for the numeric fields.
pub const MAX_IN_FLIGHT_HARD: usize = 64;
pub const POLL_INTERVAL_HARD: u32 = 60;

impl GenAiLimits {
    /// The limits every consumer must use: sizes through
    /// [`ValidationLimits::effective`], concurrency in
    /// `[1, MAX_IN_FLIGHT_HARD]`, interval in `[1, POLL_INTERVAL_HARD]`.
    pub fn effective(self) -> Self {
        Self {
            max_bytes: ValidationLimits {
                max_bytes: self.max_bytes,
            }
            .effective()
            .max_bytes,
            max_in_flight: self.max_in_flight.clamp(1, MAX_IN_FLIGHT_HARD),
            poll_interval_ticks: self.poll_interval_ticks.clamp(1, POLL_INTERVAL_HARD),
            placeholder_on_failure: self.placeholder_on_failure,
        }
    }
}

/// A validated result waiting for the shell to land it in the L4 store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetSwap {
    /// Logical L4 name the payload lands under (`insert_named` key).
    pub name: String,
    pub asset: GeneratedAsset,
    /// True when `asset` is a placeholder standing in for a failure.
    pub placeholder: bool,
}

/// L3 resource holding landed-but-not-yet-swapped assets. The
/// application shell — the side that owns the [`oriera_asset::AssetStore`]
/// — drains it with [`crate::hot_swap::apply_pending`] after the tick.
#[derive(Debug, Default)]
pub struct PendingSwaps {
    pub queue: Vec<AssetSwap>,
}

/// Event: a generation landed (validated). Fired once per asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetLanded {
    pub name: String,
    pub provider: String,
    pub placeholder: bool,
}

/// Event: a generation failed to produce a valid asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetFailed {
    pub name: String,
    pub error: String,
    /// True when a placeholder mesh was landed in its place.
    pub degraded: bool,
}

#[derive(Clone)]
struct InFlight {
    task: crate::TaskId,
    name: String,
    kind: crate::GenerationKind,
    /// 已轮询次数（W12 退避：老任务降频，减少供应商 QPS 压力）。
    polls: u32,
}

/// One background job's answer, handed back to the frame through a
/// channel (`std::sync::mpsc` — the worker thread owns the sender).
enum JobOutcome {
    Submitted {
        req: GenerationRequest,
        result: Result<crate::TaskId, GenAiError>,
    },
    Polled {
        flight: InFlight,
        result: Result<TaskStatus, GenAiError>,
    },
}

/// The L8 system: request queue + bounded task polling + validation +
/// landing into [`PendingSwaps`]. Register on the L2 scheduler like
/// any other system.
///
/// **#117 (review R2 P0): transport never runs on the frame.** Submit
/// and poll calls (the latter include the asset download for
/// done tasks — megabytes for video) are `std::thread::spawn`ed onto
/// background jobs; the frame only `try_recv`s finished outcomes and
/// does the world mutations. A slow vendor costs *latency of the
/// asset*, never *frames of the game*.
pub struct GenAiSystem {
    provider: Arc<dyn GenerationProvider>,
    queue: Vec<GenerationRequest>,
    in_flight: Vec<InFlight>,
    /// Background submit jobs whose outcome has not been reaped yet.
    submitting: Vec<std::sync::mpsc::Receiver<JobOutcome>>,
    /// Background poll jobs, same deal. The receiver lives beside the
    /// flight bookkeeping so `poll_interval` still governs cadence.
    polling: Vec<(InFlight, std::sync::mpsc::Receiver<JobOutcome>)>,
    limits: GenAiLimits,
    tick: u64,
    /// Requests rejected at submit (unsupported/empty name), for
    /// observability without a world at hand.
    pub rejected: u64,
}

impl GenAiSystem {
    pub fn new(provider: Arc<dyn GenerationProvider>) -> Self {
        Self {
            provider,
            queue: Vec::new(),
            in_flight: Vec::new(),
            submitting: Vec::new(),
            polling: Vec::new(),
            limits: GenAiLimits::default(),
            tick: 0,
            rejected: 0,
        }
    }

    /// Override the default guardrails.
    pub fn with_limits(mut self, limits: GenAiLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Queue a generation; it is submitted on a later tick within the
    /// in-flight budget.
    pub fn submit(&mut self, req: GenerationRequest) {
        self.queue.push(req);
    }

    /// Requests waiting plus tasks in flight.
    pub fn pending(&self) -> usize {
        self.queue.len() + self.in_flight.len() + self.submitting.len() + self.polling.len()
    }

    fn fail(
        &self,
        world: &mut World,
        name: &str,
        kind: crate::GenerationKind,
        error: String,
        degraded: bool,
        limits: &GenAiLimits,
    ) {
        let meter = world.resource_or_default::<GenAiMeter>();
        meter.failed += 1;
        // degraded 的口径 =「失败且真的落地了占位」(字段文档如此)——
        // placeholder_on_failure 关闭时什么都不落,就不该计数。
        if degraded && limits.placeholder_on_failure {
            meter.degraded += 1;
            world
                .resource_or_default::<PendingSwaps>()
                .queue
                .push(AssetSwap {
                    name: name.to_string(),
                    asset: placeholder_asset(kind),
                    placeholder: true,
                });
            world.write_event(AssetLanded {
                name: name.to_string(),
                provider: "placeholder".into(),
                placeholder: true,
            });
        }
        world.write_event(AssetFailed {
            name: name.to_string(),
            error,
            degraded,
        });
    }
}

/// The degrade payload: a valid, parseable minimal container **in the
/// format the slot expects** — a failed skydome texture lands a real
/// 1×1 PNG, a failed mesh lands a real empty-scene GLB. Renderers never
/// receive bytes their decoder cannot read (ADR 0003 criterion 3).
fn placeholder_asset(kind: crate::GenerationKind) -> GeneratedAsset {
    let payload = match kind.placeholder_format() {
        AssetFormat::Glb => crate::tiny_glb(),
        AssetFormat::Image => crate::tiny_png(),
        AssetFormat::Video => crate::tiny_mp4_stub(),
        AssetFormat::Audio => crate::tiny_mp3(),
        AssetFormat::Usdz => Vec::new(), // no USDZ kinds yet; unreachable
    };
    GeneratedAsset {
        format: kind.placeholder_format(),
        payload,
        provider_id: "placeholder".into(),
        model: String::new(),
        usage: None,
    }
}

impl System<World> for GenAiSystem {
    fn name(&self) -> &str {
        "l8-genai"
    }

    fn run(&mut self, ctx: &FrameCtx, world: &mut World) {
        self.tick += 1;
        let limits = self.limits.effective();
        let validation = ValidationLimits {
            max_bytes: limits.max_bytes,
        };

        // Compute-budget dial (v0.4): allocator pressure shrinks the
        // in-flight ceiling and stretches the poll interval — at most
        // 8×. Generation is the most deferrable work in the engine:
        // under pressure assets land later, never worse. Unmanaged
        // (throttle 1.0) keeps the configured limits exactly.
        let grant = ctx.grants.get(self.name());
        let max_in_flight = grant.scale_usize(limits.max_in_flight, 1);
        let poll_interval = if grant.throttle >= 1.0 {
            limits.poll_interval_ticks
        } else {
            let stretch = (1.0 / grant.throttle.max(0.05)).ceil() as u32;
            limits
                .poll_interval_ticks
                .saturating_mul(stretch)
                .min(limits.poll_interval_ticks.saturating_mul(8))
        };

        // 1. Capacity-bounded submission — the HTTP call runs on a
        //    background thread (#117); the frame only reaps outcomes.
        //    polling 也要计入上限:任务派发轮询后从 in_flight 挪到
        //    polling,只数前者的话 gate 在每个轮询窗口都看到假空位,
        //    稳态并发可达 2× max_in_flight(2026-10-29 评审 P2)。
        while self.in_flight.len() + self.submitting.len() + self.polling.len() < max_in_flight {
            let Some(req) = self.queue.pop() else {
                break;
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let provider = Arc::clone(&self.provider);
            std::thread::spawn(move || {
                let result = provider.request_generation(&req);
                let _ = tx.send(JobOutcome::Submitted { req, result });
            });
            self.submitting.push(rx);
        }
        // Reap finished submits (two-pass: split first, then handle —
        // the handlers borrow self/world, the receiver drain needs &mut).
        let mut still_submitting = Vec::new();
        let mut done_submits = Vec::new();
        let mut orphan_errors = Vec::new();
        for rx in self.submitting.drain(..) {
            match rx.try_recv() {
                Ok(JobOutcome::Submitted { req, result }) => done_submits.push((req, result)),
                Ok(_) => unreachable!("submit channel only sends Submitted"),
                // Disconnected = worker panic：以孤儿请求记失败走 fail()
                // 通道——panic 的 job 不许永久占用提交预算（评审 R3 性能 P1）。
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    orphan_errors.push("submit worker died without a result".to_string());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => still_submitting.push(rx), // worker still running
            }
        }
        self.submitting = still_submitting;
        for (req, result) in done_submits {
            match result {
                Ok(task) => {
                    world.resource_or_default::<GenAiMeter>().submitted += 1;
                    self.in_flight.push(InFlight {
                        task,
                        kind: req.kind,
                        name: req.name,
                        polls: 0,
                    });
                }
                Err(GenAiError::Unsupported(m)) => {
                    // Caller bug (kind not served / empty name): count,
                    // tell the world, land nothing — no placeholder for
                    // requests that were never valid.
                    self.rejected += 1;
                    self.fail(
                        world,
                        &req.name,
                        req.kind,
                        format!("unsupported: {m}"),
                        false,
                        &limits,
                    );
                }
                Err(e) => {
                    self.fail(world, &req.name, req.kind, e.to_string(), true, &limits);
                }
            }
        }
        // 孤儿提交(worker panic)没有真实请求名:只计失败并广播
        // AssetFailed,不落地占位、不发 AssetLanded——没有任何人请求过
        // "__orphan__",往 PendingSwaps 塞一套假资产是给世界递垃圾
        // (2026-10-29 评审 P3;轮询侧死亡路径本就用真实任务名)。
        for err in orphan_errors {
            self.fail(
                world,
                "__orphan__",
                crate::GenerationKind::TextTo3d,
                err,
                false,
                &limits,
            );
        }

        // 2. Interval-bounded polling: a slice of the frame budget, not
        //    a transport storm (stretched under budget pressure above).
        //    #117: each poll (including the done-task's asset download —
        //    megabytes for video) runs on a background thread; the frame
        //    only dispatches due polls and reaps finished ones.
        if self.tick.is_multiple_of(poll_interval as u64) {
            // Dispatch due polls (still bounded by in-flight capacity).
            // W12 退避：老任务降频——前 8 个轮询机会每机会派发，之后每
            // 4 个机会派一次（视频任务 82~212s，高频窗口覆盖最可能完成
            // 区间；供应商 QPS 压力随任务年龄递减）。按任务自身机会计数
            // 而非全局 tick 取模（架构师三轮评审 P1：poll_interval 为 4
            // 的倍数或预算拉伸时，派发 tick 恒 %4==0，全局式退避零生效）。
            let backoff_step = 4u32;
            let drained: Vec<_> = std::mem::take(&mut self.in_flight);
            for mut t in drained {
                t.polls = t.polls.saturating_add(1);
                let due = t.polls <= 8 || (t.polls - 8) % backoff_step == 0;
                if !due {
                    self.in_flight.push(t);
                    continue;
                }
                let (tx, rx) = std::sync::mpsc::channel();
                let provider = Arc::clone(&self.provider);
                let task = t.task.clone();
                let flight = t.clone();
                std::thread::spawn(move || {
                    let result = provider.poll_task(&task);
                    let _ = tx.send(JobOutcome::Polled { flight, result });
                });
                self.polling.push((t, rx));
            }
            // Reap finished polls (two-pass, same reason as submits);
            // world mutations stay on the frame. Non-terminal tasks go
            // back into the queue WITH their channel — the worker sender
            // is one-shot, but a still-running poll simply hasn't sent
            // yet, so try_recv will keep returning Empty until it does.
            let mut still_polling = Vec::new();
            let mut landed = Vec::new();
            let mut failed = Vec::new();
            for (t, rx) in self.polling.drain(..) {
                match rx.try_recv() {
                    Ok(JobOutcome::Polled {
                        flight,
                        result: Ok(TaskStatus::Succeeded(asset)),
                    }) => landed.push((flight, asset)),
                    Ok(JobOutcome::Polled {
                        flight,
                        result: Ok(TaskStatus::Failed(m)),
                    }) => failed.push((flight, m)),
                    // queued/running：回 in_flight，下个 interval 重新
                    // dispatch 一个新 poll 线程（worker 的 one-shot
                    // channel 一次性使用后丢弃）。
                    Ok(JobOutcome::Polled {
                        flight,
                        result: Ok(_),
                    }) => {
                        // polls 只在派发侧计数（架构师三轮评审 P1：派发
                        // +1 与收割再 +1 是双重计数，全速率窗口实为 4 次
                        // 而非注释的 8）——这里只回收排队。
                        self.in_flight.push(flight);
                    }
                    // 可重试传输错误(429/5xx/timeout):与 queued/running
                    // 同路,回队等下轮。
                    Ok(JobOutcome::Polled {
                        flight,
                        result: Err(e),
                    }) if e.class().retryable() => {
                        self.in_flight.push(flight);
                    }
                    // 不可重试(404 任务被删/401 key 吊销/402 配额):终局。
                    // 曾无条件回队——槽位被死任务永久占用,轮询无限烧
                    // 供应商 QPS,max_in_flight 卡死后队列全面饿死
                    // (2026-10-29 评审 P2;retryable() 分类本为此而建,
                    // 之前从未被轮询路径问询)。
                    Ok(JobOutcome::Polled {
                        flight,
                        result: Err(e),
                    }) => {
                        failed.push((flight, e.to_string()));
                    }
                    // Disconnected = worker panic：按失败落地占位（评审
                    // R3 性能 P1——panic 的 job 不许永久占用 in-flight）。
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        failed.push((t.clone(), "poll worker died without a result".to_string()));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => still_polling.push((t, rx)), // worker still running
                    _ => unreachable!("poll channel only sends Polled"),
                }
            }
            self.polling = still_polling;
            for (flight, asset) in landed {
                match validate_asset(&asset, &validation) {
                    Ok(()) => {
                        let meter = world.resource_or_default::<GenAiMeter>();
                        meter.succeeded += 1;
                        meter.bytes_in += asset.payload.len() as u64;
                        // #122：供应商计量进账本（cost 未登记的厂商计 0，
                        // 绝不猜价——与预设表 UNREGISTERED 同一立场）。
                        if let Some(u) = &asset.usage {
                            meter.tokens_in += u.tokens;
                            meter.cost_micros_in += u.cost_micros.unwrap_or(0);
                        }
                        world
                            .resource_or_default::<PendingSwaps>()
                            .queue
                            .push(AssetSwap {
                                name: flight.name.clone(),
                                asset,
                                placeholder: false,
                            });
                        world.write_event(AssetLanded {
                            name: flight.name.clone(),
                            provider: self.provider.id().to_string(),
                            placeholder: false,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(
                            name = %flight.name,
                            error = %e,
                            "L8 generated asset failed validation — degrading"
                        );
                        self.fail(
                            world,
                            &flight.name,
                            flight.kind,
                            e.to_string(),
                            true,
                            &limits,
                        );
                    }
                }
            }
            for (flight, m) in failed {
                self.fail(world, &flight.name, flight.kind, m, true, &limits);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockProvider;
    // 评审回归测试的自建 provider 需要 crate 根的类型(super::* 不转发
    // system.rs 顶部的 use 导入)。
    use crate::{AssetFailed, AssetLanded, GenAiError, GenerationRequest, TaskId, TaskStatus};
    // providers 门控的模块（seedance/transport）随其测试腿条件导入——
    // 默认腿（无 providers）不编译它们（CI 两次抓过同款盲区）。
    #[cfg(feature = "providers")]
    use crate::seedance::SeedanceProvider;
    // transport 类型仅 slow_provider 测试用（fn 内 use，providers 门控
    // 由该测试的 cfg 承担）——模块级不再导入，避免两腿矛盾。

    use oriera_common::FrameIndex;
    use std::sync::Arc;

    /// 测试辅助（#117 后台化后必用）：驱动 tick 循环，每 tick 前微
    /// sleep 让后台线程送达结果，直到 pending 归零或次数用尽。Mock
    /// provider 是即时响应，8 次余量远超所需。
    fn settle(sys: &mut GenAiSystem, ctx: &FrameCtx<'static>, world: &mut World) {
        for _ in 0..12 {
            if sys.pending() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
            sys.run(ctx, world);
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
        sys.run(ctx, world);
    }

    fn ctx() -> FrameCtx<'static> {
        FrameCtx {
            frame: FrameIndex(0),
            dt: 0.0,
            now: oriera_common::Timestamp(0),
            grants: oriera_common::Grants::EMPTY,
        }
    }

    fn world() -> World {
        World::new()
    }

    /// A context where `l8-genai` is managed at the given throttle.
    fn ctx_throttled(t: f32) -> FrameCtx<'static> {
        let entries = Box::leak(
            vec![(
                "l8-genai".to_string(),
                oriera_common::ComputeGrant {
                    share_ms: 1.0,
                    throttle: t,
                },
            )]
            .into_boxed_slice(),
        );
        FrameCtx {
            frame: FrameIndex(0),
            dt: 0.0,
            now: oriera_common::Timestamp(0),
            grants: oriera_common::Grants::new(entries),
        }
    }

    #[test]
    fn budget_pressure_defers_generation_not_degrades_it() {
        // Four queued requests, max_in_flight 4: unthrottled submits
        // all four on the first tick; half throttle submits two — the
        // rest wait for headroom, and nothing fails or placeholders.
        let queue_four = || {
            let mut sys = GenAiSystem::new(Arc::new(MockProvider::new()));
            for i in 0..4 {
                sys.submit(GenerationRequest::text_to_3d(format!("prop{i}"), "a prop"));
            }
            sys
        };
        let mut w = world();
        let mut full = queue_four();
        full.run(&ctx(), &mut w);
        // #117：首轮 dispatch 的 4 个 submit 全部在途（后台线程），帧内
        // 只见 pending 计数；settle 后全部收割。
        assert_eq!(full.pending(), 4);
        settle(&mut full, &ctx(), &mut w);
        let full_meter = *w.resource::<GenAiMeter>().unwrap();
        assert_eq!(full_meter.submitted, 4);

        let mut w2 = world();
        let mut squeezed = queue_four();
        squeezed.run(&ctx_throttled(0.5), &mut w2);
        // 半节流限制的是在途上限（2 dispatched），pending 计总数（含 2 排队）
        assert_eq!(squeezed.pending(), 4);
        settle(&mut squeezed, &ctx_throttled(0.5), &mut w2);
        let meter = *w2.resource::<GenAiMeter>().unwrap();
        assert_eq!(meter.submitted, 4); // eventually all four, throttled
        assert_eq!(meter.failed, 0); // deferred, never failed
    }

    // ---- 2026-10-29 维护评审回归 ----

    /// 轮询永远不完成、且响应慢到能跨帧占用 polling 槽的 provider。
    struct SlowPoll;
    impl crate::GenerationProvider for SlowPoll {
        fn id(&self) -> &str {
            "slow-poll"
        }
        fn capabilities(&self) -> crate::ProviderCapabilities {
            crate::ProviderCapabilities::ALL
        }
        fn request_generation(&self, _req: &GenerationRequest) -> Result<TaskId, GenAiError> {
            Ok(TaskId("slow-1".into()))
        }
        fn poll_task(&self, _task: &TaskId) -> Result<TaskStatus, GenAiError> {
            // 睡过下一帧的 gate,让 polling 槽位真实可见
            std::thread::sleep(std::time::Duration::from_millis(150));
            Ok(TaskStatus::Running)
        }
    }

    #[test]
    fn polling_tasks_count_against_in_flight_cap() {
        // 评审 P2:gate 只数 in_flight+submitting 时,任务一进 polling
        // 就腾出假空位,稳态并发冲到 2× max_in_flight。
        let mut sys = GenAiSystem::new(Arc::new(SlowPoll)).with_limits(GenAiLimits {
            max_in_flight: 1,
            ..GenAiLimits::default()
        });
        sys.submit(GenerationRequest::text_to_3d("a", "甲"));
        sys.submit(GenerationRequest::text_to_3d("b", "乙"));
        let mut w = world();
        let pause = std::time::Duration::from_millis(20);
        // tick1:提交 a(收割进 in_flight);tick2:a 派发轮询——worker 睡
        // 150ms,跨帧占住 polling;tick3:gate 此刻必须看到 polling=1 而
        // 非假空位(旧代码在这里把 b 也提交出去)。
        sys.run(&ctx(), &mut w);
        std::thread::sleep(pause);
        sys.run(&ctx(), &mut w);
        std::thread::sleep(pause);
        sys.run(&ctx(), &mut w);
        let meter = *w.resource::<GenAiMeter>().unwrap();
        assert_eq!(meter.submitted, 1, "polling task must hold the cap");
    }

    /// 轮询返回不可重试错误(任务已删)的 provider。
    struct DeletedTask;
    impl crate::GenerationProvider for DeletedTask {
        fn id(&self) -> &str {
            "deleted-task"
        }
        fn capabilities(&self) -> crate::ProviderCapabilities {
            crate::ProviderCapabilities::ALL
        }
        fn request_generation(&self, _req: &GenerationRequest) -> Result<TaskId, GenAiError> {
            Ok(TaskId("gone".into()))
        }
        fn poll_task(&self, _task: &TaskId) -> Result<TaskStatus, GenAiError> {
            Err(GenAiError::Provider(
                "tripo poll returned HTTP 404: task deleted".into(),
            ))
        }
    }

    #[test]
    fn non_retryable_poll_error_terminates_and_unblocks_queue() {
        // 评审 P2:不可重试轮询错误曾无条件回队——槽位永久占用,队列
        // 饿死。现在按 retryable() 分类终局。
        let mut sys = GenAiSystem::new(Arc::new(DeletedTask)).with_limits(GenAiLimits {
            max_in_flight: 1,
            ..GenAiLimits::default()
        });
        sys.submit(GenerationRequest::text_to_3d("dead", "旧任务"));
        sys.submit(GenerationRequest::text_to_3d("next", "新任务"));
        let mut w = world();
        settle(&mut sys, &ctx(), &mut w);
        let meter = *w.resource::<GenAiMeter>().unwrap();
        // 两个任务都不可恢复地死(404):各自终局失败——关键在「死任务
        // 让出槽位」:旧代码下 dead 卡死 in_flight,next 永远提交不出去
        // (submitted 停在 1、pending 停在 1)。
        assert_eq!(meter.failed, 2, "each dead task fails terminally");
        assert_eq!(
            meter.submitted, 2,
            "queue must drain after terminal failure"
        );
        assert_eq!(sys.pending(), 0, "nothing stuck forever");
    }

    /// 提交即 panic 的 provider(模拟 worker 死亡)。
    struct PanicSubmit;
    impl crate::GenerationProvider for PanicSubmit {
        fn id(&self) -> &str {
            "panic-submit"
        }
        fn capabilities(&self) -> crate::ProviderCapabilities {
            crate::ProviderCapabilities::ALL
        }
        fn request_generation(&self, _req: &GenerationRequest) -> Result<TaskId, GenAiError> {
            panic!("worker exploded");
        }
        fn poll_task(&self, _task: &TaskId) -> Result<TaskStatus, GenAiError> {
            unreachable!()
        }
    }

    #[test]
    fn orphan_submit_death_lands_nothing() {
        // 评审 P3:孤儿提交曾以 "__orphan__" 名落地占位资产并发
        // AssetLanded——没人请求过它,这是给世界递垃圾。现在只计失败。
        let mut sys = GenAiSystem::new(Arc::new(PanicSubmit));
        sys.submit(GenerationRequest::text_to_3d("never-landed", "甲"));
        let mut w = world();
        settle(&mut sys, &ctx(), &mut w);
        let meter = *w.resource::<GenAiMeter>().unwrap();
        assert_eq!(meter.failed, 1);
        assert_eq!(meter.degraded, 0);
        if let Some(swaps) = w.resource::<PendingSwaps>() {
            assert!(swaps.queue.is_empty(), "orphan must not land a placeholder");
        }
        assert!(w.take_events::<AssetLanded>().is_empty());
        assert_eq!(w.take_events::<AssetFailed>().len(), 1);
    }

    /// 提交即 500 的 provider(degraded 占位路径)。
    struct ServerError;
    impl crate::GenerationProvider for ServerError {
        fn id(&self) -> &str {
            "server-error"
        }
        fn capabilities(&self) -> crate::ProviderCapabilities {
            crate::ProviderCapabilities::ALL
        }
        fn request_generation(&self, _req: &GenerationRequest) -> Result<TaskId, GenAiError> {
            Err(GenAiError::Provider(
                "tripo create returned HTTP 500".into(),
            ))
        }
        fn poll_task(&self, _task: &TaskId) -> Result<TaskStatus, GenAiError> {
            unreachable!()
        }
    }

    #[test]
    fn degraded_counter_means_actually_landed() {
        // 评审 P3:meter.degraded 的文档口径是「失败且落地了占位」——
        // placeholder_on_failure 关闭时什么都不落,就不得计数。
        let mk = |placeholder: bool| {
            let mut sys = GenAiSystem::new(Arc::new(ServerError)).with_limits(GenAiLimits {
                placeholder_on_failure: placeholder,
                ..GenAiLimits::default()
            });
            sys.submit(GenerationRequest::text_to_3d("prop", "甲"));
            let mut w = world();
            settle(&mut sys, &ctx(), &mut w);
            *w.resource::<GenAiMeter>().unwrap()
        };
        let off = mk(false);
        assert_eq!(off.failed, 1);
        assert_eq!(off.degraded, 0, "nothing landed → not degraded");
        let on = mk(true);
        assert_eq!(on.failed, 1);
        assert_eq!(on.degraded, 1, "placeholder landed → degraded");
    }

    #[test]
    fn full_lifecycle_lands_a_validated_swap_and_event() {
        let mut w = world();
        let mut sys = GenAiSystem::new(Arc::new(MockProvider::new()));
        sys.submit(GenerationRequest::text_to_3d("npc_prop", "一把老木吉他"));
        // Queued → Running → Succeeded: background jobs settle across ticks.
        settle(&mut sys, &ctx(), &mut w);
        let swaps = &w.resource::<PendingSwaps>().unwrap().queue;
        assert_eq!(swaps.len(), 1);
        assert_eq!(swaps[0].name, "npc_prop");
        assert!(!swaps[0].placeholder);
        let meter = *w.resource::<GenAiMeter>().unwrap();
        assert_eq!((meter.submitted, meter.succeeded, meter.failed), (1, 1, 0));
        assert!(meter.bytes_in > 0);
        let events = w.take_events::<AssetLanded>();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "npc_prop");
        assert!(sys.pending() == 0);
    }

    #[test]
    fn provider_failure_lands_a_placeholder_not_a_crash() {
        struct AlwaysFail;
        impl GenerationProvider for AlwaysFail {
            fn id(&self) -> &str {
                "always-fail"
            }
            fn capabilities(&self) -> crate::ProviderCapabilities {
                crate::ProviderCapabilities::ALL
            }
            fn request_generation(
                &self,
                _req: &GenerationRequest,
            ) -> Result<crate::TaskId, GenAiError> {
                Err(GenAiError::Provider("vendor down".into()))
            }
            fn poll_task(&self, _t: &crate::TaskId) -> Result<TaskStatus, GenAiError> {
                unreachable!("never submitted")
            }
        }

        let mut w = world();
        let mut sys = GenAiSystem::new(Arc::new(AlwaysFail));
        sys.submit(GenerationRequest::text_to_3d("bgm_prop", "门帘"));
        settle(&mut sys, &ctx(), &mut w);

        let swaps = &w.resource::<PendingSwaps>().unwrap().queue;
        assert_eq!(swaps.len(), 1);
        assert!(swaps[0].placeholder);
        assert_eq!(swaps[0].asset.provider_id, "placeholder");
        assert!(crate::validate_asset(&swaps[0].asset, &Default::default()).is_ok());
        let meter = *w.resource::<GenAiMeter>().unwrap();
        assert_eq!((meter.failed, meter.degraded), (1, 1));
        let failed = w.take_events::<AssetFailed>();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].degraded);
    }

    #[test]
    fn image_failure_lands_a_png_placeholder_not_an_empty_mesh() {
        struct FailImages;
        impl GenerationProvider for FailImages {
            fn id(&self) -> &str {
                "fail-images"
            }
            fn capabilities(&self) -> crate::ProviderCapabilities {
                crate::ProviderCapabilities::ALL
            }
            fn request_generation(
                &self,
                req: &GenerationRequest,
            ) -> Result<crate::TaskId, GenAiError> {
                match req.kind {
                    crate::GenerationKind::TextToImage => {
                        Err(GenAiError::Provider("image vendor down".into()))
                    }
                    _ => unreachable!("test only submits images"),
                }
            }
            fn poll_task(&self, _t: &crate::TaskId) -> Result<TaskStatus, GenAiError> {
                unreachable!("never submitted")
            }
        }

        let mut w = world();
        let mut sys = GenAiSystem::new(Arc::new(FailImages));
        sys.submit(GenerationRequest::text_to_image("sky_dome", "雨夜霓虹"));
        settle(&mut sys, &ctx(), &mut w);

        let swaps = &w.resource::<PendingSwaps>().unwrap().queue;
        assert_eq!(swaps.len(), 1);
        assert!(swaps[0].placeholder);
        // The slot expects a still: the placeholder must be a decodable
        // PNG, not the 3D tiny GLB.
        assert_eq!(swaps[0].asset.format, crate::AssetFormat::Image);
        assert!(crate::validate_asset(&swaps[0].asset, &Default::default()).is_ok());
        assert_eq!(swaps[0].asset.payload.first(), Some(&0x89)); // PNG magic
    }

    #[test]
    fn unsupported_requests_fail_without_placeholder() {
        let mut w = world();
        let caps = crate::ProviderCapabilities {
            text_to_3d: false,
            ..crate::ProviderCapabilities::ALL
        };
        let mut sys = GenAiSystem::new(Arc::new(MockProvider::with_capabilities(caps)));
        sys.submit(GenerationRequest::text_to_3d("x", "y"));
        settle(&mut sys, &ctx(), &mut w);

        assert_eq!(sys.rejected, 1);
        // Nothing landed, so the resource was never even created —
        // absent or empty both mean "no placeholder".
        assert!(w
            .resource::<PendingSwaps>()
            .map(|p| p.queue.is_empty())
            .unwrap_or(true));
        let failed = w.take_events::<AssetFailed>();
        assert_eq!(failed.len(), 1);
        assert!(!failed[0].degraded);
    }

    #[test]
    fn in_flight_is_bounded_by_limits() {
        let mut w = world();
        let mut sys = GenAiSystem::new(Arc::new(MockProvider::new())).with_limits(GenAiLimits {
            max_in_flight: 1,
            ..GenAiLimits::default()
        });
        sys.submit(GenerationRequest::text_to_3d("a", "1"));
        sys.submit(GenerationRequest::text_to_3d("b", "2"));
        sys.run(&ctx(), &mut w); // tick 1: one dispatched, one queued

        // #117：首轮 dispatch 的 submit 在后台线程，帧内只见 pending。
        assert_eq!(sys.pending(), 2); // 1 submitting + 1 queued

        settle(&mut sys, &ctx(), &mut w);
        assert!(
            !w.resource::<PendingSwaps>().unwrap().queue.is_empty(),
            "max_in_flight=1 也要最终落地（顺序提交不饿死）"
        );
    }

    /// #117 核心断言（评审 R2 性能 P0）：供应商慢（400ms/请求）时，
    /// 帧 run 本身必须毫秒级返回——传输全部在后台线程，取件/轮询的
    /// 延迟计入资产时延，绝不计入帧时延。
    #[test]
    #[cfg(feature = "providers")]
    fn slow_provider_never_blocks_the_frame() {
        use crate::transport::{HttpRequest, HttpResponse, Transport};
        struct SlowScripted {
            responses: std::sync::Mutex<Vec<HttpResponse>>,
        }
        impl Transport for SlowScripted {
            fn send(&self, _req: &HttpRequest) -> Result<HttpResponse, String> {
                std::thread::sleep(std::time::Duration::from_millis(400));
                self.responses
                    .lock()
                    .unwrap()
                    .pop()
                    .ok_or_else(|| "no scripted response".to_string())
            }
        }

        let wire = [
            HttpResponse::ok(b"mp4".to_vec()),
            HttpResponse::ok(
                r#"{"id":"s1","status":"succeeded","content":{"video_url":"https://tos.example/v.mp4"}}"#,
            ),
            HttpResponse::ok(r#"{"id":"s1"}"#),
        ];
        let scripted = Arc::new(SlowScripted {
            responses: std::sync::Mutex::new(wire.to_vec()),
        });

        let mut w = world();
        let mut sys = GenAiSystem::new(Arc::new(SeedanceProvider::new(
            crate::ProviderConfig {
                kind: crate::ProviderKind::Seedance,
                model: String::new(),
                api_key: "k".into(),
                base_url: Some("https://ark.example.test".into()),
            },
            scripted,
        )));
        sys.submit(GenerationRequest::text_to_video("shot1", "雨夜站台"));

        // 首帧：dispatch create（后台 400ms），run 本身必须立即返回。
        let t0 = std::time::Instant::now();
        sys.run(&ctx(), &mut w);
        let first = t0.elapsed();
        assert!(
            first < std::time::Duration::from_millis(200),
            "frame blocked by submit: {first:?}"
        );

        // 后续帧：收割/dispatch 均不阻塞（慢传输在线程里）。
        for _ in 0..12 {
            let t = std::time::Instant::now();
            sys.run(&ctx(), &mut w);
            assert!(
                t.elapsed() < std::time::Duration::from_millis(200),
                "frame blocked: {:?}",
                t.elapsed()
            );
            if w.resource::<PendingSwaps>().map(|s| !s.queue.is_empty()) == Some(true) {
                break; // asset landed through the slow pipeline
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        assert!(
            !w.resource::<PendingSwaps>().unwrap().queue.is_empty(),
            "slow asset must still land (latency, not loss)"
        );
    }

    /// #122：供应商 usage 块随资产落地进 GenAiMeter——tokens 与
    /// micro-yuan 成本可被 UI 直接读数；无 usage 的供应商计 0 不猜价。
    #[test]
    #[cfg(feature = "providers")]
    fn usage_lands_into_the_meter_with_the_asset() {
        use crate::testing::fixture_transport;
        use crate::transport::HttpResponse;
        use crate::{ProviderConfig, ProviderKind};
        // LIFO：取件 mp4、末次 poll succeeded、首轮 poll running、create
        let scripted = fixture_transport(vec![
            HttpResponse::ok(crate::tiny_mp4_stub()),
            HttpResponse::ok(
                r#"{"id":"c1","status":"succeeded","content":{"video_url":"https://tos.example/v.mp4"},
                    "usage":{"completion_tokens":108900}}"#,
            ),
            HttpResponse::ok(r#"{"id":"c1","status":"running"}"#),
            HttpResponse::ok(r#"{"id":"c1"}"#),
        ]);
        // SlowScripted 400ms/请求：dispatch 后轮询两次即收割（约 0.8s+）。
        let mut w = world();
        let mut sys = GenAiSystem::new(Arc::new(SeedanceProvider::new(
            ProviderConfig {
                kind: ProviderKind::Seedance,
                model: String::new(),
                api_key: "k".into(),
                base_url: Some("https://ark.example.test".into()),
            },
            scripted,
        )));
        sys.submit(GenerationRequest::text_to_video("shot1", "雨夜站台"));
        for i in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            sys.run(&ctx(), &mut w);
            eprintln!(
                "iter{i}: pending={} swaps={:?}",
                sys.pending(),
                w.resource::<PendingSwaps>().map(|s| s.queue.len())
            );
            if !w
                .resource::<PendingSwaps>()
                .map(|s| !s.queue.is_empty())
                .unwrap_or(false)
            {
                continue;
            }
            break;
        }
        let swaps = &w.resource::<PendingSwaps>().unwrap().queue;
        eprintln!(
            "swap usage: {:?} meter: {:?}",
            swaps[0].asset.usage,
            w.resource::<GenAiMeter>().unwrap()
        );
        assert!(
            swaps[0].asset.usage.is_some(),
            "seedance asset must carry usage"
        );
        let meter = *w.resource::<GenAiMeter>().unwrap();
        assert_eq!(meter.tokens_in, 108900, "usage tokens must land");
        // Seedance 牌价已登记（price_micros_per_ktoken 按档位折算）：
        // 108900 tokens × 31000 µ元/k ÷ 1000 = 3375900 µ元（先乘后除）。
        assert_eq!(
            meter.cost_micros_in, 3375900,
            "cost must be folded from the vendor metering"
        );
    }

    #[test]
    fn limits_clamp_through_one_entry_point() {
        let raw = GenAiLimits {
            max_bytes: 0,
            max_in_flight: 999_999,
            poll_interval_ticks: 0,
            placeholder_on_failure: false,
        };
        let eff = raw.effective();
        assert_eq!(eff.max_bytes, crate::DEFAULT_MAX_ASSET_BYTES);
        assert_eq!(eff.max_in_flight, MAX_IN_FLIGHT_HARD);
        assert_eq!(eff.poll_interval_ticks, 1);
        assert!(!eff.placeholder_on_failure);
    }

    #[test]
    fn bare_world_boots_without_panicking() {
        let mut w = World::new();
        let mut sys = GenAiSystem::new(Arc::new(MockProvider::new()));
        for _ in 0..3 {
            sys.run(&ctx(), &mut w);
        }
        // Nothing was generated, so nothing meters — the resource stays
        // absent until the first real outcome (UsageMeter semantics).
        assert!(w.resource::<GenAiMeter>().is_none());
    }
}
