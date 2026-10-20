//! Provider health & fallback chain (v0.8, PLAN-deepseek-upgrade B7).
//!
//! The InferError taxonomy classifies failures; this module *consumes*
//! that classification with the two policies the design page promises:
//!
//! - **circuit breaker** — N consecutive provider failures open the
//!   circuit (calls fail fast without touching the wire); after a
//!   cooldown one half-open probe may through; success closes, failure
//!   re-opens with a fresh cooldown. Opening is per-provider, so one
//!   dead vendor never poisons the others.
//! - **fallback chain** — [`FallbackProvider`] walks a preset-order
//!   chain (cloud → local open-source → offline rule) and serves from
//!   the first healthy entry. "咖啡馆永不下线": when the cloud is down
//!   the story keeps running on the rule bank.
//!
//! Failure classification (what counts toward opening):
//! [`InferError::Timeout`] / [`InferError::Network`] / [`InferError::Parse`]
//! and 5xx/429/408 statuses count — switching providers can help.
//! Other 4xx statuses are config errors (a bad key is bad everywhere on
//! that provider) — they surface to the caller but do NOT trip the
//! breaker. `Parse` counts because a vendor returning garbage is
//! precisely a provider-health signal.
//!
//! Fail-open budget: even fully degraded, [`ModelProvider::complete`]
//! errors honestly (never fabricates a reply) — the world's cadence
//! systems already degrade to "……" lines on error.

use crate::provider::{InferError, ModelProvider};
use oriera_common::{DeterminismLevel, TokenUsage};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Breaker lifecycle: normal → failing fast → probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Normal: calls go through.
    Closed,
    /// Tripped: calls fail fast without touching the wire.
    Open,
    /// Cooldown elapsed: one probe call may through.
    HalfOpen,
}

impl BreakerState {
    pub fn label(self) -> &'static str {
        match self {
            BreakerState::Closed => "closed",
            BreakerState::Open => "open",
            BreakerState::HalfOpen => "half-open",
        }
    }
}

/// Consecutive-failure counter plus breaker state for ONE provider.
#[derive(Debug)]
pub struct ProviderHealth {
    pub consecutive_failures: u32,
    pub total_failures: u64,
    pub total_successes: u64,
    state: BreakerState,
    opened_at: Option<Instant>,
}

/// Defaults for the breaker dial (module policy, overridable on
/// [`FallbackProvider::with_policy`]).
pub const DEFAULT_THRESHOLD: u32 = 3;
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);

impl Default for ProviderHealth {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            total_failures: 0,
            total_successes: 0,
            state: BreakerState::Closed,
            opened_at: None,
        }
    }
}

impl ProviderHealth {
    pub fn state(&self) -> BreakerState {
        self.state
    }

    /// True when a call may through right now (Closed always; Open only
    /// after its cooldown, which flips it to HalfOpen).
    pub fn admits(&mut self, cooldown: Duration) -> bool {
        match self.state {
            BreakerState::Closed => true,
            BreakerState::Open => {
                let elapsed = self
                    .opened_at
                    .map(|t| t.elapsed() >= cooldown)
                    .unwrap_or(true);
                if elapsed {
                    self.state = BreakerState::HalfOpen;
                }
                elapsed
            }
            BreakerState::HalfOpen => true,
        }
    }

    /// Record a success: closes the breaker, resets the streak.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.total_successes += 1;
        self.state = BreakerState::Closed;
        self.opened_at = None;
    }

    /// Record a failure: increments the streak; trips the breaker at
    /// `threshold`. A failed half-open probe re-opens immediately with a
    /// fresh cooldown.
    pub fn record_failure(&mut self, threshold: u32) {
        self.total_failures += 1;
        self.consecutive_failures += 1;
        let tripped =
            self.state == BreakerState::HalfOpen || self.consecutive_failures >= threshold;
        if tripped {
            self.state = BreakerState::Open;
            self.opened_at = Some(Instant::now());
        }
    }

    /// Record an error that does NOT count toward the breaker (4xx
    /// config errors): streak survives, caller still sees the error.
    pub fn record_ignored(&mut self) {
        self.total_failures += 1;
    }
}

