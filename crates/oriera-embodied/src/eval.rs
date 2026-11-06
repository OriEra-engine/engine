//! Versioned evaluation protocols (M0b, PLAN-embodied-general §9.1
//! P-1) — one `evaluate()` for every entrance, one protocol id per
//! number.
//!
//! Three entrances used to speak three dialects (8 vs 12 episodes,
//! three seed spellings, divergent degenerate handling) and the 90%
//! gate panicked on lab runs that only wanted a record. M0b gives the
//! disagreement a *name*:
//!
//! - [`EvalProtocol::course`] — `course@v1`: the classroom contract
//!   (12 episodes, degenerate episodes redrawn out-of-budget, ≥90%
//!   hard gate on reach/pick_place). Failure is 待办语义 not a
//!   backtrace (G-2) — the caller prints the next steps and exits 2.
//! - [`EvalProtocol::paper`] — `paper@v1`: the lab contract (≥100
//!   episodes, gate = [`Gate::Report`] — record, never panic).
//!
//! The protocol id **and** its parameter hash ride in
//! `eval_report.json` next to the `spec_hash`, so "course@v1, 12
//! episodes, seed0 1000" is part of the experiment identity and a
//! half-year-old number can be re-derived byte for byte.
//!
//! ```no_run
//! use oriera_embodied::eval::{EvalProtocol, EpisodeOutcome, EpisodeRow, evaluate};
//! let p = EvalProtocol::parse("course@v1", Some(12), 1000).unwrap();
//! let result = evaluate(&p, |seed, _draw| {
//!     Ok(EpisodeOutcome::Done(EpisodeRow {
//!         seed, success: true, steps: 120, reward_sum: 3.0,
//!         fingerprint_v1: 42, rand: "{}".into(),
//!     }))
//! }).unwrap();
//! assert_eq!(result.rows.len(), 12);
//! ```
//!
//! Success-criteria semantics (pick requires released+resting, push
//! forbids holding) stay `success_criteria = "v1"`; the revision lands
//! as `@v2` in M2 — the field is the slot that makes that change
//! visible in every report from day one.

use crate::spec::fold_json;
use serde::{Deserialize, Serialize};

/// Classroom protocol id: 12 episodes + 90% hard gate (the pre-M0b
/// behaviour, named).
pub const PROTOCOL_COURSE_V1: &str = "course@v1";
/// Lab protocol id: ≥[`PAPER_MIN_EPISODES`] episodes, record-only.
pub const PROTOCOL_PAPER_V1: &str = "paper@v1";
/// Success-semantics revision tag (`pick 松爪+静止 / push 禁 holding`
/// is `v2`, M2 — see module docs).
pub const SUCCESS_CRITERIA_V1: &str = "v1";
/// `@v2` success semantics (M2): pick = zone AND released AND resting;
/// push = zone AND no holding frame; every episode dual-reports
/// [`SuccessView`]. New ids only — v1 tasks are read-only (P-2).
pub const SUCCESS_CRITERIA_V2: &str = "v2";
/// `paper@v1` minimum episode count (a Wilson interval on <100 episodes
/// is too wide to argue with a reviewer).
pub const PAPER_MIN_EPISODES: u32 = 100;
/// The hard gate threshold (`Gate::Hard90`).
pub const HARD_GATE_RATE: f64 = 0.9;

/// What happens to a degenerate episode (spawn landed in the goal zone,
/// terminated on step ~1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Degenerate {
    /// Discard it and redraw a fresh seed from a displaced band — it
    /// does not consume the episode budget (the pre-M0b fix, named).
    Redraw,
}

/// How a below-threshold success rate is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    /// Hard 90% gate: the run refuses to publish data (`Hard90`).
    Hard90,
    /// Record the verdict in the report, never fail the process
    /// (`Report`).
    Report,
}

/// A named evaluation protocol: id + every knob that moves the number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalProtocol {
    /// [`PROTOCOL_COURSE_V1`] or [`PROTOCOL_PAPER_V1`].
    pub id: String,
    /// Episodes to keep (degenerate redraws are extra draws).
    pub episodes: u32,
    /// First episode seed; episode *i* (before redraws) is `seed0 + i`.
    pub seed0: u64,
    pub degenerate: Degenerate,
    pub gate: Gate,
    /// Success-semantics revision (see module docs).
    pub success_criteria: String,
}

impl EvalProtocol {
    /// `course@v1` with the classroom defaults (12 episodes).
    pub fn course(episodes: Option<u32>, seed0: u64) -> Self {
        Self {
            id: PROTOCOL_COURSE_V1.into(),
            episodes: episodes.unwrap_or(12),
            seed0,
            degenerate: Degenerate::Redraw,
            gate: Gate::Hard90,
            success_criteria: SUCCESS_CRITERIA_V1.into(),
        }
    }

    /// `paper@v1` (≥100 episodes, report-only). Fewer episodes is an
    /// honest error naming the minimum — that's the whole point of a
    /// named protocol.
    pub fn paper(episodes: Option<u32>, seed0: u64) -> Result<Self, String> {
        let episodes = episodes.unwrap_or(PAPER_MIN_EPISODES);
        if episodes < PAPER_MIN_EPISODES {
            return Err(format!(
                "paper@v1：至少 {} 集（当前 {}）——少于这个数的 Wilson 区间宽到没法跟审稿人 argue；要课程口径请用 course@v1",
                PAPER_MIN_EPISODES, episodes
            ));
        }
        Ok(Self {
            id: PROTOCOL_PAPER_V1.into(),
            episodes,
            seed0,
            degenerate: Degenerate::Redraw,
            gate: Gate::Report,
            success_criteria: SUCCESS_CRITERIA_V1.into(),
        })
    }

    /// CLI entry: `--protocol course@v1|paper@v1` (default
    /// `course@v1` = the legacy command, zero flag changes).
    pub fn parse(id: &str, episodes: Option<u32>, seed0: u64) -> Result<Self, String> {
        match id {
            PROTOCOL_COURSE_V1 => Ok(Self::course(episodes, seed0)),
            PROTOCOL_PAPER_V1 => Self::paper(episodes, seed0),
            other => Err(format!(
                "--protocol {other:?}：没有这个协议（合法值：\"{PROTOCOL_COURSE_V1}\" = 12 集 + 90% 硬闸，\"{PROTOCOL_PAPER_V1}\" = ≥{PAPER_MIN_EPISODES} 集只记录）"
            )),
        }
    }

