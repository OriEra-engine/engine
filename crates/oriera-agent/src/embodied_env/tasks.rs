//! The task layer (PLAN §3.1, review P-2): the versioned [`Task`]
//! trait and its nine implementations behind the registry, every
//! threshold in [`TaskParams`] (spec → hash), the [`TaskKind`]
//! pre-M0a alias layer, and the legacy threshold constants (now the
//! [`TaskParams`] defaults).

use super::env::ObsSlice;
use super::scene::{Lcg, Placement, SceneGraph};
use oriera_subsys::rigid3d::PropShape;

// ---------------------------------------------------------------------------
// TaskParams + StepState + Task trait + registry (PLAN §3.1, review P-2:
// thresholds leave the compile-time-constant world and enter the spec →
// the experiment hash).
// ---------------------------------------------------------------------------

/// Every task threshold and placement range, spec-addressable and
/// hashed. Defaults are exactly the pre-M0a compile-time constants
/// ([`GOAL_XZ_TOL`] etc.) — pinning this in tests.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(default, deny_unknown_fields))]
pub struct TaskParams {
    /// Success zone XZ tolerance (pick_place / push); the reach radius
    /// shares the tolerance semantic family.
    pub goal_xz_tol: f32,
    /// Success zone height band (object centre, m).
    pub goal_y_min: f32,
    pub goal_y_max: f32,
    /// Reach termination radius (tip-to-object distance).
    pub reach_radius: f32,
    /// Minimum spawn separation (XZ, m) between the object and the goal
    /// — placement zones are adjacent, a naive draw can land the object
    /// inside the goal zone (seeds 1009/1010 terminated on step 1).
    pub min_separation: f32,
    /// Separation re-draw budget before giving up.
    pub max_redraws: u32,
    /// Where the goal zone spawns (the goal IS the object for reach).
    pub goal: Placement,

    // ----- M2 (@v2 判据 + 新任务族) 字段 ---------------------------------
    // 这些字段在 `SceneSpec::spec_hash` 里**取默认值时被规范化掉**
    // （同夹爪 approach_axis/gap_axis 的先例）：v1 金标 spec 的 hash
    // `c90869dcf6a04875` 一位不动；写了非默认值才进身份。
    /// 首次满足判据即终止回合（旧语义；`@v2` 协议参数位：false ⇒ 跑满
    /// `max_steps`，由 `success_once` / `success_at_end` 双报）。
    pub terminate_on_success: bool,
    /// `@v2` 静止判据：物体速度连续低于 `rest_speed` 的步数门槛（k）。
    pub rest_steps: u32,
    /// `@v2` 静止判据：物体质心速度阈值（m/s）。
    pub rest_speed: f32,
    /// `@v2` 松爪判据：夹爪指令低于此值视为已松爪（0 开 … 1 合）。
    pub release_below: f32,
    /// `stack@v1`：A 相对 B 的 XZ 对齐容差（m）。
    pub stack_xz_tol: f32,
    /// `stack@v1`：A 底面与 B 顶面高度差容差（m）。
    pub stack_y_tol: f32,
    /// `drawer@v1`：拉出方向（世界系，水平向量，归一化后使用）。
    pub pull_axis: [f32; 3],
    /// `drawer@v1`：沿 `pull_axis` 的拉出距离目标（m）。
    pub pull_distance: f32,
}

/// Legacy `@v2` defaults (also the values [`TaskParams::canonicalize_json`]
/// strips from the identity hash).
pub const TERMINATE_ON_SUCCESS_DEFAULT: bool = true;
pub const REST_STEPS_DEFAULT: u32 = 10;
pub const REST_SPEED_DEFAULT: f32 = 0.01;
pub const RELEASE_BELOW_DEFAULT: f32 = 0.5;
pub const STACK_XZ_TOL_DEFAULT: f32 = 0.03;
pub const STACK_Y_TOL_DEFAULT: f32 = 0.02;
pub const PULL_AXIS_DEFAULT: [f32; 3] = [-1.0, 0.0, 0.0];
pub const PULL_DISTANCE_DEFAULT: f32 = 0.08;

/// The M2 `task.params` keys that are hash-canonicalized at their
/// defaults (see [`TaskParams::canonicalize_json`]).
pub const TASK_PARAMS_M2_KEYS: [&str; 8] = [
    "terminate_on_success",
    "rest_steps",
    "rest_speed",
    "release_below",
    "stack_xz_tol",
    "stack_y_tol",
    "pull_axis",
    "pull_distance",
];

impl Default for TaskParams {
    fn default() -> Self {
        Self {
            goal_xz_tol: GOAL_XZ_TOL,
            goal_y_min: GOAL_Y_MIN,
            goal_y_max: GOAL_Y_MAX,
            reach_radius: REACH_RADIUS,
            min_separation: 0.12,
            max_redraws: 8,
            goal: Placement::Range {
                x: [0.24, 0.31],
                y: 0.5,
                z: [-0.08, 0.08],
            },
            terminate_on_success: TERMINATE_ON_SUCCESS_DEFAULT,
            rest_steps: REST_STEPS_DEFAULT,
            rest_speed: REST_SPEED_DEFAULT,
            release_below: RELEASE_BELOW_DEFAULT,
            stack_xz_tol: STACK_XZ_TOL_DEFAULT,
            stack_y_tol: STACK_Y_TOL_DEFAULT,
            pull_axis: PULL_AXIS_DEFAULT,
            pull_distance: PULL_DISTANCE_DEFAULT,
        }
    }
}

