//! Materials budget & generation ledger (D1/D3 of PLAN-film-engine):
//! the two cross-cutting disciplines a film-game production line needs
//! that pure compute budgeting does not cover.
//!
//! * [`MaterialsBudget`] — a per-kind money ceiling in micro-yuan. The
//!   R0 field test priced a 5s@720p clip at ¥2.5 (draft) / ¥8.4 (hero);
//!   a chapter is budgeted *before* generation starts, and the batch
//!   stops (not silently overruns) when the ceiling is hit.
//! * [`GenerationLedger`] — idempotent task accounting across process
//!   restarts (the engine-side distillation of interfilm's hand-rolled
//!   m1_state.json): `key -> remote id / status / usage`, JSON round
//!   trip, resume semantics where a succeeded key short-circuits a
//!   resubmission.

use crate::{GenerationKind, UsageReport};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─────────────────────────── MaterialsBudget ───────────────────────────

/// Per-kind spend ceiling + running total, micro-yuan (1e-6 CNY).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MaterialsBudget {
    /// 0 = no ceiling for that kind (the default everywhere).
    #[serde(default)]
    pub ceilings: HashMap<GenerationKind, u64>,
    #[serde(default)]
    pub spent: HashMap<GenerationKind, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetVerdict {
    /// Charged; remaining headroom in micro-yuan.
    Charged { remaining: Option<u64> },
    /// Would exceed the kind's ceiling — the batch must stop or degrade
    /// tiers, never silently overrun.
    OverCeiling { ceiling: u64, spent: u64 },
}

impl MaterialsBudget {
    pub fn with_ceiling(mut self, kind: GenerationKind, micros: u64) -> Self {
        self.ceilings.insert(kind, micros);
        self
    }

    /// Charge `micros` against `kind` if the ceiling allows it.
    pub fn charge(&mut self, kind: GenerationKind, micros: u64) -> BudgetVerdict {
        let spent = *self.spent.get(&kind).unwrap_or(&0);
        if let Some(&ceiling) = self.ceilings.get(&kind) {
            if spent + micros > ceiling {
                return BudgetVerdict::OverCeiling { ceiling, spent };
            }
        }
        *self.spent.entry(kind).or_insert(0) += micros;
        let remaining = self
            .ceilings
            .get(&kind)
            .map(|c| c.saturating_sub(spent + micros));
        BudgetVerdict::Charged { remaining }
    }

    /// Convenience: charge an asset's vendor-reported cost.
    pub fn charge_usage(&mut self, kind: GenerationKind, usage: &UsageReport) -> BudgetVerdict {
        self.charge(kind, usage.cost_micros.unwrap_or(0))
    }
}

// ─────────────────────────── GenerationLedger ───────────────────────────

/// One ledger row. `remote_id` is vendor-side; `local_key` is the
/// production-line identity (e.g. `s002a_0`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerRow {
    pub local_key: String,
    pub remote_id: String,
    pub status: LedgerStatus,
    #[serde(default)]
    pub usage: Option<UsageReport>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LedgerStatus {
    Submitted,
    Succeeded,
    Failed,
}

/// Idempotent task accounting: resubmission of an already-succeeded key
/// is a no-op (`reserve` says so), restarts reload from JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GenerationLedger {
    rows: HashMap<String, LedgerRow>,
}

impl GenerationLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Should the caller submit this key? `false` when the ledger
    /// already holds a terminal (succeeded) row — resume semantics.
    pub fn should_submit(&self, local_key: &str) -> bool {
        self.rows
            .get(local_key)
            .is_none_or(|r| r.status != LedgerStatus::Succeeded)
    }

    pub fn record(&mut self, row: LedgerRow) {
        self.rows.insert(row.local_key.clone(), row);
    }

    pub fn get(&self, local_key: &str) -> Option<&LedgerRow> {
        self.rows.get(local_key)
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Total vendor-reported cost across succeeded rows (micro-yuan).
    pub fn total_cost_micros(&self) -> u64 {
        self.rows
            .values()
            .filter(|r| r.status == LedgerStatus::Succeeded)
            .map(|r| r.usage.and_then(|u| u.cost_micros).unwrap_or(0))
            .sum()
    }

    /// Persistence round trip (the m1_state.json contract, engine-side).
    pub fn to_json(&self) -> String {
        serde_json::json!({ "rows": self.rows.values().collect::<Vec<_>>() }).to_string()
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
        let v: serde_json::Value = serde_json::from_str(s).map_err(|e| e.to_string())?;
        let rows = v
            .get("rows")
            .and_then(|r| r.as_array())
            .ok_or("missing rows array")?;
        let mut ledger = Self::new();
        for r in rows {
            let row: LedgerRow = serde_json::from_value(r.clone()).map_err(|e| e.to_string())?;
            ledger.record(row);
        }
        Ok(ledger)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_stops_at_the_ceiling_instead_of_overrunning() {
        // Field-tested price point: a draft clip ≈ ¥2.5 = 2_500_000 micros.
        let mut b = MaterialsBudget::default().with_ceiling(GenerationKind::TextToVideo, 3_000_000);
        match b.charge(GenerationKind::TextToVideo, 2_484_000) {
            BudgetVerdict::Charged { remaining: Some(r) } => assert!(r >= 500_000),
            other => panic!("first charge should pass: {other:?}"),
        }
        match b.charge(GenerationKind::TextToVideo, 2_484_000) {
            BudgetVerdict::OverCeiling {
                ceiling: 3_000_000, ..
            } => {}
            other => panic!("second charge must refuse: {other:?}"),
        }
        // unceilinged kinds always charge
        assert!(matches!(
            b.charge(GenerationKind::TextToImage, 120_000),
            BudgetVerdict::Charged { .. }
        ));
    }

    #[test]
    fn ledger_resume_skips_succeeded_and_survives_restart() {
        let mut l = GenerationLedger::new();
        assert!(l.should_submit("s002a_0"));
        l.record(LedgerRow {
            local_key: "s002a_0".into(),
            remote_id: "cgt-1".into(),
            status: LedgerStatus::Succeeded,
            usage: Some(UsageReport {
                tokens: 108_900,
                cost_micros: Some(2_484_000),
            }),
        });
        assert!(!l.should_submit("s002a_0"), "resume must skip succeeded");
        assert!(l.should_submit("s002b_0"), "failed/missing keys resubmit");
        l.record(LedgerRow {
            local_key: "s002b_0".into(),
            remote_id: "cgt-2".into(),
            status: LedgerStatus::Failed,
            usage: None,
        });
        assert!(l.should_submit("s002b_0"), "failed keys resubmit on retry");

        // restart: JSON round trip keeps semantics and the bill
        let restored = GenerationLedger::from_json(&l.to_json()).unwrap();
        assert_eq!(restored.len(), 2);
        assert!(!restored.should_submit("s002a_0"));
        assert_eq!(restored.total_cost_micros(), 2_484_000);
    }
}