    /// Override the gate from `--gate=hard|report`. `paper@v1` refuses
    /// a hard gate — the lab contract is record-only, by construction.
    pub fn with_gate(mut self, gate: Option<Gate>) -> Result<Self, String> {
        if let Some(g) = gate {
            if self.id == PROTOCOL_PAPER_V1 && g == Gate::Hard90 {
                return Err(format!(
                    "--gate=hard 与 {PROTOCOL_PAPER_V1} 冲突：paper 协议只记录不拦流程（要硬闸用 course@v1）"
                ));
            }
            self.gate = g;
        }
        Ok(self)
    }

    /// Full validation (paper floor + id plausibility).
    pub fn validate(&self) -> Result<(), String> {
        if self.episodes == 0 {
            return Err(format!("{}.episodes = 0：至少要采 1 集", self.id));
        }
        if self.id == PROTOCOL_PAPER_V1 && self.episodes < PAPER_MIN_EPISODES {
            return Self::paper(Some(self.episodes), self.seed0)
                .err()
                .map_or(Ok(()), Err);
        }
        if self.id != PROTOCOL_COURSE_V1 && self.id != PROTOCOL_PAPER_V1 {
            return Err(format!("protocol.id：未知协议 {:?}", self.id));
        }
        if self.success_criteria != SUCCESS_CRITERIA_V1
            && self.success_criteria != SUCCESS_CRITERIA_V2
        {
            return Err(format!(
                "protocol.success_criteria：{self:?} 只认 {SUCCESS_CRITERIA_V1:?} / {SUCCESS_CRITERIA_V2:?}（判据修订是 @v2 新任务 id，不原地改）",
            ));
        }
        Ok(())
    }

    /// Set the success-semantics revision (`"v1"` / `"v2"`), validated.
    /// The value folds into [`Self::protocol_hash`] — criteria are part
    /// of the experiment identity.
    pub fn with_success_criteria(mut self, criteria: &str) -> Result<Self, String> {
        if criteria != SUCCESS_CRITERIA_V1 && criteria != SUCCESS_CRITERIA_V2 {
            return Err(format!(
                "success_criteria = {criteria:?}：只认 \"v1\"（区间判据）或 \"v2\"（松爪+静止 / 禁 holding）"
            ));
        }
        self.success_criteria = criteria.into();
        Ok(self)
    }

    /// The protocol's parameter hash: canonical JSON of every knob,
    /// same splitmix fold as [`crate::spec::SceneSpec::spec_hash`].
    /// `course@v1` with 12 episodes and seed0 1000 always folds to the
    /// same 16 hex digits; any knob moves it.
    pub fn protocol_hash(&self) -> u64 {
        let v = serde_json::to_value(self).expect("EvalProtocol serializes");
        fold_json(v.to_string().as_bytes())
    }

    /// `protocol_hash` as 16 hex digits.
    pub fn protocol_hash_hex(&self) -> String {
        format!("{:016x}", self.protocol_hash())
    }

    /// First 8 hex digits (the `runs/<hash8>/` key combines spec+protocol).
    pub fn protocol_hash8(&self) -> String {
        self.protocol_hash_hex()[..8].to_string()
    }

    /// The seed of draw `draw` (1-based) after `redraws` displaced
    /// re-draws — the exact legacy band (`seed0 + i + 65536·redraws`),
    /// so pre-M0b seed lists reproduce verbatim under `course@v1`.
    pub fn episode_seed(&self, draw: u64, redraws: u64) -> u64 {
        self.seed0 + (draw - 1) + 65536 * redraws
    }

    /// Does the gate *fail* the process for this task? `course@v1`
    /// gates reach/pick_place (push's contact baseline is honestly
    /// unstable — recorded, never gating, the pre-M0b carve-out);
    /// `paper@v1` never gates.
    pub fn gate_applies(&self, task_id: &str) -> bool {
        matches!(self.gate, Gate::Hard90) && matches!(task_id, "reach@v1" | "pick_place@v1")
    }

    /// The verdict line for the report/CLI.
    pub fn gate_verdict(&self, task_id: &str, rate: f64) -> GateVerdict {
        if !self.gate_applies(task_id) {
            return GateVerdict::NotApplied;
        }
        if rate >= HARD_GATE_RATE {
            GateVerdict::Pass
        } else {
            GateVerdict::Fail {
                threshold: HARD_GATE_RATE,
                rate,
            }
        }
    }
}

/// Outcome of the gate check for one eval run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateVerdict {
    /// Gate applies and the rate clears the threshold.
    Pass,
    /// Gate applies and the rate is below the threshold — `course@v1`
    /// refuses to publish; `Report` gates just record this.
    Fail { threshold: f64, rate: f64 },
    /// No gate for this protocol/task (push, or `paper@v1`).
    NotApplied,
}

/// One kept episode, as the report and the receipt need it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeRow {
    pub seed: u64,
    pub success: bool,
    pub steps: u64,
    pub reward_sum: f32,
    /// Scheme-**v1** episode fingerprint (the `episode_state_hashes`
    /// of every existing receipt — kept for anchor continuity).
    pub fingerprint_v1: u64,
    /// Randomization receipt of the episode's reset draw.
    #[serde(default)]
    pub rand: String,
}

/// What one episode run reports back to [`evaluate`].
#[derive(Debug, Clone, PartialEq)]
pub enum EpisodeOutcome {
    /// Degenerate (spawn-in-goal): discard, redraw out of budget.
    Degenerate,
    /// A kept episode.
    Done(EpisodeRow),
}

/// The result of one `evaluate()` pass.
#[derive(Debug, Clone)]
pub struct EvalResult {
    pub protocol: EvalProtocol,
    /// Kept episodes, in draw order.
    pub rows: Vec<EpisodeRow>,
    /// Total draws (kept + degenerate).
    pub draws: u64,
    /// Degenerate re-draws (the displaced seed band count).
    pub redraws: u64,
}