impl TaskParams {
    /// Identity canonicalization (M2): drop every M2 key from a
    /// serialized `task.params` object when it equals its default — a
    /// v1 spec that never heard of `rest_steps` and a v2 spec that writes
    /// `rest_steps: 10` are the same experiment, so they hash the same
    /// (the gripper plate-axis precedent). Non-default values stay and
    /// move the hash.
    #[cfg(feature = "json")]
    pub fn canonicalize_json(params: &mut serde_json::Value) {
        let Some(obj) = params.as_object_mut() else {
            return;
        };
        let defaults = serde_json::to_value(Self::default()).expect("TaskParams serializes");
        for key in TASK_PARAMS_M2_KEYS {
            if obj.get(key) == defaults.get(key) {
                obj.remove(key);
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let ok = |v: f32| v.is_finite() && v > 0.0;
        if !ok(self.goal_xz_tol) || !ok(self.reach_radius) || !ok(self.min_separation) {
            return Err(
                "task.params: goal_xz_tol / reach_radius / min_separation 须为有限正数".into(),
            );
        }
        if self.rest_steps == 0 {
            return Err("task.params.rest_steps = 0：@v2 静止判据至少要 1 步（默认 10）".into());
        }
        if !ok(self.rest_speed) {
            return Err(format!(
                "task.params.rest_speed = {}：须为有限正数（m/s，默认 0.01）",
                self.rest_speed
            ));
        }
        if !(self.release_below.is_finite()
            && self.release_below > 0.0
            && self.release_below <= 1.0)
        {
            return Err(format!(
                "task.params.release_below = {}：须在 (0, 1]（夹爪指令低于它视为松爪，默认 0.5）",
                self.release_below
            ));
        }
        if !ok(self.stack_xz_tol) || !ok(self.stack_y_tol) {
            return Err("task.params: stack_xz_tol / stack_y_tol 须为有限正数".into());
        }
        let horiz =
            (self.pull_axis[0] * self.pull_axis[0] + self.pull_axis[2] * self.pull_axis[2]).sqrt();
        if !(self.pull_axis.iter().all(|v| v.is_finite()) && horiz > 1e-6) {
            return Err(format!(
                "task.params.pull_axis = {:?}：须为有限、水平分量非零的向量（默认 [-1,0,0] = 朝臂基座拉）",
                self.pull_axis
            ));
        }
        if !ok(self.pull_distance) {
            return Err(format!(
                "task.params.pull_distance = {}：须为有限正数（m，默认 0.08）",
                self.pull_distance
            ));
        }
        if !(self.goal_y_min.is_finite()
            && self.goal_y_max.is_finite()
            && self.goal_y_min < self.goal_y_max)
        {
            return Err(format!(
                "task.params: goal_y_min ({}) 须 < goal_y_max ({})",
                self.goal_y_min, self.goal_y_max
            ));
        }
        if let Placement::Range { x, z, .. } = self.goal {
            if x[0] > x[1] || z[0] > z[1] {
                return Err(format!(
                    "task.params.goal.placement: 区间须 lo ≤ hi（x = {:?}, z = {:?}）",
                    x, z
                ));
            }
        }
        Ok(())
    }
}

/// Everything a task's reward/success/obs_extra reads after a step.
/// Distances are precomputed once (the same expressions as pre-M0a).
/// The M2 fields (`gripper` … `obj_start`) feed the `@v2` / new-task
/// criteria only — no v1 task reads them, so v1 bytes never move.
#[derive(Debug, Clone, Copy, Default)]
pub struct StepState {
    /// End-effector (TCP) position.
    pub ee: [f32; 3],
    /// Task-object position.
    pub obj_pos: [f32; 3],
    /// Goal position (world frame).
    pub goal: [f32; 3],
    /// Gripper is geometrically holding the object (this step).
    pub holding: bool,
    /// `holding` of the previous step (grasp-event edge detector).
    pub was_holding: bool,
    /// |tip − object| Euclidean.
    pub d_ee_obj: f32,
    /// |object − goal| Euclidean (3D, as pre-M0a).
    pub d_obj_goal: f32,
    /// Gripper demand this step (0 open … 1 closed) — the `@v2`
    /// release check reads the *command*, not the hold heuristic.
    pub gripper: f32,
    /// Consecutive steps the task object's centre speed stayed below
    /// `rest_speed` (reset to 0 the moment it moves).
    pub still_steps: u32,
    /// `holding` was true on ANY step of this episode (push@v2 forbids).
    pub ever_holding: bool,
    /// Position of `scene.props[1]` (the stack base) — zero without one.
    pub prop1_pos: [f32; 3],
    /// Where the task object spawned this episode (drawer displacement
    /// origin; horizontal components are the rest position too).
    pub obj_start: [f32; 3],
}

/// Episode placement result of [`Task::reset`]: one position per scene
/// prop (index-aligned with `SceneGraph::props`) and the goal.
#[derive(Debug, Clone, PartialEq)]
pub struct Placements {
    pub props: Vec<[f32; 3]>,
    pub goal: [f32; 3],
}

/// A task = reset draws + reward + success + the privileged obs block.
/// Ids are versioned (`pick_place@v1`): criteria changes ship as `@v2`
/// additions, never in-place edits (review P-2).
pub trait Task {
    /// Registry id, e.g. `"pick_place@v1"`.
    fn id(&self) -> &'static str;
    /// Natural-language instruction (language-conditioned IL).
    fn phrase(&self) -> &'static str;
    /// Gym env name (the [`EnvSpec::name`] source).
    fn env_name(&self) -> &'static str;
    /// The [`TaskKind`] alias this task answers to (the pre-M0a enum).
    fn kind(&self) -> TaskKind;

    /// Phase 1 draws: prop placements from `scene` and the provisional
    /// goal. Draw order (object x, object z, goal x, goal z) is part of
    /// the episode receipt — do not reorder without a
    /// `fingerprint_scheme` bump.
    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements;

    /// Phase 2 (the env's own DR draws happen between the phases):
    /// enforce task-level placement constraints — e.g. re-draw the goal
    /// until it clears the object by `min_separation`.
    fn finalize(&mut self, rng: &mut Lcg, scene: &SceneGraph, placements: &mut Placements);

    /// Dense shaping + terminal bonus (the whole per-step reward).
    fn reward(&self, s: &StepState) -> f32;
    /// Termination criterion (`terminated == success` in this family).
    fn success(&self, s: &StepState) -> bool;
    /// The privileged per-step observation block (appended after the
    /// proprioceptive prefix; 9 floats for every M0a task — same order
    /// as pre-M0a).
    fn obs_extra(&self, s: &StepState) -> Vec<f32>;
    /// Names/grouping of [`Task::obs_extra`] (metadata only).
    fn obs_extra_layout(&self) -> Vec<ObsSlice>;
    /// Current goal (introspection).
    fn goal(&self) -> [f32; 3];
    /// Success-semantics revision this task's criterion belongs to —
    /// `"v1"` (zone-only) or `"v2"` (released + resting / no-hold);
    /// the eval protocol records it (`EvalProtocol::success_criteria`).
    fn success_criteria(&self) -> &'static str {
        "v1"
    }
}

/// reach@v1 — touch the object with the tip: curriculum start.
struct ReachTask {
    params: TaskParams,
    goal: [f32; 3],
}

impl ReachTask {
    fn new(params: TaskParams) -> Self {
        Self {
            params,
            goal: [0.0; 3],
        }
    }
}

impl Task for ReachTask {
    fn id(&self) -> &'static str {
        "reach@v1"
    }
    fn phrase(&self) -> &'static str {
        "reach the cube with the gripper"
    }
    fn env_name(&self) -> &'static str {
        "Reach-OrieraArm-v0"
    }
    fn kind(&self) -> TaskKind {
        TaskKind::Reach
    }

    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements {
        // Reach: the goal IS the object — `goal_rel` stays a real
        // channel (a zeroed goal used to leak −ee noise into obs,
        // caught in the ten-role review). Zero extra draws.
        let props: Vec<[f32; 3]> = scene
            .props
            .iter()
            .map(|p| p.placement.sample(rng))
            .collect();
        let goal = props[0];
        Placements { props, goal }
    }

    fn finalize(&mut self, _rng: &mut Lcg, _scene: &SceneGraph, _placements: &mut Placements) {
        // goal == object by construction; no separation to enforce.
    }

    fn reward(&self, s: &StepState) -> f32 {
        let mut reward = -0.002;
        reward -= s.d_ee_obj;
        if self.success(s) {
            reward += 5.0;
        }
        reward
    }

    fn success(&self, s: &StepState) -> bool {
        s.d_ee_obj < self.params.reach_radius
    }

    fn obs_extra(&self, s: &StepState) -> Vec<f32> {
        let rel = |p: [f32; 3], q: [f32; 3]| [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
        let mut v: Vec<f32> = rel(s.obj_pos, s.ee).to_vec();
        v.extend_from_slice(&rel(s.goal, s.ee));
        v.push(s.holding as i32 as f32);
        v.push(s.obj_pos[1]);
        v.push(s.d_ee_obj);
        v
    }

    fn obs_extra_layout(&self) -> Vec<ObsSlice> {
        default_extra_layout()
    }

    fn goal(&self) -> [f32; 3] {
        self.goal
    }
}

/// pick_place@v1 — grasp the object and place it in the goal zone
/// (flagship). Grasp bonus on the hold edge; height-band success.
pub(crate) struct PickPlaceTask {
    params: TaskParams,
    goal: [f32; 3],
}

impl PickPlaceTask {
    pub(crate) fn new(params: TaskParams) -> Self {
        Self {
            params,
            goal: [0.0; 3],
        }
    }
}

