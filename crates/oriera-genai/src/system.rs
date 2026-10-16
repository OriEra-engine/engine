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

struct InFlight {
    task: crate::TaskId,
    name: String,
}

/// The L8 system: request queue + bounded task polling + validation +
/// landing into [`PendingSwaps`]. Register on the L2 scheduler like
/// any other system.
pub struct GenAiSystem {
    provider: Arc<dyn GenerationProvider>,
    queue: Vec<GenerationRequest>,
    in_flight: Vec<InFlight>,
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
        self.queue.len() + self.in_flight.len()
    }

    fn fail(
        &self,
        world: &mut World,
        name: &str,
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
                    asset: placeholder_asset(),
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

/// The degrade payload: a fully valid, parseable minimal GLB.
fn placeholder_asset() -> GeneratedAsset {
    GeneratedAsset {
        format: AssetFormat::Glb,
        payload: crate::tiny_glb(),
        provider_id: "placeholder".into(),
        model: String::new(),
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

        // 1. Capacity-bounded submission.
        while self.in_flight.len() < max_in_flight {
            let Some(req) = self.queue.pop() else {
                break;
            };
            match self.provider.request_generation(&req) {
                Ok(task) => {
                    world.resource_or_default::<GenAiMeter>().submitted += 1;
                    self.in_flight.push(InFlight {
                        task,
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
                        format!("unsupported: {m}"),
                        false,
                        &limits,
                    );
                }
                Err(e) => {
                    self.fail(world, &req.name, e.to_string(), true, &limits);
                }
            }
        }

        // 2. Interval-bounded polling: a slice of the frame budget, not
        //    a transport storm (stretched under budget pressure above).
        if self.tick.is_multiple_of(poll_interval as u64) {
            let mut in_flight = std::mem::take(&mut self.in_flight);
            in_flight.retain(|t| {
                match self.provider.poll_task(&t.task) {
                    Ok(TaskStatus::Succeeded(asset)) => {
                        match validate_asset(&asset, &validation) {
                            Ok(()) => {
                                let meter = world.resource_or_default::<GenAiMeter>();
                                meter.succeeded += 1;
                                meter.bytes_in += asset.payload.len() as u64;
                                world
                                    .resource_or_default::<PendingSwaps>()
                                    .queue
                                    .push(AssetSwap {
                                        name: t.name.clone(),
                                        asset,
                                        placeholder: false,
                                    });
                                world.write_event(AssetLanded {
                                    name: t.name.clone(),
                                    provider: self.provider.id().to_string(),
                                    placeholder: false,
                                });
                            }
                            Err(e) => {
                                tracing::warn!(
                                    name = %t.name,
                                    error = %e,
                                    "L8 generated asset failed validation — degrading"
                                );
                                self.fail(world, &t.name, e.to_string(), true, &limits);
                            }
                        }
                        false // terminal
                    }
                    Ok(TaskStatus::Failed(m)) => {
                        self.fail(world, &t.name, m, true, &limits);
                        false
                    }
                    Ok(_) => true, // queued/running: poll again later
                    Err(e) => {
                        // Transport/poll error: keep the task; the next
                        // interval retries. Only the task's own terminal
                        // FAILED state degrades to a placeholder.
                        tracing::warn!(
                            task = %t.task.0,
                            error = %e,
                            "L8 poll error — will retry next interval"
                        );
                        true
                    }
                }
            });
            self.in_flight = in_flight;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockProvider;
    use oriera_common::FrameIndex;
    use std::sync::Arc;

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
                sys.submit(GenerationRequest::text_to_3d(&format!("prop{i}"), "a prop"));
            }
            sys
        };
        let mut w = world();
        let mut full = queue_four();
        full.run(&ctx(), &mut w);
        let full_meter = *w.resource::<GenAiMeter>().unwrap();
        assert_eq!(full_meter.submitted, 4); // full ceiling, first tick

        let mut w2 = world();
        let mut squeezed = queue_four();
        squeezed.run(&ctx_throttled(0.5), &mut w2);
        let meter = *w2.resource::<GenAiMeter>().unwrap();
        assert_eq!(meter.submitted, 2); // half the ceiling
        assert_eq!(meter.failed, 0); // deferred, never failed
    }

    #[test]
    fn full_lifecycle_lands_a_validated_swap_and_event() {
        let mut w = world();
        let mut sys = GenAiSystem::new(Arc::new(MockProvider::new()));
        sys.submit(GenerationRequest::text_to_3d("npc_prop", "一把老木吉他"));
        // Queued → Running → Succeeded: three polling ticks.
        for _ in 0..3 {
            sys.run(&ctx(), &mut w);
        }
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
        sys.run(&ctx(), &mut w);

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
    fn unsupported_requests_fail_without_placeholder() {
        let mut w = world();
        let caps = crate::ProviderCapabilities {
            text_to_3d: false,
            ..crate::ProviderCapabilities::ALL
        };
        let mut sys = GenAiSystem::new(Arc::new(MockProvider::with_capabilities(caps)));
        sys.submit(GenerationRequest::text_to_3d("x", "y"));
        sys.run(&ctx(), &mut w);

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
        sys.run(&ctx(), &mut w); // tick 1: one submitted, one queued

        let meter = *w.resource::<GenAiMeter>().unwrap();
        assert_eq!(meter.submitted, 1);
        assert_eq!(sys.pending(), 2); // 1 in flight + 1 queued

        for _ in 0..2 {
            sys.run(&ctx(), &mut w);
        }
        sys.run(&ctx(), &mut w); // first lands, second submits
        assert!(!w.resource::<PendingSwaps>().unwrap().queue.is_empty());
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