impl EvalResult {
    pub fn successes(&self) -> usize {
        self.rows.iter().filter(|r| r.success).count()
    }

    pub fn success_rate(&self) -> f64 {
        self.successes() as f64 / self.rows.len().max(1) as f64
    }

    /// Wilson 95% score interval — the honest way to report a success
    /// rate from n episodes (never a bare point estimate).
    pub fn wilson95(&self) -> (f64, f64) {
        wilson95(self.successes(), self.rows.len())
    }

    pub fn gate_verdict(&self, task_id: &str) -> GateVerdict {
        self.protocol.gate_verdict(task_id, self.success_rate())
    }
}

/// Run a protocol end to end: the seed band, the degenerate redraw
/// policy and the episode budget all live HERE, so every entrance
/// (CLI / Python / Studio) speaks one dialect (P-1). `run_episode`
/// gets the fully determined seed and returns [`EpisodeOutcome`].
pub fn evaluate<F>(protocol: &EvalProtocol, mut run_episode: F) -> Result<EvalResult, String>
where
    F: FnMut(u64, u64) -> Result<EpisodeOutcome, String>,
{
    protocol.validate()?;
    let mut rows = Vec::new();
    let mut kept = 0u32;
    let mut draws = 0u64;
    let mut redraws = 0u64;
    // A redraw flood means the task's spawn band is broken, not that
    // the lab asked for the wrong protocol — cap and say so.
    let redraw_cap = 50 * protocol.episodes as u64 + 200;
    while kept < protocol.episodes {
        draws += 1;
        let seed = protocol.episode_seed(draws, redraws);
        match run_episode(seed, draws)
            .map_err(|e| format!("episode draw {draws}（seed {seed}）失败：{e}"))?
        {
            EpisodeOutcome::Degenerate => {
                redraws += 1;
                if redraws > redraw_cap {
                    return Err(format!(
                        "退化重抽 {redraws} 次（上限 {redraw_cap}）：生成带几乎全部落进目标区——先查 task 的 spawn 范围，这不是协议参数问题"
                    ));
                }
            }
            EpisodeOutcome::Done(row) => {
                kept += 1;
                rows.push(row);
            }
        }
    }
    Ok(EvalResult {
        protocol: protocol.clone(),
        rows,
        draws,
        redraws,
    })
}

/// Wilson 95% score interval (0.975 quantile).
pub fn wilson95(k: usize, n: usize) -> (f64, f64) {
    let z = 1.959_963_984_540_054_f64;
    if n == 0 {
        return (0.0, 1.0);
    }
    let (k, n) = (k as f64, n as f64);
    let p = k / n;
    let denom = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denom;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denom;
    (
        (center - half).clamp(0.0, 1.0),
        (center + half).clamp(0.0, 1.0),
    )
}

/// Per-episode dual success view (M2, review P-2): `once` = the
/// criterion held on ANY frame of the episode, `at_end` = on the FINAL
/// frame. v1 tasks carry `SuccessView::legacy(success)` — once and
/// at_end collapse into the old boolean because v1 terminates on first
/// success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SuccessView {
    pub once: bool,
    pub at_end: bool,
}

impl SuccessView {
    /// The pre-M2 collapsed view (v1 tasks keep the old semantics).
    pub fn legacy(success: bool) -> Self {
        Self {
            once: success,
            at_end: success,
        }
    }
}

/// Both success rates over one episode set, with Wilson intervals —
/// the eval_report.json 双报 block.
#[derive(Debug, Clone, Serialize)]
pub struct DualSuccessSummary {
    pub episodes: usize,
    /// Episodes where the criterion held on some frame.
    pub successes_once: usize,
    /// Episodes where it still held on the final frame.
    pub successes_at_end: usize,
    pub success_once_rate: f64,
    pub success_at_end_rate: f64,
    pub once_wilson95: (f64, f64),
    pub at_end_wilson95: (f64, f64),
}

/// Summarize the dual view over the kept episodes (draw order).
pub fn summarize_success(views: &[SuccessView]) -> DualSuccessSummary {
    let once = views.iter().filter(|v| v.once).count();
    let at_end = views.iter().filter(|v| v.at_end).count();
    let n = views.len();
    DualSuccessSummary {
        episodes: n,
        successes_once: once,
        successes_at_end: at_end,
        success_once_rate: once as f64 / n.max(1) as f64,
        success_at_end_rate: at_end as f64 / n.max(1) as f64,
        once_wilson95: wilson95(once, n),
        at_end_wilson95: wilson95(at_end, n),
    }
}

// ---------------------------------------------------------------------------
// Failure-phase telemetry (P2, PLAN-embodied-v3 P2 深水区 Wave-1 · P 包):
// WHICH phase a failed episode died in — reach / grasp / lift / place —
// judged from channels every env already exposes (TCP and object
// positions, the goal, the `holding` flag). Pure bookkeeping on top of the
// episode loop: it reads, never writes, so trajectories and every receipt
// key stay byte-identical; the report block is opt-in at the entrance
// (`embodied_sim_baseline --failure-phases`). Rules live in
// docs/LAB-GUIDE.md §2.2 and are versioned by `FAILURE_PHASE_SCHEME_V1`.
// ---------------------------------------------------------------------------

/// Scheme tag of the failure-phase rules below — bump it (new constant)
/// on any rule change; a report always names the rules it was judged by.
pub const FAILURE_PHASE_SCHEME_V1: &str = "failure_phase@v1";

/// The phase a failed episode died in (pick-family order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePhase {
    /// The TCP never entered the grasp zone around the object.
    Reach,
    /// Reached, but `holding` never turned true.
    Grasp,
    /// Held, but the object never arrived over the goal under grip
    /// (slipped out on the lift, dropped in transit; drawer: the drag).
    Lift,
    /// Delivered over the goal under grip, the episode still failed
    /// (released off-zone, not resting, still held at the end, bounced).
    Place,
}