impl Task for PickPlaceTask {
    fn id(&self) -> &'static str {
        "pick_place@v1"
    }
    fn phrase(&self) -> &'static str {
        "pick up the cube and place it at the target"
    }
    fn env_name(&self) -> &'static str {
        "PickPlace3D-OrieraArm-v0"
    }
    fn kind(&self) -> TaskKind {
        TaskKind::PickPlace
    }

    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements {
        let props: Vec<[f32; 3]> = scene
            .props
            .iter()
            .map(|p| p.placement.sample(rng))
            .collect();
        let goal = self.params.goal.sample(rng);
        Placements { props, goal }
    }

    fn finalize(&mut self, rng: &mut Lcg, _scene: &SceneGraph, placements: &mut Placements) {
        // Minimum spawn separation: re-draw the goal until it is at
        // least `min_separation` away in XZ (bounded by `max_redraws`).
        let obj = placements.props[0];
        let mut attempts = 0;
        loop {
            let d = ((obj[0] - placements.goal[0]).powi(2) + (obj[2] - placements.goal[2]).powi(2))
                .sqrt();
            if d >= self.params.min_separation || attempts >= self.params.max_redraws {
                break;
            }
            placements.goal = self.params.goal.sample(rng);
            attempts += 1;
        }
        self.goal = placements.goal;
    }

    fn reward(&self, s: &StepState) -> f32 {
        let mut reward = -0.002;
        let grasp_event = s.holding && !s.was_holding;
        if s.holding {
            reward +=
                -1.2 * s.d_obj_goal + 2.0 * (s.obj_pos[1] - self.params.goal_y_min).clamp(0.0, 0.2);
        } else {
            reward -= 0.6 * s.d_ee_obj;
        }
        reward += if grasp_event { 2.0 } else { 0.0 };
        if self.success(s) {
            reward += 10.0;
        }
        reward
    }

    fn success(&self, s: &StepState) -> bool {
        s.d_obj_goal < self.params.goal_xz_tol
            && s.obj_pos[1] > self.params.goal_y_min
            && s.obj_pos[1] < self.params.goal_y_max
    }

    fn obs_extra(&self, s: &StepState) -> Vec<f32> {
        default_obs_extra(s)
    }

    fn obs_extra_layout(&self) -> Vec<ObsSlice> {
        default_extra_layout()
    }

    fn goal(&self) -> [f32; 3] {
        self.goal
    }
}

/// push@v1 — move the object into the goal zone without lifting: the
/// non-prehensile baseline every manipulation paper reports. Contact
/// push only: dragging by attachment still earns the (reduced) grasp
/// bonus but success is the same zone criterion as pre-M0a (the `@v2`
/// holding==false criterion is an M2 addition, never an in-place edit).
struct PushTask {
    params: TaskParams,
    goal: [f32; 3],
}

impl PushTask {
    fn new(params: TaskParams) -> Self {
        Self {
            params,
            goal: [0.0; 3],
        }
    }
}

impl Task for PushTask {
    fn id(&self) -> &'static str {
        "push@v1"
    }
    fn phrase(&self) -> &'static str {
        "push the cube into the target zone"
    }
    fn env_name(&self) -> &'static str {
        "Push-OrieraArm-v0"
    }
    fn kind(&self) -> TaskKind {
        TaskKind::Push
    }

    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements {
        let props: Vec<[f32; 3]> = scene
            .props
            .iter()
            .map(|p| p.placement.sample(rng))
            .collect();
        let goal = self.params.goal.sample(rng);
        Placements { props, goal }
    }

    fn finalize(&mut self, rng: &mut Lcg, _scene: &SceneGraph, placements: &mut Placements) {
        let obj = placements.props[0];
        let mut attempts = 0;
        loop {
            let d = ((obj[0] - placements.goal[0]).powi(2) + (obj[2] - placements.goal[2]).powi(2))
                .sqrt();
            if d >= self.params.min_separation || attempts >= self.params.max_redraws {
                break;
            }
            placements.goal = self.params.goal.sample(rng);
            attempts += 1;
        }
        self.goal = placements.goal;
    }

    fn reward(&self, s: &StepState) -> f32 {
        let mut reward = -0.002;
        let grasp_event = s.holding && !s.was_holding;
        reward -= s.d_obj_goal;
        reward += if grasp_event { 1.0 } else { 0.0 };
        if self.success(s) {
            reward += 10.0;
        }
        reward
    }

    fn success(&self, s: &StepState) -> bool {
        s.d_obj_goal < self.params.goal_xz_tol
            && s.obj_pos[1] > self.params.goal_y_min
            && s.obj_pos[1] < self.params.goal_y_max
    }

    fn obs_extra(&self, s: &StepState) -> Vec<f32> {
        default_obs_extra(s)
    }

    fn obs_extra_layout(&self) -> Vec<ObsSlice> {
        default_extra_layout()
    }

    fn goal(&self) -> [f32; 3] {
        self.goal
    }
}

// ---------------------------------------------------------------------------
// M2 task family: the @v2 criteria revisions (pick_place@v2 / push@v2 /
// reach@v2 — new ids, v1 entries stay read-only, review P-2) and the new
// tasks stack@v1 / bin_pick@v1 / drawer@v1.
// ---------------------------------------------------------------------------

/// The zone half of the pick/push criterion (v1 semantics, shared).
fn obj_in_zone(params: &TaskParams, s: &StepState) -> bool {
    s.d_obj_goal < params.goal_xz_tol
        && s.obj_pos[1] > params.goal_y_min
        && s.obj_pos[1] < params.goal_y_max
}

/// The `@v2` release + rest half: gripper commanded open for the check
/// and the object has been quasi-static for `rest_steps` consecutive
/// steps (a dropped prop wakes the counter by moving).
fn released_and_resting(params: &TaskParams, s: &StepState) -> bool {
    s.gripper < params.release_below && s.still_steps >= params.rest_steps
}

/// Goal re-draw until it clears the object by `min_separation` (the
/// shared finalize of the goal-drawing tasks; v1 keeps its own inline
/// copy — identical expressions, kept separate so the frozen v1 path is
/// never touched).
fn finalize_goal_separation(
    params: &TaskParams,
    rng: &mut Lcg,
    placements: &mut Placements,
) -> [f32; 3] {
    let obj = placements.props[0];
    let mut attempts = 0;
    loop {
        let d =
            ((obj[0] - placements.goal[0]).powi(2) + (obj[2] - placements.goal[2]).powi(2)).sqrt();
        if d >= params.min_separation || attempts >= params.max_redraws {
            break;
        }
        placements.goal = params.goal.sample(rng);
        attempts += 1;
    }
    placements.goal
}

/// Vertical half-extent of a prop collider (stack height math).
fn prop_half_height(shape: PropShape) -> f32 {
    match shape {
        PropShape::Cuboid { half } => half[1],
        PropShape::Cylinder { half_h, .. } => half_h,
        PropShape::Sphere { r } => r,
    }
}

/// pick_place@v2 — the v1 zone criterion AND released AND resting for
/// `rest_steps` (P-2: "放进目标区"必须经得起 ManiSkill 对比：松爪后仍
/// 在区才算放置). Reward shaping is byte-equal to v1; only the success
/// predicate and the termination slot move.
struct PickPlaceTaskV2 {
    params: TaskParams,
    goal: [f32; 3],
}

impl PickPlaceTaskV2 {
    fn new(params: TaskParams) -> Self {
        Self {
            params,
            goal: [0.0; 3],
        }
    }
}