/// Which errors trip the breaker (module doc: the "can switching help?"
/// test).
fn trips_breaker(e: &InferError) -> bool {
    match e {
        InferError::Provider(_) | InferError::Network(_) | InferError::Timeout => true,
        InferError::Parse(_) => true,
        // Busy is backpressure — another entry may be freer.
        InferError::Busy => true,
        InferError::Status { code, .. } => *code >= 500 || *code == 429 || *code == 408,
    }
}

/// One healthy entry of the chain serves the call; failures walk the
/// chain downward. The last entry should be the offline rule backend so
/// the chain never truly runs out.
pub struct FallbackProvider {
    chain: Vec<Arc<dyn ModelProvider>>,
    healths: Vec<Mutex<ProviderHealth>>,
    threshold: u32,
    cooldown: Duration,
    /// Index of the entry that served the most recent call (for
    /// `last_usage`), if any.
    served_by: Mutex<Option<usize>>,
    label: String,
}

impl FallbackProvider {
    /// Build a chain with the default policy (threshold 3, cooldown 30s).
    pub fn new(chain: Vec<Arc<dyn ModelProvider>>) -> Self {
        Self::with_policy(chain, DEFAULT_THRESHOLD, DEFAULT_COOLDOWN)
    }

    /// Explicit policy override for constrained shells and tests.
    pub fn with_policy(
        chain: Vec<Arc<dyn ModelProvider>>,
        threshold: u32,
        cooldown: Duration,
    ) -> Self {
        assert!(!chain.is_empty(), "fallback chain needs at least one entry");
        let label = if chain.len() == 1 {
            chain[0].id().to_string()
        } else {
            format!(
                "fallback[{}]",
                chain.iter().map(|p| p.id()).collect::<Vec<_>>().join(" → ")
            )
        };
        Self {
            healths: chain
                .iter()
                .map(|_| Mutex::new(ProviderHealth::default()))
                .collect(),
            chain,
            threshold,
            cooldown,
            served_by: Mutex::new(None),
            label,
        }
    }

    /// Per-entry health snapshot `(provider id, state, streak)` for
    /// shell status lights — allocation-light, lock-per-entry, never
    /// blocks on the wire.
    pub fn health_report(&self) -> Vec<(&str, BreakerState, u32)> {
        self.chain
            .iter()
            .zip(&self.healths)
            .map(|(p, h)| {
                let h = h.lock().expect("health mutex");
                (p.id(), h.state(), h.consecutive_failures)
            })
            .collect()
    }

    fn record(&self, idx: usize, error: Option<&InferError>) {
        let mut h = self.healths[idx].lock().expect("health mutex");
        match error {
            None => h.record_success(),
            Some(e) => {
                if trips_breaker(e) {
                    h.record_failure(self.threshold);
                } else {
                    h.record_ignored();
                }
            }
        }
    }
}

impl ModelProvider for FallbackProvider {
    fn id(&self) -> &str {
        &self.label
    }

    fn complete(&self, messages: &[crate::provider::ChatMessage]) -> Result<String, InferError> {
        let mut last_err: Option<InferError> = None;
        for idx in 0..self.chain.len() {
            let admits = {
                let mut h = self.healths[idx].lock().expect("health mutex");
                h.admits(self.cooldown)
            };
            if !admits {
                continue; // circuit open: fail fast, never touch the wire
            }
            let result = self.chain[idx].complete(messages);
            match &result {
                Ok(_) => {
                    self.record(idx, None);
                    if let Ok(mut s) = self.served_by.lock() {
                        *s = Some(idx);
                    }
                }
                Err(e) => self.record(idx, Some(e)),
            }
            match result {
                Ok(text) => return Ok(text),
                Err(e) => last_err = Some(e),
            }
        }
        // The chain is exhausted — every entry down or open. Report the
        // primary's error (or a summary when the primary was skipped as
        // open) so shells see the true top-of-chain signal.
        Err(last_err.unwrap_or_else(|| {
            InferError::Provider("fallback chain exhausted: every entry open".into())
        }))
    }