impl FailurePhase {
    /// All four, in pipeline order (report/CSV column order).
    pub const ALL: [Self; 4] = [Self::Reach, Self::Grasp, Self::Lift, Self::Place];

    /// The snake_case report key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reach => "reach",
            Self::Grasp => "grasp",
            Self::Lift => "lift",
            Self::Place => "place",
        }
    }
}

/// Which phases a task family has: reach tasks die only in `reach`;
/// push has no grasp (`reach` → `place`); every grasp task (pick_place /
/// bin_pick / stack / drawer) runs all four.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseFamily {
    Reach,
    Push,
    Pick,
}

/// The classifier thresholds (recorded next to every summary).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FailurePhaseRules {
    pub family: PhaseFamily,
    /// TCP→object distance that counts as "reached" (m): the engine's
    /// grasp zone [`oriera_subsys::GRASP_RADIUS`] (0.06) — the `TipRadius`
    /// holding criterion's own radius.
    pub reach_radius: f32,
    /// Horizontal object→goal distance that counts as "delivered" while
    /// held (m): the task's own zone tolerance — `goal_xz_tol` for
    /// pick_place / bin_pick / push, `stack_xz_tol` for stack,
    /// `pull_distance / 4` for drawer (its goal sits `pull_distance`
    /// from the spawn, so the zone tolerance would count the spawn as
    /// delivered).
    pub deliver_xz: f32,
}

impl FailurePhaseRules {
    /// The rules for a task id (`base@vN` — the version does not move the
    /// phase taxonomy) and its spec-resolved thresholds.
    pub fn for_task(task_id: &str, params: &crate::embodied_env::TaskParams) -> Self {
        let base = task_id.split('@').next().unwrap_or(task_id);
        let (family, deliver_xz) = match base {
            "reach" => (PhaseFamily::Reach, params.reach_radius),
            "push" => (PhaseFamily::Push, params.goal_xz_tol),
            "stack" => (PhaseFamily::Pick, params.stack_xz_tol),
            "drawer" => (PhaseFamily::Pick, params.pull_distance * 0.25),
            _ => (PhaseFamily::Pick, params.goal_xz_tol),
        };
        Self {
            family,
            reach_radius: oriera_subsys::GRASP_RADIUS,
            deliver_xz,
        }
    }
}

/// Per-episode phase signals, folded tick by tick (after every `step`)
/// from read-only env channels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PhaseSignals {
    /// Object position at reset (settled spawn) — the lift reference.
    pub obj0: [f32; 3],
    /// Ticks observed.
    pub ticks: u64,
    /// min |TCP − object| over the episode (m).
    pub min_ee_obj: f32,
    /// `holding` true on any tick.
    pub ever_holding: bool,
    /// Ticks with `holding` true.
    pub holding_ticks: u64,
    /// max(object.y − obj0.y) over holding ticks (m; 0 if never held).
    pub max_lift_held: f32,
    /// min horizontal |object − goal| over holding ticks (m; `None` if
    /// never held).
    pub min_goal_xz_held: Option<f32>,
    /// Horizontal |object − goal| on the last tick (m).
    pub final_goal_xz: f32,
    /// `holding` on the last tick.
    pub holding_at_end: bool,
}

impl PhaseSignals {
    /// Start an episode's fold at the reset pose of the task object.
    pub fn start(obj0: [f32; 3]) -> Self {
        Self {
            obj0,
            ticks: 0,
            min_ee_obj: f32::MAX,
            ever_holding: false,
            holding_ticks: 0,
            max_lift_held: 0.0,
            min_goal_xz_held: None,
            final_goal_xz: f32::MAX,
            holding_at_end: false,
        }
    }

    /// Fold one post-step observation: TCP, object, goal, holding flag.
    pub fn observe(&mut self, ee: [f32; 3], obj: [f32; 3], goal: [f32; 3], holding: bool) {
        let d_ee =
            ((ee[0] - obj[0]).powi(2) + (ee[1] - obj[1]).powi(2) + (ee[2] - obj[2]).powi(2)).sqrt();
        let d_goal_xz = ((obj[0] - goal[0]).powi(2) + (obj[2] - goal[2]).powi(2)).sqrt();
        self.ticks += 1;
        self.min_ee_obj = self.min_ee_obj.min(d_ee);
        self.final_goal_xz = d_goal_xz;
        self.holding_at_end = holding;
        if holding {
            self.ever_holding = true;
            self.holding_ticks += 1;
            self.max_lift_held = self.max_lift_held.max(obj[1] - self.obj0[1]);
            self.min_goal_xz_held = Some(
                self.min_goal_xz_held
                    .map_or(d_goal_xz, |m: f32| m.min(d_goal_xz)),
            );
        }
    }
}

/// Classify one episode (`failure_phase@v1`): `None` for a success,
/// otherwise the FIRST phase whose milestone was never reached —
///
/// 1. `min_ee_obj > reach_radius` → [`FailurePhase::Reach`];
/// 2. never `holding` → [`FailurePhase::Grasp`] (push: → `Place`, it has
///    no grasp; reach tasks stop at step 1 — any failure is `Reach`);
/// 3. never held within `deliver_xz` (horizontal) of the goal →
///    [`FailurePhase::Lift`];
/// 4. otherwise → [`FailurePhase::Place`].
pub fn classify_failure(
    success: bool,
    s: &PhaseSignals,
    rules: &FailurePhaseRules,
) -> Option<FailurePhase> {
    if success {
        return None;
    }
    let reached = s.min_ee_obj <= rules.reach_radius;
    Some(match rules.family {
        PhaseFamily::Reach => FailurePhase::Reach,
        PhaseFamily::Push if !reached => FailurePhase::Reach,
        PhaseFamily::Push => FailurePhase::Place,
        PhaseFamily::Pick if !reached => FailurePhase::Reach,
        PhaseFamily::Pick if !s.ever_holding => FailurePhase::Grasp,
        PhaseFamily::Pick if s.min_goal_xz_held.is_none_or(|d| d > rules.deliver_xz) => {
            FailurePhase::Lift
        }
        PhaseFamily::Pick => FailurePhase::Place,
    })
}