impl Task for PickPlaceTaskV2 {
    fn id(&self) -> &'static str {
        "pick_place@v2"
    }
    fn phrase(&self) -> &'static str {
        "pick up the cube and place it at the target"
    }
    fn env_name(&self) -> &'static str {
        "PickPlace3D-OrieraArm-v2"
    }
    fn kind(&self) -> TaskKind {
        TaskKind::PickPlace
    }
    fn success_criteria(&self) -> &'static str {
        "v2"
    }

    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements {
        let props: Vec<[f32; 3]> = scene
            .props
            .iter()
            .map(|p| p.placement.sample(rng))
            .collect();
        let goal = self.params.goal.sample(rng);
        Placements { props, goal }
    }

    fn finalize(&mut self, rng: &mut Lcg, _scene: &SceneGraph, placements: &mut Placements) {
        self.goal = finalize_goal_separation(&self.params, rng, placements);
    }

    fn reward(&self, s: &StepState) -> f32 {
        let mut reward = -0.002;
        let grasp_event = s.holding && !s.was_holding;
        if s.holding {
            reward +=
                -1.2 * s.d_obj_goal + 2.0 * (s.obj_pos[1] - self.params.goal_y_min).clamp(0.0, 0.2);
        } else {
            reward -= 0.6 * s.d_ee_obj;
        }
        reward += if grasp_event { 2.0 } else { 0.0 };
        if self.success(s) {
            reward += 10.0;
        }
        reward
    }

    fn success(&self, s: &StepState) -> bool {
        obj_in_zone(&self.params, s) && released_and_resting(&self.params, s)
    }

    fn obs_extra(&self, s: &StepState) -> Vec<f32> {
        default_obs_extra(s)
    }

    fn obs_extra_layout(&self) -> Vec<ObsSlice> {
        default_extra_layout()
    }

    fn goal(&self) -> [f32; 3] {
        self.goal
    }
}

/// push@v2 — the episode must be contact-only: ANY frame with
/// `holding == true` voids the episode (reward carries no grasp bonus,
/// P-2), and the zone criterion stays.
struct PushTaskV2 {
    params: TaskParams,
    goal: [f32; 3],
}

impl PushTaskV2 {
    fn new(params: TaskParams) -> Self {
        Self {
            params,
            goal: [0.0; 3],
        }
    }
}

impl Task for PushTaskV2 {
    fn id(&self) -> &'static str {
        "push@v2"
    }
    fn phrase(&self) -> &'static str {
        "push the cube into the target zone"
    }
    fn env_name(&self) -> &'static str {
        "Push-OrieraArm-v2"
    }
    fn kind(&self) -> TaskKind {
        TaskKind::Push
    }
    fn success_criteria(&self) -> &'static str {
        "v2"
    }

    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements {
        let props: Vec<[f32; 3]> = scene
            .props
            .iter()
            .map(|p| p.placement.sample(rng))
            .collect();
        let goal = self.params.goal.sample(rng);
        Placements { props, goal }
    }

    fn finalize(&mut self, rng: &mut Lcg, _scene: &SceneGraph, placements: &mut Placements) {
        self.goal = finalize_goal_separation(&self.params, rng, placements);
    }

    fn reward(&self, s: &StepState) -> f32 {
        // No grasp edge bonus: dragging-by-attachment must not earn here
        // (review P-2 named v1's grasp reward as the criterion defect).
        let mut reward = -0.002;
        reward -= s.d_obj_goal;
        if s.ever_holding {
            reward -= 2.0;
        }
        if self.success(s) {
            reward += 10.0;
        }
        reward
    }

    fn success(&self, s: &StepState) -> bool {
        obj_in_zone(&self.params, s) && !s.ever_holding
    }

    fn obs_extra(&self, s: &StepState) -> Vec<f32> {
        default_obs_extra(s)
    }

    fn obs_extra_layout(&self) -> Vec<ObsSlice> {
        default_extra_layout()
    }

    fn goal(&self) -> [f32; 3] {
        self.goal
    }
}

/// reach@v2 — byte-equal criterion to reach@v1; the variant exists so
/// experiments can record `success_criteria = "v2"` and the
/// `terminate_on_success` protocol slot (`TaskParams`) without the
/// criteria revision pretending to be the same experiment.
struct ReachTaskV2 {
    params: TaskParams,
    goal: [f32; 3],
}

impl ReachTaskV2 {
    fn new(params: TaskParams) -> Self {
        Self {
            params,
            goal: [0.0; 3],
        }
    }
}

impl Task for ReachTaskV2 {
    fn id(&self) -> &'static str {
        "reach@v2"
    }
    fn phrase(&self) -> &'static str {
        "reach the cube with the gripper"
    }
    fn env_name(&self) -> &'static str {
        "Reach-OrieraArm-v2"
    }
    fn kind(&self) -> TaskKind {
        TaskKind::Reach
    }
    fn success_criteria(&self) -> &'static str {
        "v2"
    }

    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements {
        let props: Vec<[f32; 3]> = scene
            .props
            .iter()
            .map(|p| p.placement.sample(rng))
            .collect();
        let goal = props[0];
        Placements { props, goal }
    }

    fn finalize(&mut self, _rng: &mut Lcg, _scene: &SceneGraph, _placements: &mut Placements) {}

    fn reward(&self, s: &StepState) -> f32 {
        let mut reward = -0.002;
        reward -= s.d_ee_obj;
        if self.success(s) {
            reward += 5.0;
        }
        reward
    }

    fn success(&self, s: &StepState) -> bool {
        s.d_ee_obj < self.params.reach_radius
    }

    fn obs_extra(&self, s: &StepState) -> Vec<f32> {
        let rel = |p: [f32; 3], q: [f32; 3]| [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
        let mut v: Vec<f32> = rel(s.obj_pos, s.ee).to_vec();
        v.extend_from_slice(&rel(s.goal, s.ee));
        v.push(s.holding as i32 as f32);
        v.push(s.obj_pos[1]);
        v.push(s.d_ee_obj);
        v
    }

    fn obs_extra_layout(&self) -> Vec<ObsSlice> {
        default_extra_layout()
    }

    fn goal(&self) -> [f32; 3] {
        self.goal
    }
}

/// stack@v1 — two blocks: A (`props[0]`, the drawn one) must rest ON
/// B (`props[1]`, the fixed base): XZ aligned within `stack_xz_tol`,
/// centre-height difference ≈ the two colliders' half-heights within
/// `stack_y_tol`, gripper released, A quasi-static for `rest_steps`.
struct StackTask {
    params: TaskParams,
    goal: [f32; 3],
    /// hA + hB — the centre-height difference of a clean stack.
    half_sum: f32,
}

impl StackTask {
    fn new(params: TaskParams) -> Self {
        Self {
            params,
            goal: [0.0; 3],
            half_sum: 0.05,
        }
    }
}