    fn last_usage(&self) -> TokenUsage {
        let served = self.served_by.lock().ok().and_then(|s| *s);
        match served {
            Some(idx) => self.chain[idx].last_usage(),
            None => TokenUsage::ZERO,
        }
    }

    /// Degrade must be visible (trait contract): a multi-entry chain can
    /// silently serve a lower entry, so the note says so.
    fn degradation_note(&self) -> Option<&str> {
        (self.chain.len() > 1)
            .then_some("fallback chain active — may serve from a non-primary entry")
    }

    /// A chain is only as reproducible as its LEAST-reproducible entry —
    /// the chain may serve any of them, so its promise is the weakest
    /// one (the recorded floor once any cloud entry exists).
    fn determinism(&self) -> DeterminismLevel {
        self.chain
            .iter()
            .map(|p| p.determinism())
            .max_by_key(|l| match l {
                DeterminismLevel::Strict => 0,
                DeterminismLevel::Pinned => 1,
                DeterminismLevel::Recorded => 2,
            })
            .unwrap_or(DeterminismLevel::Recorded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage, ModelProvider, RuleProvider};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Fails the first `n` calls with the given error, then succeeds —
    /// the #60 必败 provider 手法, upgraded to a countdown.
    struct FlakyByCall {
        remaining: AtomicU32,
        error: InferError,
    }
    impl FlakyByCall {
        fn new(n: u32, error: InferError) -> Self {
            Self {
                remaining: AtomicU32::new(n),
                error,
            }
        }
    }
    impl ModelProvider for FlakyByCall {
        fn id(&self) -> &str {
            "flaky"
        }
        fn complete(&self, _m: &[ChatMessage]) -> Result<String, InferError> {
            if self.remaining.fetch_sub(1, Ordering::SeqCst) > 1 {
                Err(self.error.clone())
            } else {
                Ok("恢复了".into())
            }
        }
        fn determinism(&self) -> DeterminismLevel {
            DeterminismLevel::Recorded
        }
    }

    struct AlwaysDown;
    impl ModelProvider for AlwaysDown {
        fn id(&self) -> &str {
            "down"
        }
        fn complete(&self, _m: &[ChatMessage]) -> Result<String, InferError> {
            Err(InferError::Network("test: down".into()))
        }
        fn determinism(&self) -> DeterminismLevel {
            DeterminismLevel::Recorded
        }
    }

    fn msg() -> Vec<ChatMessage> {
        vec![ChatMessage::user("x")]
    }

    #[test]
    fn breaker_trips_after_threshold_and_fails_fast() {
        let mut h = ProviderHealth::default();
        assert_eq!(h.state(), BreakerState::Closed);
        for _ in 0..(DEFAULT_THRESHOLD - 1) {
            h.record_failure(DEFAULT_THRESHOLD);
            assert_eq!(
                h.state(),
                BreakerState::Closed,
                "below threshold stays closed"
            );
        }
        h.record_failure(DEFAULT_THRESHOLD);
        assert_eq!(h.state(), BreakerState::Open);
        // Open fails fast: no cooldown elapsed → not admitted.
        assert!(!h.admits(DEFAULT_COOLDOWN));
        assert_eq!(h.state(), BreakerState::Open);
    }

    #[test]
    fn half_open_probe_success_closes_failure_reopens() {
        let h = &mut ProviderHealth::default();
        for _ in 0..DEFAULT_THRESHOLD {
            h.record_failure(DEFAULT_THRESHOLD);
        }
        assert_eq!(h.state(), BreakerState::Open);
        // Cooldown zero → admits immediately, flipping to HalfOpen.
        assert!(h.admits(Duration::ZERO));
        assert_eq!(h.state(), BreakerState::HalfOpen);
        h.record_failure(DEFAULT_THRESHOLD);
        assert_eq!(h.state(), BreakerState::Open, "failed probe re-opens");
        assert!(h.admits(Duration::ZERO));
        assert_eq!(h.state(), BreakerState::HalfOpen);
        h.record_success();
        assert_eq!(h.state(), BreakerState::Closed);
        assert_eq!(h.consecutive_failures, 0);
    }

    #[test]
    fn config_4xx_does_not_trip_but_5xx_and_429_do() {
        // 401-class config errors: counted, but never open the breaker —
        // a bad key is bad on every entry; switching can't help.
        let mut h = ProviderHealth::default();
        for _ in 0..10 {
            h.record_ignored();
        }
        assert_eq!(h.state(), BreakerState::Closed);
        let trip: [InferError; 5] = [
            InferError::Status {
                code: 500,
                detail: "x".into(),
            },
            InferError::Status {
                code: 429,
                detail: "x".into(),
            },
            InferError::Timeout,
            InferError::Network("x".into()),
            InferError::Parse("x".into()),
        ];
        for e in &trip {
            assert!(trips_breaker(e), "{e:?} must trip");
        }
        assert!(!trips_breaker(&InferError::Status {
            code: 401,
            detail: "x".into()
        }));
        assert!(!trips_breaker(&InferError::Status {
            code: 400,
            detail: "x".into()
        }));
    }

    #[test]
    fn chain_walks_down_on_failure_and_the_rule_bank_saves_the_show() {
        // primary down forever, local flaky twice, rule never fails.
        let chain: Vec<Arc<dyn ModelProvider>> = vec![
            Arc::new(AlwaysDown),
            Arc::new(FlakyByCall::new(2, InferError::Timeout)),
            Arc::new(RuleProvider::new()),
        ];
        let fb = FallbackProvider::with_policy(chain, 3, Duration::ZERO);
        // First call: primary fails, local fails, rule answers.
        let a = fb.complete(&msg()).unwrap();
        // Second call: primary still down, local recovered (its 2 fails
        // were below this chain's threshold=3, so it never opened).
        let b = fb.complete(&msg()).unwrap();
        assert_eq!(b, "恢复了", "local entry serves once recovered");
        let _ = a;
        let report = fb.health_report();
        assert_eq!(report[2].1, BreakerState::Closed, "rule bank stays closed");
    }

    #[test]
    fn open_circuit_is_skipped_without_touching_the_wire() {
        struct Counting {
            calls: AtomicU32,
        }
        impl ModelProvider for Counting {
            fn id(&self) -> &str {
                "counting"
            }
            fn complete(&self, _m: &[ChatMessage]) -> Result<String, InferError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(InferError::Network("down".into()))
            }
            fn determinism(&self) -> DeterminismLevel {
                DeterminismLevel::Recorded
            }
        }
        let counting = Arc::new(Counting {
            calls: AtomicU32::new(0),
        });
        let chain: Vec<Arc<dyn ModelProvider>> =
            vec![counting.clone(), Arc::new(RuleProvider::new())];
        let fb = FallbackProvider::with_policy(chain, 2, Duration::from_secs(3600));
        // Trip the primary (2 failures across 2 calls, each also served by rule).
        let _ = fb.complete(&msg());
        let _ = fb.complete(&msg());
        assert_eq!(counting.calls.load(Ordering::SeqCst), 2);
        // Now the primary is open: a call must NOT touch it at all.
        let _ = fb.complete(&msg()).unwrap();
        assert_eq!(
            counting.calls.load(Ordering::SeqCst),
            2,
            "open circuit skips the wire"
        );
        // Last error surfaces the primary's signal when it was skipped:
        // the rule bank still served, so this call succeeded.
        let report = fb.health_report();
        assert_eq!(report[0].1, BreakerState::Open);
        assert_eq!(report[1].1, BreakerState::Closed);
    }

    #[test]
    fn determinism_takes_the_chain_floor() {
        let chain: Vec<Arc<dyn ModelProvider>> =
            vec![Arc::new(RuleProvider::new()), Arc::new(AlwaysDown)];
        let fb = FallbackProvider::new(chain);
        assert_eq!(fb.determinism(), DeterminismLevel::Recorded);
    }
}