/// Failed-episode counts per phase (serialized in pipeline order).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseCounts {
    pub reach: usize,
    pub grasp: usize,
    pub lift: usize,
    pub place: usize,
}

/// Failed seeds per phase (drill-down: `ORIERA_SIM_TRACE` one of them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseSeeds {
    pub reach: Vec<u64>,
    pub grasp: Vec<u64>,
    pub lift: Vec<u64>,
    pub place: Vec<u64>,
}

/// The `failure_phases` report block: the scheme, the rules, the
/// per-phase failure counts and seeds over one episode set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailurePhaseSummary {
    /// [`FAILURE_PHASE_SCHEME_V1`].
    pub scheme: String,
    pub rules: FailurePhaseRules,
    pub episodes: usize,
    pub failures: usize,
    pub by_phase: PhaseCounts,
    pub seeds: PhaseSeeds,
}

/// Summarize `(seed, phase)` rows (draw order; `None` = success).
pub fn summarize_failure_phases(
    rows: &[(u64, Option<FailurePhase>)],
    rules: FailurePhaseRules,
) -> FailurePhaseSummary {
    let mut by_phase = PhaseCounts::default();
    let mut seeds = PhaseSeeds::default();
    for (seed, phase) in rows {
        let Some(p) = phase else { continue };
        let (n, v) = match p {
            FailurePhase::Reach => (&mut by_phase.reach, &mut seeds.reach),
            FailurePhase::Grasp => (&mut by_phase.grasp, &mut seeds.grasp),
            FailurePhase::Lift => (&mut by_phase.lift, &mut seeds.lift),
            FailurePhase::Place => (&mut by_phase.place, &mut seeds.place),
        };
        *n += 1;
        v.push(*seed);
    }
    FailurePhaseSummary {
        scheme: FAILURE_PHASE_SCHEME_V1.into(),
        rules,
        episodes: rows.len(),
        failures: rows.iter().filter(|(_, p)| p.is_some()).count(),
        by_phase,
        seeds,
    }
}

/// The M2 per-preset honest baseline gate (PLAN §四 M2): the threshold
/// a scripted/numeric baseline must clear on a preset, keyed by task id
/// and robot name. New first-version tasks get honest floors (75% /
/// 60%) rather than a one-size 90%; the golden tabletop preset keeps
/// 90%.
///
/// `None` = no gate (record only).
pub fn baseline_gate(task_id: &str, robot_name: &str) -> Option<(f64, &'static str)> {
    let base = task_id.split('@').next().unwrap_or(task_id);
    match base {
        "reach" => Some((
            0.90,
            "reach 是课程起点，脚本/数值基线都应打满（course@v1 同款硬闸）",
        )),
        "pick_place" => {
            if robot_name.contains("tabletop-4dof") {
                Some((
                    0.90,
                    "金标桌面预设：91.7% 锚不许退（走原脚本或 DLS 均需过）",
                ))
            } else {
                Some((0.75, "新臂首版诚实门槛（Panda 7-DoF 数值 IK 基线）"))
            }
        }
        "stack" => Some((0.75, "stack 新任务首版诚实门槛")),
        "bin_pick" => Some((0.60, "bin_pick 新任务首版诚实门槛（料箱 prop 近似）")),
        "drawer" => Some((
            0.60,
            "drawer 新任务首版诚实门槛（真 prismatic 滑轨面板，v2 预设）",
        )),
        "push" => None,
        _ => None,
    }
}

/// The three fingerprint views of one episode set (M0b receipt block):
/// the pure world-state hash, the legacy v1 fold (anchor continuity)
/// and the complete-identity v2 fold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintSet {
    /// `"v2"` (the scheme of `episode_state_hashes` below).
    pub fingerprint_scheme: String,
    /// Pure world-state hash per episode (state axis only — the
    /// pre-M0b receipts folded identity in; recover it with
    /// `oriera_subsys::rigid3d::state_hash_from_v1`).
    pub episode_state_hashes: Vec<u64>,
    /// Legacy scheme-`v1` values, byte-equal to pre-M0b receipts on an
    /// unchanged default path (anchor continuity for old comparisons).
    pub episode_fingerprints_v1: Vec<u64>,
    /// Scheme-`v2` complete-identity values
    /// (state ⊕ robot identity ⊕ spec_hash).
    pub episode_fingerprints_v2: Vec<u64>,
}

/// Derive the fingerprint block from v1 values + the robot that ran.
pub fn fingerprint_set(
    robot: &oriera_subsys::RobotSpec,
    spec_hash: u64,
    v1: &[u64],
) -> FingerprintSet {
    use oriera_subsys::rigid3d::{
        fingerprint_v2_from_parts, robot_identity_hash, state_hash_from_v1,
    };
    let (kp, kd) = robot.joints.first().map_or((0.0, 0.0), |j| (j.kp, j.kd));
    let identity = robot_identity_hash(robot);
    let states: Vec<u64> = v1
        .iter()
        .map(|fp| state_hash_from_v1(*fp, kp, kd, robot.mass_scale))
        .collect();
    let v2: Vec<u64> = states
        .iter()
        .map(|s| fingerprint_v2_from_parts(*s, identity, Some(spec_hash)))
        .collect();
    FingerprintSet {
        fingerprint_scheme: oriera_subsys::rigid3d::FINGERPRINT_SCHEME_V2.into(),
        episode_state_hashes: states,
        episode_fingerprints_v1: v1.to_vec(),
        episode_fingerprints_v2: v2,
    }
}