impl Task for StackTask {
    fn id(&self) -> &'static str {
        "stack@v1"
    }
    fn phrase(&self) -> &'static str {
        "stack block A on top of block B"
    }
    fn env_name(&self) -> &'static str {
        "Stack-OrieraArm-v0"
    }
    fn kind(&self) -> TaskKind {
        TaskKind::PickPlace
    }
    fn success_criteria(&self) -> &'static str {
        "v2"
    }

    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements {
        // Draw order (receipt): A first (props[0]), then B's placement
        // (Fixed in the presets — zero draws), goal derived from B.
        let props: Vec<[f32; 3]> = scene
            .props
            .iter()
            .map(|p| p.placement.sample(rng))
            .collect();
        let half_sum = match (scene.props.first(), scene.props.get(1)) {
            (Some(a), Some(b)) => prop_half_height(a.shape) + prop_half_height(b.shape),
            _ => 0.05,
        };
        self.half_sum = half_sum;
        let b = props.get(1).copied().unwrap_or([0.30, 0.55, 0.0]);
        let goal = [b[0], b[1] + half_sum, b[2]];
        Placements { props, goal }
    }

    fn finalize(&mut self, rng: &mut Lcg, scene: &SceneGraph, placements: &mut Placements) {
        // Keep A clear of B at spawn: re-draw A only (goal is derived
        // from B, never drawn — order: A, then A redraws).
        let b = placements
            .props
            .get(1)
            .copied()
            .unwrap_or([0.30, 0.55, 0.0]);
        if let Some(placement) = scene.props.first().map(|p| p.placement) {
            let mut attempts = 0;
            while attempts < self.params.max_redraws {
                let d = ((placements.props[0][0] - b[0]).powi(2)
                    + (placements.props[0][2] - b[2]).powi(2))
                .sqrt();
                if d >= self.params.min_separation {
                    break;
                }
                placements.props[0] = placement.sample(rng);
                attempts += 1;
            }
        }
        self.goal = [b[0], b[1] + self.half_sum, b[2]];
    }

    fn reward(&self, s: &StepState) -> f32 {
        let mut reward = -0.002;
        let grasp_event = s.holding && !s.was_holding;
        if s.holding {
            reward +=
                -1.2 * s.d_obj_goal + 2.0 * (s.obj_pos[1] - self.params.goal_y_min).clamp(0.0, 0.2);
        } else {
            reward -= 0.6 * s.d_ee_obj;
        }
        reward += if grasp_event { 2.0 } else { 0.0 };
        if self.success(s) {
            reward += 10.0;
        }
        reward
    }

    fn success(&self, s: &StepState) -> bool {
        let dxz = ((s.obj_pos[0] - s.prop1_pos[0]).powi(2)
            + (s.obj_pos[2] - s.prop1_pos[2]).powi(2))
        .sqrt();
        let dy = (s.obj_pos[1] - s.prop1_pos[1]) - self.half_sum;
        dxz < self.params.stack_xz_tol
            && dy.abs() < self.params.stack_y_tol
            && released_and_resting(&self.params, s)
    }

    fn obs_extra(&self, s: &StepState) -> Vec<f32> {
        default_obs_extra(s)
    }

    fn obs_extra_layout(&self) -> Vec<ObsSlice> {
        default_extra_layout()
    }

    fn goal(&self) -> [f32; 3] {
        self.goal
    }
}

/// bin_pick@v1 — pick the object OUT of the bin walls (the scene's
/// heavy near-static wall props; real bin fixtures are M3) onto the
/// table goal zone. Criterion = the @v2 pick predicate (released +
/// resting).
struct BinPickTask {
    params: TaskParams,
    goal: [f32; 3],
}

impl BinPickTask {
    fn new(params: TaskParams) -> Self {
        Self {
            params,
            goal: [0.0; 3],
        }
    }
}

impl Task for BinPickTask {
    fn id(&self) -> &'static str {
        "bin_pick@v1"
    }
    fn phrase(&self) -> &'static str {
        "pick the cube out of the bin and place it at the target"
    }
    fn env_name(&self) -> &'static str {
        "BinPick-OrieraArm-v0"
    }
    fn kind(&self) -> TaskKind {
        TaskKind::PickPlace
    }
    fn success_criteria(&self) -> &'static str {
        "v2"
    }

    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements {
        let props: Vec<[f32; 3]> = scene
            .props
            .iter()
            .map(|p| p.placement.sample(rng))
            .collect();
        let goal = self.params.goal.sample(rng);
        Placements { props, goal }
    }

    fn finalize(&mut self, rng: &mut Lcg, _scene: &SceneGraph, placements: &mut Placements) {
        self.goal = finalize_goal_separation(&self.params, rng, placements);
    }

    fn reward(&self, s: &StepState) -> f32 {
        PickPlaceTaskV2 {
            params: self.params,
            goal: self.goal,
        }
        .reward(s)
    }

    fn success(&self, s: &StepState) -> bool {
        obj_in_zone(&self.params, s) && released_and_resting(&self.params, s)
    }

    fn obs_extra(&self, s: &StepState) -> Vec<f32> {
        default_obs_extra(s)
    }

    fn obs_extra_layout(&self) -> Vec<ObsSlice> {
        default_extra_layout()
    }

    fn goal(&self) -> [f32; 3] {
        self.goal
    }
}

/// drawer@v1 — the engine has no prismatic fixture yet (M3): the
/// "drawer panel" is a movable box prop on the table and success is
/// pulling it ≥ `pull_distance` along `pull_axis` (toward the base),
/// released and resting. The displacement reads the object's SPAWN
/// position (`obj_start`), not the settled pose.
struct DrawerTask {
    params: TaskParams,
    goal: [f32; 3],
}

impl DrawerTask {
    fn new(params: TaskParams) -> Self {
        Self {
            params,
            goal: [0.0; 3],
        }
    }

    fn axis(&self) -> [f32; 3] {
        let a = self.params.pull_axis;
        let n = (a[0] * a[0] + a[2] * a[2]).sqrt();
        [a[0] / n, 0.0, a[2] / n]
    }
}

impl Task for DrawerTask {
    fn id(&self) -> &'static str {
        "drawer@v1"
    }
    fn phrase(&self) -> &'static str {
        "pull the drawer handle out by the target distance"
    }
    fn env_name(&self) -> &'static str {
        "Drawer-OrieraArm-v0"
    }
    fn kind(&self) -> TaskKind {
        TaskKind::PickPlace
    }
    fn success_criteria(&self) -> &'static str {
        "v2"
    }

    fn reset(&mut self, rng: &mut Lcg, scene: &SceneGraph) -> Placements {
        let props: Vec<[f32; 3]> = scene
            .props
            .iter()
            .map(|p| p.placement.sample(rng))
            .collect();
        let start = props[0];
        let u = self.axis();
        let goal = [
            start[0] + u[0] * self.params.pull_distance,
            start[1],
            start[2] + u[2] * self.params.pull_distance,
        ];
        Placements { props, goal }
    }

    fn finalize(&mut self, _rng: &mut Lcg, _scene: &SceneGraph, _placements: &mut Placements) {}

    fn reward(&self, s: &StepState) -> f32 {
        let mut reward = -0.002;
        let grasp_event = s.holding && !s.was_holding;
        if s.holding {
            reward += -1.2 * s.d_obj_goal;
        } else {
            reward -= 0.6 * s.d_ee_obj;
        }
        reward += if grasp_event { 2.0 } else { 0.0 };
        if self.success(s) {
            reward += 10.0;
        }
        reward
    }

    fn success(&self, s: &StepState) -> bool {
        let u = self.axis();
        let pulled =
            (s.obj_pos[0] - s.obj_start[0]) * u[0] + (s.obj_pos[2] - s.obj_start[2]) * u[2];
        pulled >= self.params.pull_distance && released_and_resting(&self.params, s)
    }

    fn obs_extra(&self, s: &StepState) -> Vec<f32> {
        default_obs_extra(s)
    }

    fn obs_extra_layout(&self) -> Vec<ObsSlice> {
        default_extra_layout()
    }

    fn goal(&self) -> [f32; 3] {
        self.goal
    }
}

