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
        if degraded {
            meter.degraded += 1;
        }
        if degraded && limits.placeholder_on_failure {
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
        while self.in_flight.len() + self.submitting.len() < max_in_flight {
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
        for rx in self.submitting.drain(..) {
            match rx.try_recv() {
                Ok(JobOutcome::Submitted { req, result }) => done_submits.push((req, result)),
                Ok(_) => unreachable!("submit channel only sends Submitted"),
                Err(_) => still_submitting.push(rx), // worker still running
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

        // 2. Interval-bounded polling: a slice of the frame budget, not
        //    a transport storm (stretched under budget pressure above).
        //    #117: each poll (including the done-task's asset download —
        //    megabytes for video) runs on a background thread; the frame
        //    only dispatches due polls and reaps finished ones.
        if self.tick.is_multiple_of(poll_interval as u64) {
            // Dispatch due polls (still bounded by in-flight capacity).
            for t in self.in_flight.drain(..) {
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
                    // queued/running 与传输错误：回 in_flight，下个
                    // interval 重新 dispatch 一个新 poll 线程（worker 的
                    // one-shot channel 一次性使用后丢弃）。
                    Ok(JobOutcome::Polled {
                        flight,
                        result: Ok(_),
                    })
                    | Ok(JobOutcome::Polled {
                        flight,
                        result: Err(_),
                    }) => {
                        self.in_flight.push(flight);
                    }
                    Ok(_) => unreachable!("poll channel only sends Polled"),
                    Err(_) => still_polling.push((t, rx)), // worker still running
                }
            }
            self.polling = still_polling;
            for (flight, asset) in landed {
                match validate_asset(&asset, &validation) {
                    Ok(()) => {
                        let meter = world.resource_or_default::<GenAiMeter>();
                        meter.succeeded += 1;
                        meter.bytes_in += asset.payload.len() as u64;
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
    use crate::seedance::SeedanceProvider;
    use crate::transport::{HttpRequest, HttpResponse, Transport};
    use crate::MockProvider;

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

    /// 慢速脚本传输：send 睡眠 400ms + 响应按 LIFO 出队（#117 断言用）。
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
    fn slow_provider_never_blocks_the_frame() {
        use crate::transport::HttpResponse;

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