/// The absolute golden fingerprint file (P-10): what
/// `--fingerprints contracts/golden/embodied_fingerprints.v2.json`
/// writes. Two runs of the same command on the same tree produce
/// byte-identical JSON (no wall-clock fields — provenance is
/// `engine_commit`, which HEAD provides).
#[derive(Debug, Clone, Serialize)]
pub struct FingerprintGolden {
    /// Anchor family tag (`oriera-embodied-v2`), same convention as the
    /// pre-M0a anchor.
    pub scheme: String,
    pub purpose: String,
    /// Engine git SHA at generation time (`unknown` outside a repo).
    pub engine_commit: String,
    pub features: String,
    pub backend: String,
    pub dt: f32,
    /// The exact regeneration command (`<FILE>` placeholder for the
    /// output path, so the file quotes itself without self-referencing).
    pub command: String,
    pub spec_hash: String,
    pub protocol_id: String,
    pub protocol_parameter_hash: String,
    /// The v2 robot identity fold (`robot_identity_hash`).
    pub identity_hash: u64,
    pub seeds: Vec<u64>,
    pub successes: usize,
    pub success_rate: f64,
    pub transitions: u64,
    /// The three per-episode views (see [`FingerprintSet`]).
    pub fingerprints: FingerprintSet,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seed: u64, success: bool) -> EpisodeOutcome {
        EpisodeOutcome::Done(EpisodeRow {
            seed,
            success,
            steps: 100,
            reward_sum: 1.0,
            fingerprint_v1: seed,
            rand: "{}".into(),
        })
    }

    #[test]
    fn course_is_the_legacy_band_and_budget() {
        let p = EvalProtocol::course(None, 1000);
        assert_eq!(p.episodes, 12);
        assert_eq!(p.gate, Gate::Hard90);
        // The exact pre-M0b seed list: 1000, 1001, …, redraws land in
        // the displaced 65536 band.
        assert_eq!(p.episode_seed(1, 0), 1000);
        assert_eq!(p.episode_seed(12, 0), 1011);
        assert_eq!(p.episode_seed(1, 1), 66536);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn paper_requires_the_floor_and_never_gates() {
        let err = EvalProtocol::paper(Some(99), 0).unwrap_err();
        assert!(err.contains("100"), "{err}");
        let p = EvalProtocol::paper(Some(100), 0).unwrap();
        assert_eq!(p.gate, Gate::Report);
        assert!(
            !p.gate_applies("pick_place@v1"),
            "paper records, never gates"
        );
        let err = p.clone().with_gate(Some(Gate::Hard90)).unwrap_err();
        assert!(err.contains("paper@v1") && err.contains("hard"), "{err}");
        assert!(p.with_gate(Some(Gate::Report)).is_ok());
    }

    #[test]
    fn parse_names_the_unknown_protocol() {
        let err = EvalProtocol::parse("final@v9", None, 0).unwrap_err();
        assert!(
            err.contains("final@v9") && err.contains("course@v1"),
            "{err}"
        );
        assert_eq!(
            EvalProtocol::parse("course@v1", None, 5).unwrap().episodes,
            12
        );
    }

    #[test]
    fn protocol_hash_moves_with_every_knob() {
        let a = EvalProtocol::course(None, 1000);
        let b = EvalProtocol::course(Some(12), 1000);
        assert_eq!(
            a.protocol_hash_hex(),
            b.protocol_hash_hex(),
            "same knobs, same hash"
        );
        let mut c = a.clone();
        c.episodes = 16;
        assert_ne!(a.protocol_hash(), c.protocol_hash());
        let mut d = a.clone();
        d.seed0 = 1001;
        assert_ne!(a.protocol_hash(), d.protocol_hash());
        let mut e = a.clone();
        e.gate = Gate::Report;
        assert_ne!(a.protocol_hash(), e.protocol_hash());
        assert_eq!(a.protocol_hash(), a.protocol_hash());
    }

    #[test]
    fn evaluate_redraws_out_of_budget_and_keeps_order() {
        let p = EvalProtocol::course(Some(4), 1000);
        let mut degenerate_first = true;
        let r = evaluate(&p, |seed, _draw| {
            if degenerate_first && seed == 1002 {
                degenerate_first = false;
                return Ok(EpisodeOutcome::Degenerate);
            }
            Ok(row(seed, true))
        })
        .unwrap();
        assert_eq!(r.rows.len(), 4);
        assert_eq!(r.redraws, 1);
        assert_eq!(r.draws, 5);
        // The legacy band: once a redraw happens, later draws shift into
        // the displaced 65536 band — exactly how the pre-M0a anchor's
        // seed list [1000, 66538, 66539, …] came about.
        assert_eq!(
            r.rows.iter().map(|x| x.seed).collect::<Vec<_>>(),
            [1000, 1001, 66539, 66540],
            "the degenerate seed leaves the budget, the band stays legacy"
        );
        assert_eq!(r.success_rate(), 1.0);
    }

    #[test]
    fn gate_verdicts_per_protocol_and_task() {
        let course = EvalProtocol::course(Some(10), 0);
        assert_eq!(
            course.gate_verdict("pick_place@v1", 0.5),
            GateVerdict::Fail {
                threshold: 0.9,
                rate: 0.5
            }
        );
        assert_eq!(course.gate_verdict("push@v1", 0.0), GateVerdict::NotApplied);
        assert_eq!(course.gate_verdict("reach@v1", 0.95), GateVerdict::Pass);
        let paper = EvalProtocol::paper(None, 0).unwrap();
        assert_eq!(
            paper.gate_verdict("pick_place@v1", 0.1),
            GateVerdict::NotApplied
        );
    }

    #[test]
    fn wilson_interval_bounds() {
        let (lo, hi) = wilson95(11, 12);
        assert!(
            (0.64..0.66).contains(&lo) && (0.98..=1.0).contains(&hi),
            "{lo} {hi}"
        );
        assert_eq!(wilson95(0, 0), (0.0, 1.0));
    }

    // --- M2: success_criteria v2 + the dual success view -----

    #[test]
    fn success_criteria_v2_is_accepted_and_hashed() {
        let p = EvalProtocol::course(None, 1000)
            .with_success_criteria(SUCCESS_CRITERIA_V2)
            .unwrap();
        assert_eq!(p.success_criteria, "v2");
        p.validate().expect("v2 validates");
        let v1 = EvalProtocol::course(None, 1000);
        assert_ne!(
            p.protocol_hash(),
            v1.protocol_hash(),
            "criteria are identity"
        );
        let err = EvalProtocol::course(None, 1000)
            .with_success_criteria("v3")
            .unwrap_err();
        assert!(err.contains("v1"), "{err}");
    }

    #[test]
    fn dual_success_summary_reports_both_views() {
        let views = [
            SuccessView {
                once: true,
                at_end: true,
            },
            SuccessView {
                once: true,
                at_end: false,
            },
            SuccessView::default(),
        ];
        let s = summarize_success(&views);
        assert_eq!(
            (s.episodes, s.successes_once, s.successes_at_end),
            (3, 2, 1)
        );
        assert!((s.success_once_rate - 2.0 / 3.0).abs() < 1e-12);
        assert!((s.success_at_end_rate - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(s.once_wilson95.0, wilson95(2, 3).0);
    }

    #[test]
    fn legacy_view_collapses_like_v1() {
        let v = SuccessView::legacy(true);
        assert!(v.once && v.at_end);
        let v = SuccessView::legacy(false);
        assert!(!v.once && !v.at_end);
    }

    // --- P2: failure-phase telemetry (failure_phase@v1) -------------

    /// Fold a scripted `(tcp, object, holding)` path into signals.
    fn sig_path(
        obj0: [f32; 3],
        goal: [f32; 3],
        path: &[([f32; 3], [f32; 3], bool)],
    ) -> PhaseSignals {
        let mut s = PhaseSignals::start(obj0);
        for (ee, obj, hold) in path {
            s.observe(*ee, *obj, goal, *hold);
        }
        s
    }

    #[test]
    fn failure_phase_rules_name_the_first_missed_milestone() {
        let params = crate::embodied_env::TaskParams::default();
        let rules = FailurePhaseRules::for_task("pick_place@v1", &params);
        assert_eq!(rules.family, PhaseFamily::Pick);
        assert_eq!(rules.reach_radius, oriera_subsys::GRASP_RADIUS);
        assert_eq!(rules.deliver_xz, params.goal_xz_tol);
        let goal = [0.28, 0.5, 0.0];
        let o = [0.18, 0.525, 0.0];
        // reach: the TCP never came within the 6 cm grasp zone
        let s = sig_path(o, goal, &[([0.18, 0.70, 0.0], o, false)]);
        assert_eq!(
            classify_failure(false, &s, &rules),
            Some(FailurePhase::Reach)
        );
        // grasp: reached (3 cm), never held
        let s = sig_path(o, goal, &[([0.18, 0.555, 0.0], o, false)]);
        assert_eq!(
            classify_failure(false, &s, &rules),
            Some(FailurePhase::Grasp)
        );
        // lift: held, rose 1 cm, slipped out before the goal
        let s = sig_path(
            o,
            goal,
            &[
                ([0.18, 0.555, 0.0], o, true),
                ([0.18, 0.565, 0.0], [0.18, 0.535, 0.0], true),
                ([0.18, 0.62, 0.0], o, false),
            ],
        );
        assert_eq!(
            classify_failure(false, &s, &rules),
            Some(FailurePhase::Lift)
        );
        assert!((s.max_lift_held - 0.01).abs() < 1e-6, "{}", s.max_lift_held);
        assert_eq!(
            (s.ever_holding, s.holding_ticks, s.holding_at_end),
            (true, 2, false)
        );
        // place: carried over the goal under grip, released off-zone
        let s = sig_path(
            o,
            goal,
            &[
                ([0.18, 0.555, 0.0], o, true),
                ([0.27, 0.70, 0.0], [0.27, 0.67, 0.0], true),
                ([0.30, 0.60, 0.1], [0.40, 0.525, 0.12], false),
            ],
        );
        assert_eq!(
            classify_failure(false, &s, &rules),
            Some(FailurePhase::Place)
        );
        assert!(s.final_goal_xz > params.goal_xz_tol);
        // a success is never a failure, whatever the signals
        assert_eq!(classify_failure(true, &s, &rules), None);
    }

    #[test]
    fn failure_phase_families_follow_the_task() {
        let p = crate::embodied_env::TaskParams::default();
        let o = [0.18, 0.525, 0.0];
        let goal = [0.28, 0.5, 0.0];
        let near_no_hold = sig_path(o, goal, &[([0.18, 0.555, 0.0], o, false)]);
        let far = sig_path(o, goal, &[([0.18, 0.70, 0.0], o, false)]);
        // reach tasks only have the reach phase
        let r = FailurePhaseRules::for_task("reach@v2", &p);
        assert_eq!(r.family, PhaseFamily::Reach);
        assert_eq!(
            classify_failure(false, &near_no_hold, &r),
            Some(FailurePhase::Reach)
        );
        // push has no grasp: reached-but-failed is a placement failure
        let r = FailurePhaseRules::for_task("push@v1", &p);
        assert_eq!(
            classify_failure(false, &near_no_hold, &r),
            Some(FailurePhase::Place)
        );
        assert_eq!(classify_failure(false, &far, &r), Some(FailurePhase::Reach));
        // stack / drawer deliver at their own tolerances
        assert_eq!(
            FailurePhaseRules::for_task("stack@v1", &p).deliver_xz,
            p.stack_xz_tol
        );
        let d = FailurePhaseRules::for_task("drawer@v1", &p);
        assert_eq!(
            (d.family, d.deliver_xz),
            (PhaseFamily::Pick, p.pull_distance * 0.25)
        );
        assert!(
            d.deliver_xz < p.pull_distance,
            "the drawer spawn ({} m from its goal) must not count as delivered",
            p.pull_distance
        );
    }

    #[test]
    fn failure_phase_summary_counts_seeds_in_pipeline_order() {
        let rules = FailurePhaseRules::for_task(
            "pick_place@v1",
            &crate::embodied_env::TaskParams::default(),
        );
        let rows = [
            (1000, None),
            (1001, Some(FailurePhase::Lift)),
            (1002, Some(FailurePhase::Lift)),
            (1003, Some(FailurePhase::Grasp)),
        ];
        let s = summarize_failure_phases(&rows, rules);
        assert_eq!(s.scheme, FAILURE_PHASE_SCHEME_V1);
        assert_eq!((s.episodes, s.failures), (4, 3));
        assert_eq!(
            s.by_phase,
            PhaseCounts {
                reach: 0,
                grasp: 1,
                lift: 2,
                place: 0
            }
        );
        assert_eq!(s.seeds.lift, [1001, 1002]);
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains(r#""by_phase":{"reach":0,"grasp":1,"lift":2,"place":0}"#),
            "pipeline order, all four keys: {json}"
        );
        assert_eq!(
            FailurePhase::ALL.map(FailurePhase::as_str),
            ["reach", "grasp", "lift", "place"]
        );
    }

    #[test]
    fn baseline_gates_are_per_preset_and_honest() {
        // The golden tabletop preset keeps the 90% anchor…
        let (t, _) = baseline_gate("pick_place@v1", "tabletop-4dof").expect("tabletop gate");
        assert_eq!(t, 0.90);
        // …other robots get the honest 75% first-version floor…
        let (t, _) = baseline_gate("pick_place@v1", "panda7").expect("panda gate");
        assert_eq!(t, 0.75);
        // …and the new families their own floors; push stays ungated.
        for (task, want) in [
            ("stack@v1", 0.75),
            ("bin_pick@v1", 0.60),
            ("drawer@v1", 0.60),
        ] {
            let (t, _) = baseline_gate(task, "tabletop-4dof").expect(task);
            assert_eq!(t, want, "{task}");
        }
        assert!(baseline_gate("push@v1", "tabletop-4dof").is_none());
        assert!(baseline_gate("fly@v9", "tabletop-4dof").is_none());
    }

    #[test]
    fn fingerprint_set_is_deterministic_and_layered() {
        use oriera_subsys::{rigid3d::robot_identity_hash, ArmDescription, RobotSpec};
        let robot = RobotSpec::from(&ArmDescription::default());
        let v1 = [1u64, 2, 3];
        let set = fingerprint_set(&robot, 0xabc, &v1);
        assert_eq!(set.episode_fingerprints_v1, v1);
        assert_eq!(set.fingerprint_scheme, "v2");
        // v2 = state ⊕ identity ⊕ spec, exactly as advertised
        for (i, fp) in set.episode_fingerprints_v2.iter().enumerate() {
            let expected = oriera_subsys::rigid3d::fingerprint_v2_from_parts(
                set.episode_state_hashes[i],
                robot_identity_hash(&robot),
                Some(0xabc),
            );
            assert_eq!(*fp, expected);
        }
        // identity change ⇒ same state, different v2
        let mut robot2 = robot.clone();
        robot2.joints[1].kp += 1.0;
        let set2 = fingerprint_set(&robot2, 0xabc, &v1);
        assert_eq!(set2.episode_state_hashes, set.episode_state_hashes);
        assert_ne!(set2.episode_fingerprints_v2, set.episode_fingerprints_v2);
    }

    /// The absolute golden (P-10) is self-consistent and continuous
    /// with the pre-M0a anchor, checked WITHOUT running physics:
    /// `contracts/golden/embodied_fingerprints.v2.json`'s v1 values are
    /// the pre-M0a anchor's `episode_state_hashes` (same 12 seeds, same
    /// default experiment), every recorded pure-state hash unfolds from
    /// its v1 value, and every v2 value is `parts(state, identity(preset),
    /// spec_hash(golden))`. A physics change that moves the trajectory
    /// fails the live collect-vs-golden comparison; a fold change fails
    /// here — either way the `fingerprint_scheme` must be bumped.
    #[test]
    fn v2_golden_is_self_consistent_and_continuous_with_the_anchor() {
        use crate::spec::{SceneSpec, GOLDEN_SPEC_HASH_HEX, GOLDEN_TABLETOP_4DOF_PICK_SPEC};
        use oriera_subsys::rigid3d::{
            fingerprint_v2_from_parts, robot_identity_hash, state_hash_from_v1,
        };
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contracts/golden/");
        let v2: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{root}embodied_fingerprints.v2.json"))
                .expect("read v2 golden"),
        )
        .unwrap();
        let anchor: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{root}embodied_fingerprints.pre-m0a.json"))
                .expect("read pre-m0a anchor"),
        )
        .unwrap();
        let u64s = |v: &serde_json::Value| -> Vec<u64> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap())
                .collect()
        };
        let fps = &v2["fingerprints"];
        assert_eq!(fps["fingerprint_scheme"], "v2");
        assert_eq!(v2["spec_hash"], GOLDEN_SPEC_HASH_HEX);
        assert_eq!(v2["protocol_id"], PROTOCOL_COURSE_V1);
        // continuity: same seeds, same v1 numbers as the pre-M0a anchor
        assert_eq!(u64s(&v2["seeds"]), u64s(&anchor["seeds"]));
        assert_eq!(
            u64s(&fps["episode_fingerprints_v1"]),
            u64s(&anchor["episode_state_hashes"]),
            "v2 golden's v1 view must equal the pre-M0a anchor"
        );
        assert_eq!(v2["successes"], anchor["successes"]);
        assert_eq!(v2["transitions"], anchor["transitions"]);
        // self-consistency: every view derives from the v1 view + the spec
        let spec = SceneSpec::from_json_v2(GOLDEN_TABLETOP_4DOF_PICK_SPEC).unwrap();
        let r = &spec.robot;
        let identity = robot_identity_hash(r);
        assert_eq!(v2["identity_hash"].as_u64().unwrap(), identity);
        let states = u64s(&fps["episode_state_hashes"]);
        let v1 = u64s(&fps["episode_fingerprints_v1"]);
        let v2s = u64s(&fps["episode_fingerprints_v2"]);
        assert_eq!((states.len(), v1.len(), v2s.len()), (12, 12, 12));
        for i in 0..12 {
            assert_eq!(
                state_hash_from_v1(v1[i], r.joints[0].kp, r.joints[0].kd, r.mass_scale),
                states[i],
                "episode {i}: state hash must unfold from v1"
            );
            assert_eq!(
                fingerprint_v2_from_parts(states[i], identity, Some(spec.spec_hash())),
                v2s[i],
                "episode {i}: v2 must be parts(state, identity, spec_hash)"
            );
        }
        // the protocol hash recorded is course@v1 at 12 episodes / seed0 1000
        let p = EvalProtocol::course(Some(12), 1000);
        assert_eq!(v2["protocol_parameter_hash"], p.protocol_hash_hex());
    }
}