/// The shared privileged block: object-relative, goal-relative, hold
/// flag, object height, |tip − object| — the pre-M0a obs tail, byte for
/// byte.
fn default_obs_extra(s: &StepState) -> Vec<f32> {
    let rel = |p: [f32; 3], q: [f32; 3]| [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
    let mut v: Vec<f32> = rel(s.obj_pos, s.ee).to_vec();
    v.extend_from_slice(&rel(s.goal, s.ee));
    v.push(s.holding as i32 as f32);
    v.push(s.obj_pos[1]);
    v.push(s.d_ee_obj);
    v
}

/// The shared privileged layout: object_rel / goal_rel / flags.
fn default_extra_layout() -> Vec<ObsSlice> {
    vec![
        ObsSlice {
            name: "object_rel".into(),
            names: ["obj_rel.x", "obj_rel.y", "obj_rel.z"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        },
        ObsSlice {
            name: "goal_rel".into(),
            names: ["goal_rel.x", "goal_rel.y", "goal_rel.z"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        },
        ObsSlice {
            name: "flags".into(),
            names: ["holding", "obj_height", "dist_ee_obj"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        },
    ]
}

/// One registry row: versioned id → constructor. `phrase` / `env_name`
/// are carried here (not on the [`TaskKind`] alias) so @v2 revisions and
/// the new families can differ from their v1 aliases.
pub struct TaskEntry {
    pub id: &'static str,
    pub kind: TaskKind,
    pub make: fn(TaskParams) -> Box<dyn Task>,
    /// Natural-language instruction (language-conditioned IL).
    pub phrase: &'static str,
    /// Gym env name (the `EnvSpec::name` source).
    pub env_name: &'static str,
    /// Success-semantics revision (`"v1"` / `"v2"`).
    pub success_criteria: &'static str,
}

/// Registry version tag of the bare-family default (`pick_place` ⇒
/// `pick_place@v1` — the frozen alias behaviour).
const REGISTRY: &[TaskEntry] = &[
    TaskEntry {
        id: "reach@v1",
        kind: TaskKind::Reach,
        make: |p| Box::new(ReachTask::new(p)),
        phrase: "reach the cube with the gripper",
        env_name: "Reach-OrieraArm-v0",
        success_criteria: "v1",
    },
    TaskEntry {
        id: "pick_place@v1",
        kind: TaskKind::PickPlace,
        make: |p| Box::new(PickPlaceTask::new(p)),
        phrase: "pick up the cube and place it at the target",
        env_name: "PickPlace3D-OrieraArm-v0",
        success_criteria: "v1",
    },
    TaskEntry {
        id: "push@v1",
        kind: TaskKind::Push,
        make: |p| Box::new(PushTask::new(p)),
        phrase: "push the cube into the target zone",
        env_name: "Push-OrieraArm-v0",
        success_criteria: "v1",
    },
    // ----- @v2 criteria revisions (M2, review P-2): new ids, never an
    // in-place edit of the v1 entries above -----
    TaskEntry {
        id: "pick_place@v2",
        kind: TaskKind::PickPlace,
        make: |p| Box::new(PickPlaceTaskV2::new(p)),
        phrase: "pick up the cube and place it at the target",
        env_name: "PickPlace3D-OrieraArm-v2",
        success_criteria: "v2",
    },
    TaskEntry {
        id: "push@v2",
        kind: TaskKind::Push,
        make: |p| Box::new(PushTaskV2::new(p)),
        phrase: "push the cube into the target zone",
        env_name: "Push-OrieraArm-v2",
        success_criteria: "v2",
    },
    TaskEntry {
        id: "reach@v2",
        kind: TaskKind::Reach,
        make: |p| Box::new(ReachTaskV2::new(p)),
        phrase: "reach the cube with the gripper",
        env_name: "Reach-OrieraArm-v2",
        success_criteria: "v2",
    },
    // ----- new task families (M2) -----
    TaskEntry {
        id: "stack@v1",
        kind: TaskKind::PickPlace,
        make: |p| Box::new(StackTask::new(p)),
        phrase: "stack block A on top of block B",
        env_name: "Stack-OrieraArm-v0",
        success_criteria: "v2",
    },
    TaskEntry {
        id: "bin_pick@v1",
        kind: TaskKind::PickPlace,
        make: |p| Box::new(BinPickTask::new(p)),
        phrase: "pick the cube out of the bin and place it at the target",
        env_name: "BinPick-OrieraArm-v0",
        success_criteria: "v2",
    },
    TaskEntry {
        id: "drawer@v1",
        kind: TaskKind::PickPlace,
        make: |p| Box::new(DrawerTask::new(p)),
        phrase: "pull the drawer handle out by the target distance",
        env_name: "Drawer-OrieraArm-v0",
        success_criteria: "v2",
    },
];

/// The task registry — `@v1` trio (frozen aliases) + `@v2` revisions +
/// the M2 families (`stack` / `bin_pick` / `drawer`).
pub fn task_registry() -> &'static [TaskEntry] {
    REGISTRY
}

/// Build a task by registry id. A versioned id (`"pick_place@v2"`)
/// matches exactly; a bare family name (`"pick_place"`) resolves to the
/// family's `@v1` entry — the frozen pre-M0a alias behaviour (`--task
/// pick_place` stays v1 byte for byte).
pub fn make_task(id: &str, params: TaskParams) -> Result<Box<dyn Task>, String> {
    let entry = task_entry(id)?;
    Ok((entry.make)(params))
}

/// The registry entry of an id (exact or bare `@v1` alias), or the
/// named human error.
pub fn task_entry(id: &str) -> Result<&'static TaskEntry, String> {
    let find = |pred: &dyn Fn(&TaskEntry) -> bool| task_registry().iter().find(|e| pred(e));
    let entry = if id.contains('@') {
        find(&|e: &TaskEntry| e.id == id)
    } else {
        find(&|e: &TaskEntry| e.id.split('@').next() == Some(id) && e.id.ends_with("@v1"))
    };
    entry.ok_or_else(|| {
        let ids: Vec<&str> = task_registry().iter().map(|e| e.id).collect();
        format!("task：未知任务 id {id:?}（合法值：{}）", ids.join(", "))
    })
}

/// Natural language → registry id: registry `phrase()` hits first, then
/// family keywords (stack / bin / drawer before the legacy router so
/// "stack the cube" doesn't fall into pick_place).
pub fn task_id_from_phrase(phrase: &str) -> Option<&'static str> {
    let p = phrase.to_ascii_lowercase();
    for e in task_registry() {
        if p.contains(e.phrase.to_ascii_lowercase().as_str()) {
            return Some(e.id);
        }
    }
    let hits = |words: &[&str]| words.iter().any(|w| p.contains(w));
    if hits(&["stack"]) {
        Some("stack@v1")
    } else if hits(&["bin"]) {
        Some("bin_pick@v1")
    } else if hits(&["drawer", "pull"]) {
        Some("drawer@v1")
    } else {
        TaskKind::from_phrase(phrase).map(|k| k.id())
    }
}

// ---------------------------------------------------------------------------
// TaskKind — the pre-M0a alias layer. The [`Task`] trait objects are the
// real logic now; this enum keeps every existing caller (tests, Python,
// the collection CLI, the NL router) working untouched.
// ---------------------------------------------------------------------------

/// The lab task family — one env, three daily-driver tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// Touch the cube with the tip: curriculum start, solves in seconds.
    Reach,
    /// Grasp the cube and place it in the goal zone (flagship).
    PickPlace,
    /// Move the cube into the goal zone without lifting: the
    /// non-prehensile baseline every manipulation paper reports.
    Push,
}

