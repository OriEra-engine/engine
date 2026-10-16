//! L0 Common — shared types used across all layers.
//! No dependencies on other oriera crates.

/// Nanoseconds since the UNIX epoch (wall clock).
///
/// Used for logs and metrics. Frame timing and anything feeding the
/// deterministic replay (book T5) must use the L2 frame counter instead,
/// never wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub fn now() -> Self {
        Self(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        )
    }
    /// Seconds elapsed from `since` to `self` (0 if `since` is later).
    pub fn elapsed(self, since: Self) -> f32 {
        (self.0.saturating_sub(since.0)) as f32 / 1_000_000_000.0
    }
}

/// Frame counter, starts at 0, increments each tick. Deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameIndex(pub u64);

/// Model token accounting for one call or a running total (book T15
/// metering). L7 providers report it, L9 systems accumulate it, the
/// app/UI layer reads it — one vocabulary across layers, so a usage
/// meter never re-derives units per provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    pub prompt: u64,
    pub completion: u64,
}

impl TokenUsage {
    pub const ZERO: TokenUsage = TokenUsage {
        prompt: 0,
        completion: 0,
    };

    pub fn total(self) -> u64 {
        self.prompt + self.completion
    }
}

/// One system's compute allocation for a frame (book T3 §3.5, T15).
///
/// The shared vocabulary of the compute-budget pipeline: L2's allocator
/// produces one `ComputeGrant` per system each frame, [`FrameCtx`](oriera_core)
/// carries it into every `run`, and spend-hungry systems (L7 inference
/// queue, L9 cognition cadence, …) scale their work by `throttle`
/// instead of inventing their own pacing. `share_ms` is the slice of
/// the frame budget the allocator judges this system may have; the
/// throttle is how hard it is currently being asked to slow down.
///
/// A system that finds itself in the table must never assume it will be
/// next frame — grants are re-derived from observation every tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComputeGrant {
    /// The slice of the frame budget allocated to this system, in ms.
    pub share_ms: f32,
    /// Work-rate dial in `[0, 1]`: `1.0` = full speed, lower = do less
    /// this frame (fewer commands, longer cadence, coarser LOD).
    /// Never negative even if a caller feeds garbage into the allocator.
    pub throttle: f32,
}

impl Default for ComputeGrant {
    fn default() -> Self {
        // Full speed, no share promised: the "not managed" grant.
        Self {
            share_ms: f32::INFINITY,
            throttle: 1.0,
        }
    }
}

impl ComputeGrant {
    /// Clamp a work quantity (commands, polls, iterations) to the
    /// throttled rate, never below `min` — the one helper every
    /// consumer needs so off-by-one rounding lives in one place.
    pub fn scale_usize(self, quantity: usize, min: usize) -> usize {
        let scaled = (quantity as f32 * self.throttle.clamp(0.0, 1.0)).round() as usize;
        scaled.max(min)
    }
}

/// Read-only grant table handed to systems inside the frame context.
///
/// Lookup is by system name; a missing entry means "unmanaged" and
/// returns [`ComputeGrant::default`] (full throttle) — plugging a new
/// system in must never require touching the allocator first. Backed by
/// a borrowed slice so carrying it through a frame is allocation-free.
#[derive(Debug, Clone, Copy)]
pub struct Grants<'a> {
    entries: &'a [(String, ComputeGrant)],
}

impl<'a> Grants<'a> {
    /// An empty table: every lookup answers "unmanaged, full speed".
    pub const EMPTY: Grants<'static> = Grants { entries: &[] };

    pub fn new(entries: &'a [(String, ComputeGrant)]) -> Self {
        Self { entries }
    }

    /// The grant for `name`, or the unmanaged default when absent.
    pub fn get(&self, name: &str) -> ComputeGrant {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, g)| *g)
            .unwrap_or_default()
    }

    /// Just the throttle dial for `name` (1.0 when unmanaged).
    pub fn throttle_of(&self, name: &str) -> f32 {
        self.get(name).throttle
    }

    /// True when the table knows this system at all.
    pub fn is_managed(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, _)| n == name)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl std::ops::Add for TokenUsage {
    type Output = TokenUsage;
    fn add(self, rhs: TokenUsage) -> TokenUsage {
        TokenUsage {
            prompt: self.prompt + rhs.prompt,
            completion: self.completion + rhs.completion,
        }
    }
}

impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, rhs: TokenUsage) {
        self.prompt += rhs.prompt;
        self.completion += rhs.completion;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_is_zero_for_later_since() {
        let a = Timestamp(1_000);
        let b = Timestamp(2_000);
        assert!((b.elapsed(a) - 0.000_001).abs() < 1e-9);
        assert_eq!(a.elapsed(b), 0.0);
    }

    #[test]
    fn token_usage_accumulates() {
        let a = TokenUsage {
            prompt: 10,
            completion: 5,
        };
        let mut total = a + TokenUsage::ZERO;
        total += TokenUsage {
            prompt: 1,
            completion: 2,
        };
        assert_eq!(total.total(), 18);
        assert_eq!(
            total,
            TokenUsage {
                prompt: 11,
                completion: 7
            }
        );
    }

    #[test]
    fn empty_grants_leave_every_system_unmanaged() {
        let g = Grants::EMPTY;
        assert!(g.is_empty());
        assert_eq!(g.get("infer"), ComputeGrant::default());
        assert_eq!(g.throttle_of("anything"), 1.0);
        assert!(!g.is_managed("anything"));
    }

    #[test]
    fn grants_lookup_finds_the_right_system() {
        let entries = vec![
            (
                "infer".to_string(),
                ComputeGrant {
                    share_ms: 4.0,
                    throttle: 0.5,
                },
            ),
            (
                "render".to_string(),
                ComputeGrant {
                    share_ms: 8.0,
                    throttle: 1.0,
                },
            ),
        ];
        let g = Grants::new(&entries);
        assert!(g.is_managed("infer"));
        assert_eq!(g.throttle_of("infer"), 0.5);
        assert!((g.get("render").share_ms - 8.0).abs() < 1e-6);
        // Unknown systems are unmanaged, not errors.
        assert_eq!(g.get("physics"), ComputeGrant::default());
        // Lookup is exact, not prefix/contains.
        assert!(!g.is_managed("inf"));
    }

    #[test]
    fn scale_usize_respects_minimum_and_clamps_garbage() {
        let full = ComputeGrant {
            share_ms: 1.0,
            throttle: 1.0,
        };
        assert_eq!(full.scale_usize(32, 1), 32);

        let half = ComputeGrant {
            share_ms: 1.0,
            throttle: 0.5,
        };
        assert_eq!(half.scale_usize(9, 1), 5); // rounds 4.5 up
        assert_eq!(half.scale_usize(1, 1), 1); // never below min

        // A negative or >1 throttle (never produced by the allocator,
        // but a hallucinated model value could land here) clamps clean.
        let wild = ComputeGrant {
            share_ms: 1.0,
            throttle: -3.0,
        };
        assert_eq!(wild.scale_usize(10, 2), 2);
        let wild2 = ComputeGrant {
            share_ms: 1.0,
            throttle: 42.0,
        };
        assert_eq!(wild2.scale_usize(10, 2), 10);
    }
}
