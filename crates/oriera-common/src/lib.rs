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
}