impl TaskKind {
    pub fn parse(s: &str) -> Option<Self> {
        let base = s.split('@').next().unwrap_or(s);
        match base {
            "reach" => Some(Self::Reach),
            "pick_place" | "pickplace" => Some(Self::PickPlace),
            "push" => Some(Self::Push),
            _ => None,
        }
    }
    /// Language→task resolution (the NL→embodied pivot): exact phrase
    /// first, then distinctive keywords. "push the cube" routes to Push
    /// even though it contains "cube" — keyword sets are disjoint.
    pub fn from_phrase(phrase: &str) -> Option<Self> {
        let p = phrase.to_ascii_lowercase();
        for t in [Self::Reach, Self::PickPlace, Self::Push] {
            if p.contains(&t.phrase().to_ascii_lowercase()) {
                return Some(t);
            }
        }
        let hits = |words: &[&str]| words.iter().any(|w| p.contains(w));
        if hits(&["push"]) {
            Some(Self::Push)
        } else if hits(&["pick", "place"]) {
            Some(Self::PickPlace)
        } else if hits(&["reach", "touch"]) {
            Some(Self::Reach)
        } else {
            None
        }
    }
    pub fn phrase(self) -> &'static str {
        match self {
            Self::Reach => "reach the cube with the gripper",
            Self::PickPlace => "pick up the cube and place it at the target",
            Self::Push => "push the cube into the target zone",
        }
    }
    /// The registry id of this family's v1 task (`pick_place@v1`).
    pub fn id(self) -> &'static str {
        match self {
            Self::Reach => "reach@v1",
            Self::PickPlace => "pick_place@v1",
            Self::Push => "push@v1",
        }
    }
    pub(crate) fn env_name(self) -> &'static str {
        match self {
            Self::Reach => "Reach-OrieraArm-v0",
            Self::PickPlace => "PickPlace3D-OrieraArm-v0",
            Self::Push => "Push-OrieraArm-v0",
        }
    }
}

/// Legacy threshold constants — now the [`TaskParams`] defaults (kept
/// as pub aliases so pre-M0a readers keep compiling).
pub const GOAL_XZ_TOL: f32 = 0.09;
/// Success zone height band (pick_place / push), object centre (m).
pub const GOAL_Y_MIN: f32 = 0.5;
pub const GOAL_Y_MAX: f32 = 0.62;
/// Reach termination radius (tip-to-cube distance).
pub const REACH_RADIUS: f32 = 0.055;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_phrase_routes_keywords_without_cube_trap() {
        assert_eq!(
            TaskKind::from_phrase("pick up the cube"),
            Some(TaskKind::PickPlace)
        );
        assert_eq!(TaskKind::from_phrase("push the cube"), Some(TaskKind::Push));
        assert_eq!(
            TaskKind::from_phrase("reach the cube"),
            Some(TaskKind::Reach)
        );
        assert_eq!(TaskKind::from_phrase("fly to the moon"), None);
    }

    #[test]
    fn task_kind_parse_covers_the_lab_family() {
        assert_eq!(TaskKind::parse("reach"), Some(TaskKind::Reach));
        assert_eq!(TaskKind::parse("pick_place"), Some(TaskKind::PickPlace));
        assert_eq!(TaskKind::parse("push"), Some(TaskKind::Push));
        assert_eq!(TaskKind::parse("fly"), None);
    }

    // ----- M0a additions: Task trait / TaskParams / ObsLayout -----

    #[test]
    fn task_params_defaults_equal_the_legacy_constants() {
        let p = TaskParams::default();
        assert_eq!(p.goal_xz_tol, GOAL_XZ_TOL);
        assert_eq!(p.goal_y_min, GOAL_Y_MIN);
        assert_eq!(p.goal_y_max, GOAL_Y_MAX);
        assert_eq!(p.reach_radius, REACH_RADIUS);
        assert_eq!(p.min_separation, 0.12);
        assert_eq!(p.max_redraws, 8);
        // M2 defaults
        assert!(p.terminate_on_success);
        assert_eq!(p.rest_steps, 10);
        assert_eq!(p.rest_speed, 0.01);
        assert_eq!(p.release_below, 0.5);
        assert_eq!(p.stack_xz_tol, 0.03);
        assert_eq!(p.stack_y_tol, 0.02);
        assert_eq!(p.pull_axis, [-1.0, 0.0, 0.0]);
        assert_eq!(p.pull_distance, 0.08);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn registry_covers_the_family_and_builds_by_id() {
        let ids: Vec<&str> = task_registry().iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            [
                "reach@v1",
                "pick_place@v1",
                "push@v1",
                "pick_place@v2",
                "push@v2",
                "reach@v2",
                "stack@v1",
                "bin_pick@v1",
                "drawer@v1",
            ]
        );
        for id in &ids {
            let t = make_task(id, TaskParams::default()).expect("id must build");
            assert_eq!(t.id(), *id);
        }
        // bare family names resolve to the @v1 entries; junk is an
        // error listing ids
        assert_eq!(
            make_task("pick_place", TaskParams::default()).unwrap().id(),
            "pick_place@v1"
        );
        assert_eq!(
            make_task("stack", TaskParams::default()).unwrap().id(),
            "stack@v1"
        );
        let err = match make_task("fly", TaskParams::default()) {
            Ok(_) => panic!("fly must not build"),
            Err(e) => e,
        };
        assert!(
            err.contains("合法值") && err.contains("pick_place@v1"),
            "{err}"
        );
        // every entry self-describes phrase/env_name/criteria
        for e in task_registry() {
            assert!(!e.phrase.is_empty() && !e.env_name.is_empty(), "{}", e.id);
            assert!(
                e.success_criteria == "v1" || e.success_criteria == "v2",
                "{}",
                e.id
            );
            let t = make_task(e.id, TaskParams::default()).unwrap();
            assert_eq!(t.success_criteria(), e.success_criteria, "{}", e.id);
            assert_eq!(t.phrase(), e.phrase, "{}", e.id);
        }
    }

    #[test]
    fn task_kind_is_an_alias_layer_over_the_registry() {
        // The rows whose id IS the TaskKind alias's @v1 id (the frozen
        // trio) must round-trip through the enum; @v2 revisions and the
        // new families (stack/bin_pick/drawer) answer only by registry
        // id — TaskKind has exactly three variants.
        let aliased = task_registry()
            .iter()
            .filter(|e| e.kind.id() == e.id)
            .count();
        assert_eq!(aliased, 3, "the frozen trio must be the alias rows");
        for e in task_registry().iter().filter(|e| e.kind.id() == e.id) {
            assert_eq!(TaskKind::parse(e.id), Some(e.kind), "{}", e.id);
        }
    }

    #[test]
    fn phrase_routing_covers_the_new_families() {
        assert_eq!(task_id_from_phrase("stack the red block"), Some("stack@v1"));
        assert_eq!(
            task_id_from_phrase("pick the cube out of the bin and place it at the target"),
            Some("bin_pick@v1")
        );
        assert_eq!(
            task_id_from_phrase("pull the drawer handle out by the target distance"),
            Some("drawer@v1")
        );
        assert_eq!(
            task_id_from_phrase("pick up the cube"),
            Some("pick_place@v1"),
            "legacy routing unchanged"
        );
        assert_eq!(task_id_from_phrase("fly to the moon"), None);
    }

    #[test]
    fn task_reward_matches_the_legacy_shaping_constants() {
        let task = make_task("pick_place@v1", TaskParams::default()).unwrap();
        // Object centre inside the success band (y in (0.5, 0.62)).
        let s = StepState {
            ee: [0.0; 3],
            obj_pos: [0.3, 0.55, 0.0],
            goal: [0.3, 0.5, 0.0],
            holding: false,
            was_holding: false,
            d_ee_obj: 0.1,
            d_obj_goal: 0.05,
            ..StepState::default()
        };
        // holding=false: -0.002 - 0.6*d_ee_obj; in-zone success bonus +10
        assert!((task.reward(&s) - (-0.002 - 0.6 * 0.1 + 10.0)).abs() < 1e-6);
        let s_hold = StepState { holding: true, ..s };
        // holding + hold edge (was_holding=false ⇒ grasp_event):
        // -0.002 - 1.2*d_obj_goal + 2*clamp(y-0.5) + 2.0 grasp bonus
        // (+10 in zone)
        assert!(
            (task.reward(&s_hold) - (-0.002 - 1.2 * 0.05 + 2.0 * (0.55 - 0.5) + 2.0 + 10.0)).abs()
                < 1e-6
        );
        // steady hold (no edge): no grasp bonus
        let s_holding_steady = StepState {
            was_holding: true,
            ..s_hold
        };
        assert!(
            (task.reward(&s_holding_steady) - (-0.002 - 1.2 * 0.05 + 2.0 * (0.55 - 0.5) + 10.0))
                .abs()
                < 1e-6
        );
        // outside the height band ⇒ no bonus
        let s_low = StepState {
            obj_pos: [0.3, 0.45, 0.0],
            ..s
        };
        assert!((task.reward(&s_low) - (-0.002 - 0.6 * 0.1)).abs() < 1e-6);
        let reach = make_task("reach@v1", TaskParams::default()).unwrap();
        let s_far = StepState {
            d_ee_obj: 0.2,
            d_obj_goal: 0.2,
            ..s
        };
        assert!((reach.reward(&s_far) - (-0.002 - 0.2)).abs() < 1e-6);
    }

    // ----- M2 additions: @v2 criteria + new task families -----

    fn zone_state() -> StepState {
        // Object resting at the goal, gripper open, quasi-static.
        StepState {
            ee: [0.3, 0.62, 0.0],
            obj_pos: [0.3, 0.525, 0.0],
            goal: [0.3, 0.5, 0.0],
            holding: false,
            was_holding: true,
            d_ee_obj: 0.12,
            d_obj_goal: 0.03,
            gripper: 0.0,
            still_steps: 12,
            ever_holding: true,
            prop1_pos: [0.0; 3],
            obj_start: [0.18, 0.55, 0.0],
        }
    }

    #[test]
    fn pick_place_v2_requires_release_and_rest() {
        let t = make_task("pick_place@v2", TaskParams::default()).unwrap();
        assert_eq!(t.success_criteria(), "v2");
        // Zone + released + resting ⇒ success
        assert!(t.success(&zone_state()));
        // Gripper still commanded closed ⇒ NOT success (v1 would fire)
        let holding = StepState {
            gripper: 1.0,
            ..zone_state()
        };
        assert!(!t.success(&holding));
        let v1 = make_task("pick_place@v1", TaskParams::default()).unwrap();
        assert!(v1.success(&holding), "v1 predicate unchanged (zone only)");
        // Object still moving (dropped but rolling) ⇒ NOT success
        let moving = StepState {
            still_steps: 2,
            ..zone_state()
        };
        assert!(!t.success(&moving));
        // v2 reward shaping equals v1 when success fires
        let zv = zone_state();
        assert!((t.reward(&zv) - v1.reward(&zv)).abs() < 1e-6);
    }

    #[test]
    fn push_v2_voids_any_holding_frame() {
        let t = make_task("push@v2", TaskParams::default()).unwrap();
        let clean = StepState {
            ever_holding: false,
            ..zone_state()
        };
        assert!(t.success(&clean));
        // ONE holding frame this episode voids it, even in-zone now.
        let touched = StepState {
            ever_holding: true,
            ..clean
        };
        assert!(!t.success(&touched));
        // No grasp bonus in the reward (P-2): holding earns nothing.
        let hold_now = StepState {
            holding: true,
            was_holding: false,
            ever_holding: true,
            ..clean
        };
        assert!((t.reward(&hold_now) - (-0.002 - hold_now.d_obj_goal - 2.0)).abs() < 1e-6);
    }

    #[test]
    fn reach_v2_is_the_v1_criterion_under_a_new_id() {
        let v2 = make_task("reach@v2", TaskParams::default()).unwrap();
        let v1 = make_task("reach@v1", TaskParams::default()).unwrap();
        let s = zone_state();
        assert_eq!(v2.success(&s), v1.success(&s));
        assert_eq!(v2.reward(&s), v1.reward(&s));
        assert_eq!(v2.success_criteria(), "v2");
    }

    #[test]
    fn stack_v1_judges_the_relative_stack_pose() {
        let t = make_task("stack@v1", TaskParams::default()).unwrap();
        // A cleanly stacked pair (both 5 cm cubes ⇒ centre diff 0.05).
        let s = StepState {
            obj_pos: [0.30, 0.575, 0.0],
            prop1_pos: [0.30, 0.525, 0.0],
            gripper: 0.0,
            still_steps: 10,
            ..zone_state()
        };
        assert!(t.success(&s));
        // Half a block off in XZ ⇒ no
        let slid = StepState {
            obj_pos: [0.30, 0.575, 0.035],
            ..s
        };
        assert!(!t.success(&slid));
        // Floating 4 cm above the stack top ⇒ no
        let floating = StepState {
            obj_pos: [0.30, 0.615, 0.0],
            ..s
        };
        assert!(!t.success(&floating));
        // Gripper still holding the top block ⇒ no
        let gripped = StepState { gripper: 1.0, ..s };
        assert!(!t.success(&gripped));
    }

    #[test]
    fn drawer_v1_judges_pulled_distance_from_spawn() {
        let t = make_task("drawer@v1", TaskParams::default()).unwrap();
        let mut s = zone_state();
        s.obj_start = [0.25, 0.55, 0.0];
        s.obj_pos = [0.25, 0.55, 0.0];
        assert!(!t.success(&s), "zero pull ⇒ no");
        s.obj_pos = [0.25 - 0.08, 0.55, 0.0];
        assert!(t.success(&s), "pulled the full distance, released, still");
        // Pulled sideways (off-axis) does not count.
        s.obj_pos = [0.25, 0.55, -0.08];
        assert!(!t.success(&s));
    }

    #[test]
    fn bin_pick_v1_shares_the_v2_pick_predicate() {
        let t = make_task("bin_pick@v1", TaskParams::default()).unwrap();
        let p2 = make_task("pick_place@v2", TaskParams::default()).unwrap();
        let s = zone_state();
        assert_eq!(t.success(&s), p2.success(&s));
        let gripped = StepState { gripper: 1.0, ..s };
        assert!(!t.success(&gripped));
        assert_eq!(t.success_criteria(), "v2");
    }

    /// The M2 `task.params` fields serialize; defaults are stripped from
    /// the spec hash by `SceneSpec::canonicalize_task_params` (pinned in
    /// spec.rs) — here only the round-trip is proven.
    #[cfg(feature = "json")]
    #[test]
    fn m2_task_params_round_trip() {
        let p = TaskParams::default();
        let v = serde_json::to_value(p).unwrap();
        for key in TASK_PARAMS_M2_KEYS {
            assert!(v.get(key).is_some(), "default {key} must serialize");
        }
        let back: TaskParams = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
        // a non-default value survives
        let mut p = p;
        p.rest_steps = 5;
        let v = serde_json::to_value(p).unwrap();
        assert_eq!(v["rest_steps"], 5);
        TaskParams::canonicalize_json(&mut serde_json::json!({}));
    }
}
