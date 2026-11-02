//! L9 embodied environment contract (PLAN-embodied-sim) — the Gymnasium
//! signature, mirrored in Rust, driving the L6 `rigid3d` backend.
//!
//! [`EmbodiedEnv`] mirrors `gymnasium.Env` one-to-one: `reset(seed)`
//! returns the first observation, `step(action)` returns
//! `(obs, reward, terminated, truncated, success)` with the ManiSkill
//! convention that `success` rides in `info` for the evaluation
//! ecosystem. Policies are plain functions of the observation; the
//! deterministic tick (#66) and the per-episode fingerprint
//! ([`TableTopEnv::fingerprint`], #67 semantics) make every episode a
//! replayable receipt.
//!
//! **Spec-driven task family (M0a, PLAN-embodied-general §3.1)**: the
//! [`Task`] trait wraps `reach` / `pick_place` / `push` behind a
//! registry keyed by versioned ids (`pick_place@v1`); every threshold
//! lives in [`TaskParams`] (spec → hash, review P-2). [`TaskKind`] stays
//! as the thin alias layer the pre-M0a API (and every existing caller)
//! speaks. The scene is a [`SceneGraph`] — fixtures + props with
//! shapes (cube/cylinder/sphere) and placement ranges (G-3: the cube
//! finally lives in the spec) — and [`ObsLayout`] derives the
//! observation's names/grouping metadata from `RobotSpec` + task
//! without moving a single float (M0a does NOT change the 21-dim
//! vector, only documents it; review P-8). **Domain randomization**
//! (reset-mode, IsaacLab tradition): prop placement, prop density and
//! friction — every episode's draw is recorded in the randomization
//! receipt. **Top-down camera** (feature `cam`): a deterministic
//! overhead RGB track at the control rate, the modality vision-based
//! IL policies actually consume; non-cuboid props render their true
//! footprint (circle for cylinder/sphere) without changing the
//! legacy cube path's bytes.
//!
//! The env type is [`TableTopEnv`]; `PickPlace3D` is kept as a
//! deprecated alias — the struct was the flagship task before the
//! family landed.

/// Legacy observation/action column names (pre-M0a LeRobot feature
/// meta) — kept public as the positional contract the derived
/// [`ObsLayout`] is tested against.
pub use self::embodied_env_defaults::{ACTION_NAMES, OBS_NAMES};
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, World};
use oriera_subsys::rigid3d::PropShape;
use oriera_subsys::{
    ArmDescription, Gravity, JointTargets, JointTelemetry, MultibodySystem, PhysicsParams, Pose3,
    PropPhys, RobotSpec, Scene3D, SimObject, TimeScale,
};

#[cfg(feature = "cam")]
use oriera_subsys::camera::{capture_png, encode_png, render_top_down, CameraView};

/// Static description of an environment: identity, rates, dimensions.
#[derive(Debug, Clone)]
pub struct EnvSpec {
    pub name: &'static str,
    pub fps: f32,
    pub obs_dim: usize,
    pub action_dim: usize,
    pub max_steps: u64,
}

/// One step's outcome — the Gymnasium tuple, plus the `info["success"]`
/// semantics the evaluation ecosystem reads.
#[derive(Debug, Clone, PartialEq)]
pub struct StepOutcome {
    pub obs: Vec<f32>,
    pub reward: f32,
    pub terminated: bool,
    pub truncated: bool,
    pub success: bool,
}

/// The environment contract every OriEra embodied task implements.
pub trait EmbodiedEnv {
    fn spec(&self) -> EnvSpec;
    /// Column names for `observation.state` (LeRobot features meta) —
    /// the [`ObsLayout`]'s flat name list (joint names, M0a layout).
    fn obs_names(&self) -> Vec<String>;
    /// Column names for `action` (LeRobot features meta).
    fn action_names(&self) -> Vec<String>;
    /// Reset to a fresh episode; `seed` fully determines the episode.
    fn reset(&mut self, seed: u64) -> Vec<f32>;
    /// Advance one fixed step with a joint-space action.
    fn step(&mut self, action: &[f32]) -> StepOutcome;
    /// Deterministic receipt of the current episode state (#67 at sim
    /// granularity — the reproducibility claim of the dataset).
    fn fingerprint(&self) -> u64;
}

/// Deterministic LCG (the same constants as the #80 example): episode
/// seeds must fully determine randomization.
pub struct Lcg(u64);

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_f32(&mut self, lo: f32, hi: f32) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let f = (self.0 >> 33) as f32 / (u32::MAX >> 1) as f32;
        lo + f * (hi - lo)
    }
}

// ---------------------------------------------------------------------------
// SceneGraph (M0a G-3): fixtures + props with shapes and placements.
// serde derives ride the `spec` feature (which turns on subsys' serde
// for RobotSpec/PropShape/PropPhys/Scene3D).
// ---------------------------------------------------------------------------

/// One static furniture slab (ground/table/shelf/bin): an axis-aligned
/// box. M0a maps fixture[0] → `Scene3D.ground`, fixture[1] →
/// `Scene3D.table`; more fixtures (shelf/bin) are M2.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(deny_unknown_fields))]
pub struct Fixture {
    pub name: String,
    /// Slab centre, world frame (m).
    pub center: [f32; 3],
    /// Half extents (m).
    pub half: [f32; 3],
    /// Contact friction. M0a: the backend builds both slabs at 0.9 —
    /// other values are a validation error until M2 (no silent defaults).
    pub friction: f32,
}

/// Where an episode's prop spawn: uniform in the XZ range at fixed
/// height, or pinned to a point (no draws).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(rename_all = "snake_case", deny_unknown_fields))]
pub enum Placement {
    /// Draw `[x.lo, x.hi]` then `[z.lo, z.hi]` (draw order is part of
    /// the episode receipt: x first, then z).
    Range { x: [f32; 2], y: f32, z: [f32; 2] },
    /// Fixed world position, zero draws.
    Fixed([f32; 3]),
}

impl Default for Placement {
    /// `Fixed` at the origin — serde's per-field `default` needs a
    /// Default; real placements are always written explicitly.
    fn default() -> Self {
        Self::Fixed([0.0; 3])
    }
}

impl Placement {
    /// Sample the placement; `Range` draws x then z (receipt order).
    fn sample(&self, rng: &mut Lcg) -> [f32; 3] {
        match *self {
            Self::Range { x, y, z } => [rng.next_f32(x[0], x[1]), y, rng.next_f32(z[0], z[1])],
            Self::Fixed(p) => p,
        }
    }
}

/// One dynamic prop: shape + nominal physics + where it spawns.
/// `props[0]` is the *task object* (the one the obs/reward channels
/// track); any further props spawn as scene clutter.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(deny_unknown_fields))]
pub struct PropSpec {
    pub name: String,
    /// Collider primitive. Default: the legacy 5 cm cube.
    #[cfg_attr(feature = "spec", serde(default))]
    pub shape: PropShape,
    /// Nominal physics (the `randomize=false` values; the reset-time
    /// jitter ranges live in the spec's `randomization` block).
    #[cfg_attr(feature = "spec", serde(default))]
    pub phys: PropPhys,
    pub placement: Placement,
}

/// The scene as data: static fixtures + dynamic props. The default is
/// exactly the pre-M0a hardcoded bench (ground slab + table + 5 cm cube
/// at `OBJ_RANGE`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(deny_unknown_fields))]
pub struct SceneGraph {
    pub fixtures: Vec<Fixture>,
    pub props: Vec<PropSpec>,
}

/// Legacy spawn range of the default cube (x ∈ [0.14, 0.22], y = 0.55
/// (table rest height), z ∈ [−0.10, 0.10]).
pub const OBJ_RANGE: Placement = Placement::Range {
    x: [0.14, 0.22],
    y: 0.55,
    z: [-0.10, 0.10],
};

impl Default for SceneGraph {
    fn default() -> Self {
        Self {
            fixtures: vec![
                Fixture {
                    name: "ground".into(),
                    center: [0.0, -0.05, 0.0],
                    half: [2.0, 0.05, 1.5],
                    friction: 0.9,
                },
                Fixture {
                    name: "table".into(),
                    center: [0.32, 0.25, 0.0],
                    half: [0.28, 0.25, 0.32],
                    friction: 0.9,
                },
            ],
            props: vec![PropSpec {
                name: "cube".into(),
                shape: PropShape::default(),
                phys: PropPhys::default(),
                placement: OBJ_RANGE,
            }],
        }
    }
}

impl SceneGraph {
    /// Project onto the L6 static-scene description: fixture[0] →
    /// ground, fixture[1] → table (more fixtures are M2 — [`Self::validate`]
    /// refuses them rather than silently dropping).
    pub fn to_scene3d(&self) -> Scene3D {
        let ground = (self.fixtures[0].center, self.fixtures[0].half);
        let table = self.fixtures.get(1).map(|f| (f.center, f.half));
        Scene3D { ground, table }
    }

    /// Lift a legacy `Scene3D` (ground + optional table) into a
    /// SceneGraph, keeping the default cube prop.
    pub fn from_scene3d(scene: &Scene3D) -> Self {
        let mut g = Self::default();
        g.fixtures[0] = Fixture {
            name: "ground".into(),
            center: scene.ground.0,
            half: scene.ground.1,
            friction: 0.9,
        };
        match scene.table {
            Some((center, half)) => {
                g.fixtures[1] = Fixture {
                    name: "table".into(),
                    center,
                    half,
                    friction: 0.9,
                };
            }
            None => {
                g.fixtures.truncate(1);
            }
        }
        g
    }

    /// Structural checks beyond the serde layer (the parts L6 must be
    /// told about honestly).
    pub fn validate(&self) -> Result<(), String> {
        let ok = |v: f32| v.is_finite() && v > 0.0;
        if self.fixtures.is_empty() || self.fixtures.len() > 2 {
            return Err(format!(
                "scene.fixtures: 目前 {} 个 —— M0a 只支持 1–2 个 fixture（ground[+table]，shelf/bin 在 M2）",
                self.fixtures.len()
            ));
        }
        for (i, f) in self.fixtures.iter().enumerate() {
            if !(f.center.iter().all(|v| v.is_finite()) && f.half.iter().all(|v| ok(*v))) {
                return Err(format!(
                    "scene.fixtures[{i}]（{}）: center 须有限、half 须为正数（half = {:?}）",
                    f.name, f.half
                ));
            }
            if f.friction != 0.9 {
                return Err(format!(
                    "scene.fixtures[{i}]（{}）.friction = {}: M0a 后端固定 0.9（L6 build_scene 硬编码），其他值 M2 开放",
                    f.name, f.friction
                ));
            }
        }
        if self.props.is_empty() {
            return Err("scene.props: 至少要有一个 prop（props[0] 是任务对象）".into());
        }
        for (i, p) in self.props.iter().enumerate() {
            if !p.shape.is_valid() {
                return Err(format!(
                    "scene.props[{i}]（{}）.shape {:?}: 尺寸须为有限正数",
                    p.name, p.shape
                ));
            }
            if !(p.phys.density.is_finite() && p.phys.density > 0.0) {
                return Err(format!(
                    "scene.props[{i}]（{}）.phys.density = {}: 须为有限正数",
                    p.name, p.phys.density
                ));
            }
            if !(p.phys.friction.is_finite() && p.phys.friction >= 0.0) {
                return Err(format!(
                    "scene.props[{i}]（{}）.phys.friction = {}: 须有限且 ≥ 0",
                    p.name, p.phys.friction
                ));
            }
            if let Placement::Range { x, z, .. } = p.placement {
                if x[0] > x[1] || z[0] > z[1] {
                    return Err(format!(
                        "scene.props[{i}]（{}）.placement: 区间须 lo ≤ hi（x = {:?}, z = {:?}）",
                        p.name, x, z
                    ));
                }
            }
        }
        Ok(())
    }
}

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
}

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
        }
    }
}

impl TaskParams {
    pub fn validate(&self) -> Result<(), String> {
        let ok = |v: f32| v.is_finite() && v > 0.0;
        if !ok(self.goal_xz_tol) || !ok(self.reach_radius) || !ok(self.min_separation) {
            return Err("task.params: goal_xz_tol / reach_radius / min_separation 须为有限正数".into());
        }
        if !(self.goal_y_min.is_finite() && self.goal_y_max.is_finite() && self.goal_y_min < self.goal_y_max)
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
#[derive(Debug, Clone, Copy)]
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
        let props: Vec<[f32; 3]> = scene.props.iter().map(|p| p.placement.sample(rng)).collect();
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
struct PickPlaceTask {
    params: TaskParams,
    goal: [f32; 3],
}

impl PickPlaceTask {
    fn new(params: TaskParams) -> Self {
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
        let props: Vec<[f32; 3]> = scene.props.iter().map(|p| p.placement.sample(rng)).collect();
        let goal = self.params.goal.sample(rng);
        Placements { props, goal }
    }

    fn finalize(&mut self, rng: &mut Lcg, _scene: &SceneGraph, placements: &mut Placements) {
        // Minimum spawn separation: re-draw the goal until it is at
        // least `min_separation` away in XZ (bounded by `max_redraws`).
        let obj = placements.props[0];
        let mut attempts = 0;
        loop {
            let d = ((obj[0] - placements.goal[0]).powi(2)
                + (obj[2] - placements.goal[2]).powi(2))
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
        let props: Vec<[f32; 3]> = scene.props.iter().map(|p| p.placement.sample(rng)).collect();
        let goal = self.params.goal.sample(rng);
        Placements { props, goal }
    }

    fn finalize(&mut self, rng: &mut Lcg, _scene: &SceneGraph, placements: &mut Placements) {
        let obj = placements.props[0];
        let mut attempts = 0;
        loop {
            let d = ((obj[0] - placements.goal[0]).powi(2)
                + (obj[2] - placements.goal[2]).powi(2))
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

/// One registry row: versioned id → constructor.
pub struct TaskEntry {
    pub id: &'static str,
    pub kind: TaskKind,
    pub make: fn(TaskParams) -> Box<dyn Task>,
}

/// The M0a task registry — `reach` / `pick_place` / `push`, all `@v1`.
pub fn task_registry() -> &'static [TaskEntry] {
    &[
        TaskEntry {
            id: "reach@v1",
            kind: TaskKind::Reach,
            make: |p| Box::new(ReachTask::new(p)),
        },
        TaskEntry {
            id: "pick_place@v1",
            kind: TaskKind::PickPlace,
            make: |p| Box::new(PickPlaceTask::new(p)),
        },
        TaskEntry {
            id: "push@v1",
            kind: TaskKind::Push,
            make: |p| Box::new(PushTask::new(p)),
        },
    ]
}

/// Build a task by registry id. Accepts the bare family name
/// (`"pick_place"`) or the versioned id (`"pick_place@v1"`).
pub fn make_task(id: &str, params: TaskParams) -> Result<Box<dyn Task>, String> {
    let base = id.split('@').next().unwrap_or(id);
    task_registry()
        .iter()
        .find(|e| e.id.split('@').next() == Some(base))
        .map(|e| (e.make)(params))
        .ok_or_else(|| {
            let ids: Vec<&str> = task_registry().iter().map(|e| e.id).collect();
            format!("task：未知任务 id {id:?}（合法值：{}）", ids.join(", "))
        })
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
    fn env_name(self) -> &'static str {
        match self {
            Self::Reach => "Reach-OrieraArm-v0",
            Self::PickPlace => "PickPlace3D-OrieraArm-v0",
            Self::Push => "Push-OrieraArm-v0",
        }
    }
}

// ---------------------------------------------------------------------------
// ObsLayout (review P-8): the observation's name/group metadata,
// derived from RobotSpec + Task. M0a does NOT move a single float — the
// 21-dim vector keeps its order; this only documents it (LeRobot
// features names / modality.json / v3 metadata derive from here).
// ---------------------------------------------------------------------------

/// One named run of observation dims (a contiguous slice).
#[derive(Debug, Clone, PartialEq)]
pub struct ObsSlice {
    /// Group key (modality.json / v3 metadata: `arm`, `arm_vel`, …).
    pub name: String,
    /// Per-dimension names (`base_yaw.pos`, `ee.x`, …).
    pub names: Vec<String>,
}

/// One observation group: `state` (proprioceptive) or
/// `environment_state` (privileged).
#[derive(Debug, Clone, PartialEq)]
pub struct ObsGroup {
    pub name: String,
    pub slices: Vec<ObsSlice>,
}

/// The observation layout: groups → slices → dims, plus the total dim.
/// Derived from the robot's joints and the task's privileged block.
#[derive(Debug, Clone, PartialEq)]
pub struct ObsLayout {
    pub groups: Vec<ObsGroup>,
    pub total: usize,
}

impl ObsLayout {
    /// Derive from the robot + task: joints ×(pos, vel) + gripper + EE
    /// pose are `state`; the task's `obs_extra` block is
    /// `environment_state`.
    pub fn derive(robot: &RobotSpec, task: &dyn Task) -> Self {
        let joint = |suffix: &str| -> Vec<String> {
            robot
                .joints
                .iter()
                .map(|j| format!("{}.{}", j.name, suffix))
                .collect()
        };
        let s = |names: &[&str]| -> Vec<String> { names.iter().map(|s| s.to_string()).collect() };
        let groups = vec![
            ObsGroup {
                name: "state".into(),
                slices: vec![
                    ObsSlice {
                        name: "arm".into(),
                        names: joint("pos"),
                    },
                    ObsSlice {
                        name: "arm_vel".into(),
                        names: joint("vel"),
                    },
                    ObsSlice {
                        name: "gripper".into(),
                        names: s(&["gripper.pos"]),
                    },
                    ObsSlice {
                        name: "ee_pose".into(),
                        names: s(&["ee.x", "ee.y", "ee.z"]),
                    },
                ],
            },
            ObsGroup {
                name: "environment_state".into(),
                slices: task.obs_extra_layout(),
            },
        ];
        let total = groups
            .iter()
            .flat_map(|g| g.slices.iter().map(|sl| sl.names.len()))
            .sum();
        Self { groups, total }
    }

    /// Every dim name, in vector order.
    pub fn flat_names(&self) -> Vec<String> {
        self.groups
            .iter()
            .flat_map(|g| g.slices.iter())
            .flat_map(|sl| sl.names.iter().cloned())
            .collect()
    }

    /// Index of a dim by name (the obs-noise exemption uses `holding`).
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.flat_names().iter().position(|n| n == name)
    }

    /// `(slice name, start, end)` in vector order across all groups —
    /// the modality.json / v3 metadata source.
    pub fn slices_with_offsets(&self) -> Vec<(String, usize, usize)> {
        let mut out = Vec::new();
        let mut off = 0;
        for g in &self.groups {
            for sl in &g.slices {
                out.push((sl.name.clone(), off, off + sl.names.len()));
                off += sl.names.len();
            }
        }
        out
    }
}

/// The action layout: `dof` target angles + gripper demand.
pub fn action_layout(robot: &RobotSpec) -> ObsLayout {
    let joint: Vec<String> = robot
        .joints
        .iter()
        .map(|j| format!("{}.tgt", j.name))
        .collect();
    let total = robot.dof() + 1;
    ObsLayout {
        groups: vec![ObsGroup {
            name: "action".into(),
            slices: vec![
                ObsSlice {
                    name: "arm".into(),
                    names: joint,
                },
                ObsSlice {
                    name: "gripper".into(),
                    names: vec!["gripper".into()],
                },
            ],
        }],
        total,
    }
}

/// Ticks run in `reset` before the first observation, so the dropped
/// cube has settled and episodes start from a clean, deterministic
/// resting state.
pub const SETTLE_TICKS: u64 = 45;

/// Legacy threshold constants — now the [`TaskParams`] defaults (kept
/// as pub aliases so pre-M0a readers keep compiling).
pub const GOAL_XZ_TOL: f32 = 0.09;
/// Success zone height band (pick_place / push), object centre (m).
pub const GOAL_Y_MIN: f32 = 0.5;
pub const GOAL_Y_MAX: f32 = 0.62;
/// Reach termination radius (tip-to-cube distance).
pub const REACH_RADIUS: f32 = 0.055;

/// Table-top manipulation environment for the OriEra arm.
///
/// Observation (21 dims at the 4-DoF preset; see [`ObsLayout`]): joint
/// angles ×4, joint velocities ×4, gripper demand, end-effector pose
/// ×3, object relative to tip ×3, goal relative to tip ×3, holding
/// flag, object height, |tip-object|. Action (5 dims): 4 target joint
/// angles (rad) + gripper (0..1).
pub struct TableTopEnv {
    /// The robot this env runs (M0a: spec-driven; `ArmDescription` is
    /// its 4-DoF preset projection).
    pub robot: RobotSpec,
    /// Scene as data (fixtures + props with shapes/placements).
    pub scene: SceneGraph,
    /// Task family alias (the registry build happens per-reset from
    /// `task_params`; setting this field keeps the pre-M0a API).
    pub task: TaskKind,
    /// Thresholds/placement ranges (spec → hash; P-2).
    pub task_params: TaskParams,
    pub dt: f32,
    pub max_steps: u64,
    /// Enable reset-mode physics randomization (cube density/friction
    /// jitter in addition to placement draws). Recorded in the
    /// randomization receipt either way.
    pub randomize: bool,
    /// Arm-side DR half-ranges (reset draws): motor gain ±, link mass
    /// ±. 0.0 disables that axis (defaults keep the M0 behaviour).
    pub kp_jitter: f32,
    pub mass_jitter: f32,
    /// Additive observation noise (isaac `enable_corruption`); 0.0 off.
    pub obs_noise: f32,
    /// Prop density DR range (absolute kg/m³), drawn for props[0].
    pub prop_density_range: [f32; 2],
    /// Prop friction DR range (absolute), drawn for props[0].
    pub prop_friction_range: [f32; 2],
    /// Solver knobs handed to L6 (part of the fingerprint).
    pub physics: PhysicsParams,
    /// Where the task goal is (world frame; from the task's draws).
    pub goal: [f32; 3],
    world: World,
    sys: MultibodySystem,
    arm: Entity,
    /// Task-object entity (`scene.props[0]`).
    obj: Entity,
    rng: Lcg,
    steps: u64,
    was_holding: bool,
    tick: u64,
    rand_note: String,
    /// The (possibly randomized) robot this episode runs with —
    /// nominal `robot` is never mutated.
    robot_working: RobotSpec,
    /// The task implementation (rebuilt each reset from the registry).
    task_impl: Box<dyn Task>,
    /// Observation layout metadata (names/groups; the vector itself is
    /// unchanged in M0a).
    layout: ObsLayout,
    /// Action layout metadata.
    actions: ObsLayout,
    /// Top-down camera (feature `cam`): Some ⇒ frames captured at the
    /// the control rate (30 Hz under the 60 Hz loop).
    #[cfg(feature = "cam")]
    cam: Option<CameraView>,
    /// (tick, PNG bytes) of the open episode; drained by
    /// [`TableTopEnv::take_frames`] on export.
    frames: Vec<(u64, Vec<u8>)>,
}

impl TableTopEnv {
    pub fn new() -> Self {
        Self::with(
            ArmDescription::default(),
            Scene3D::default(),
            1.0 / 60.0,
            700,
        )
    }

    pub fn with(desc: ArmDescription, scene: Scene3D, dt: f32, max_steps: u64) -> Self {
        Self::from_parts(
            RobotSpec::from(&desc),
            SceneGraph::from_scene3d(&scene),
            dt,
            max_steps,
        )
    }

    /// Spec-driven constructor (M0a): any [`RobotSpec`] + [`SceneGraph`].
    pub fn from_parts(robot: RobotSpec, scene: SceneGraph, dt: f32, max_steps: u64) -> Self {
        robot
            .validate()
            .unwrap_or_else(|e| panic!("TableTopEnv::from_parts：robot 非法：{e}"));
        scene
            .validate()
            .unwrap_or_else(|e| panic!("TableTopEnv::from_parts：scene 非法：{e}"));
        let layout = {
            let probe = make_task(TaskKind::PickPlace.id(), TaskParams::default())
                .expect("pick_place@v1 must build");
            ObsLayout::derive(&robot, probe.as_ref())
        };
        let actions = action_layout(&robot);
        Self {
            robot,
            scene,
            task: TaskKind::PickPlace,
            task_params: TaskParams::default(),
            dt,
            max_steps,
            randomize: true,
            kp_jitter: 0.0,
            mass_jitter: 0.0,
            obs_noise: 0.0,
            prop_density_range: [240.0, 640.0],
            prop_friction_range: [0.5, 1.1],
            physics: PhysicsParams::default(),
            goal: [0.0; 3],
            world: World::new(),
            sys: MultibodySystem::new(ArmDescription::default(), Scene3D::default()),
            arm: Entity {
                index: 0,
                generation: 0,
            },
            obj: Entity {
                index: 0,
                generation: 0,
            },
            rng: Lcg::new(1),
            steps: 0,
            was_holding: false,
            tick: 0,
            rand_note: String::new(),
            robot_working: RobotSpec::from(&ArmDescription::default()),
            task_impl: Box::new(PickPlaceTask::new(TaskParams::default())),
            layout,
            actions,
            #[cfg(feature = "cam")]
            cam: None,
            frames: Vec::new(),
        }
    }

    /// Observation layout metadata (names/groups; M0a: vector unchanged).
    pub fn obs_layout(&self) -> &ObsLayout {
        &self.layout
    }

    /// Action layout metadata.
    pub fn action_layout(&self) -> &ObsLayout {
        &self.actions
    }

    /// The active task's registry id (`pick_place@v1`).
    pub fn task_id(&self) -> &'static str {
        self.task.id()
    }

    /// Per-joint action scale (rad per unit action) — the wrapper
    /// layers' passthrough (review U-10; was the hardcoded ×2.0).
    pub fn action_scale(&self) -> &[f32] {
        &self.robot.action_scale
    }

    /// Mount the top-down camera (feature `cam`).
    #[cfg(feature = "cam")]
    pub fn set_camera(&mut self, view: Option<CameraView>) {
        self.cam = view;
    }

    /// Drain the open episode's captured PNG frames: `(tick, bytes)`.
    pub fn take_frames(&mut self) -> Vec<(u64, Vec<u8>)> {
        std::mem::take(&mut self.frames)
    }

    /// The randomization receipt of the current/last reset.
    pub fn randomization_receipt(&self) -> &str {
        &self.rand_note
    }

    fn observe(&mut self) -> Vec<f32> {
        let tel = self
            .world
            .get::<JointTelemetry>(self.arm)
            .cloned()
            .unwrap_or_default();
        let obj = self
            .world
            .get::<Pose3>(self.obj)
            .copied()
            .unwrap_or_default();
        let ee = tel.ee_pos;
        let gripper = self
            .world
            .get::<JointTargets>(self.arm)
            .map(|t| t.gripper)
            .unwrap_or(0.0);
        let mut obs = Vec::with_capacity(self.layout.total);
        obs.extend_from_slice(&tel.angles);
        obs.extend_from_slice(&tel.velocities);
        obs.push(gripper);
        obs.extend_from_slice(&ee);
        let s = StepState {
            ee,
            obj_pos: obj.pos,
            goal: self.goal,
            holding: tel.holding,
            was_holding: self.was_holding,
            d_ee_obj: 0.0,
            d_obj_goal: 0.0,
        };
        obs.extend(self.task_impl.obs_extra(&s));
        // Observation corruption (IsaacLab `enable_corruption`): uniform
        // additive noise on the float channels, drawn from the episode
        // rng — same seed ⇒ same noise sequence. The holding flag stays
        // clean by name, not by count (take(20) used to corrupt it —
        // caught in the 20-role review); the index is a layout lookup.
        let holding_idx = self.layout.index_of("holding").unwrap_or(usize::MAX);
        if self.obs_noise > 0.0 {
            for (i, v) in obs.iter_mut().enumerate() {
                if i == holding_idx {
                    continue;
                }
                *v += self.rng.next_f32(-self.obs_noise, self.obs_noise);
            }
        }
        obs
    }

    fn ctx(&self) -> FrameCtx<'static> {
        FrameCtx {
            frame: oriera_common::FrameIndex(self.tick),
            dt: self.dt,
            now: oriera_common::Timestamp(0),
            grants: oriera_common::Grants::EMPTY,
        }
    }

    /// One top-down frame as PNG bytes. The legacy cube (default
    /// `PropShape`) rides the untouched `capture_png` path (byte-stable
    /// frames); a non-default shape re-renders and stamps the true
    /// footprint — circle for cylinder/sphere (G-3: 改一字段看得出).
    #[cfg(feature = "cam")]
    fn frame_png(&self, view: &CameraView, obj: [f32; 3]) -> Result<Vec<u8>, String> {
        let chain = self.sys.chain_points();
        let scene = self.scene.to_scene3d();
        let shape = self.scene.props.first().map(|p| p.shape).unwrap_or_default();
        if shape == PropShape::default() {
            return capture_png(view, &scene, &chain, obj, self.goal);
        }
        // Push the legacy cube marker off-screen, then stamp the real
        // footprint at the prop's position (integer math, deterministic).
        let off = [
            view.center[0] - view.extent[0] * 8.0,
            0.0,
            view.center[1] - view.extent[1] * 8.0,
        ];
        let mut rgb = render_top_down(view, &scene, &chain, off, self.goal);
        stamp_prop(view, &mut rgb, obj, shape);
        encode_png(&rgb, view.w, view.h)
    }
}

impl Default for TableTopEnv {
    fn default() -> Self {
        Self::new()
    }
}

/// Stamp a non-default prop footprint onto a rendered RGB frame
/// (top-down): filled circle for cylinder/sphere, rect for a custom
/// cuboid. Same prop colour as the legacy cube marker.
#[cfg(feature = "cam")]
fn stamp_prop(view: &CameraView, rgb: &mut [u8], pos: [f32; 3], shape: PropShape) {
    let prop_colour = [255u8, 206, 110];
    let scale = |metres: f32, extent: f32, px: u32| {
        (metres / extent * px as f32).round() as i32
    };
    // Same projection as `CameraView::px` (private in subsys; kept in
    // lockstep — deterministic integer rounding).
    let px = ((pos[0] - (view.center[0] - view.extent[0] / 2.0)) / view.extent[0]
        * view.w as f32)
        .round() as i32;
    let py = ((pos[2] - (view.center[1] - view.extent[1] / 2.0)) / view.extent[1]
        * view.h as f32)
        .round() as i32;
    let put = |rgb: &mut [u8], x: i32, y: i32| {
        if x < 0 || y < 0 || x >= view.w as i32 || y >= view.h as i32 {
            return;
        }
        let i = (y as u32 * view.w + x as u32) as usize * 3;
        rgb[i..i + 3].copy_from_slice(&prop_colour);
    };
    match shape {
        // A custom cuboid would double-draw identically through the
        // legacy marker; only the default shape takes the fast path, so
        // stamp rounded extents here (covers e.g. half=[0.03,0.01,0.04]).
        PropShape::Cuboid { half } => {
            let (hw, hh) = (
                scale(half[0], view.extent[0], view.w),
                scale(half[2], view.extent[1], view.h),
            );
            for y in (py - hh)..=(py + hh) {
                for x in (px - hw)..=(px + hw) {
                    put(rgb, x, y);
                }
            }
        }
        PropShape::Cylinder { .. } | PropShape::Sphere { .. } => {
            let (fx, _) = shape.footprint();
            let r = scale(fx, view.extent[0], view.w);
            for y in (py - r)..=(py + r) {
                for x in (px - r)..=(px + r) {
                    if (x - px).pow(2) + (y - py).pow(2) <= r * r {
                        put(rgb, x, y);
                    }
                }
            }
        }
    }
}

impl EmbodiedEnv for TableTopEnv {
    fn spec(&self) -> EnvSpec {
        EnvSpec {
            name: self.task.env_name(),
            fps: (1.0 / self.dt).round(),
            obs_dim: self.layout.total,
            action_dim: self.actions.total,
            max_steps: self.max_steps,
        }
    }

    fn obs_names(&self) -> Vec<String> {
        self.layout.flat_names()
    }

    fn action_names(&self) -> Vec<String> {
        self.actions.flat_names()
    }

    fn reset(&mut self, seed: u64) -> Vec<f32> {
        self.rng = Lcg::new(seed ^ 0x5eed_00c0_ffee_0001);
        let _ = &mut self.rng; // draw order: obj first, then goal
        self.steps = 0;
        self.tick = 0;
        self.was_holding = false;
        self.frames.clear();
        self.task_impl = make_task(self.task.id(), self.task_params)
            .unwrap_or_else(|e| panic!("reset：{e}"));

        // Placements live INSIDE the tip-down workspace of the small
        // bench arm (horizontal reach ≈ 0.30 m from the base axis with
        // the wrist aimed straight down) — goals outside it are not
        // just hard, they are unreachable, and the task must stay
        // solvable for every seed. The ranges themselves now come from
        // the SceneGraph spec (default = the legacy OBJ_RANGE).
        let mut placements = self.task_impl.reset(&mut self.rng, &self.scene);

        // Reset-mode physics randomization (IsaacLab EventTerm): cube
        // density/friction jitter — the two knobs sim2real reviewers
        // look for first. Draws recorded in the receipt.
        let (density, friction) = if self.randomize {
            (
                self.rng
                    .next_f32(self.prop_density_range[0], self.prop_density_range[1]),
                self.rng
                    .next_f32(self.prop_friction_range[0], self.prop_friction_range[1]),
            )
        } else {
            (self.scene.props[0].phys.density, self.scene.props[0].phys.friction)
        };
        // Arm-side DR: motor gains ±kp_jitter, link mass ±mass_jitter
        // (IsaacLab EventTerm spirit — the policy must not overfit the
        // nominal servo tuning). Draws recorded in the receipt.
        let mut robot = self.robot.clone();
        let mut dr_note = String::new();
        if self.randomize && self.kp_jitter > 0.0 {
            let g = 1.0 - self.kp_jitter + 2.0 * self.kp_jitter * self.rng.next_f32(0.0, 1.0);
            for j in &mut robot.joints {
                j.kp *= g;
                j.kd *= g;
            }
            dr_note.push_str(&format!(" gain_x{g:.3}"));
        }
        if self.randomize && self.mass_jitter > 0.0 {
            let g = 1.0 - self.mass_jitter + 2.0 * self.mass_jitter * self.rng.next_f32(0.0, 1.0);
            robot.mass_scale *= g;
            dr_note.push_str(&format!(" mass_x{g:.3}"));
        }
        self.robot_working = robot;
        // Receipt v2: obs-noise profile appended so downstream consumers
        // can distinguish channel semantics (v2 = noise on all float
        // channels EXCEPT the holding flag; v1 datasets noised it by
        // accident).
        let noise_tag = if self.obs_noise > 0.0 {
            format!(" obsnoise=v2/±{}", self.obs_noise)
        } else {
            String::new()
        };
        let obj_name = &self.scene.props[0].name;
        self.rand_note = format!(
            "{obj_name}_density={density:.0}kg/m3 {obj_name}_friction={friction:.2}{dr_note}{noise_tag} {}",
            if self.randomize {
                "profile=default"
            } else {
                "profile=off"
            }
        );
        // Phase 2: the task enforces its placement constraints (the DR
        // draws above sit between the phases — draw order is receipt).
        self.task_impl
            .finalize(&mut self.rng, &self.scene, &mut placements);
        self.goal = placements.goal;

        self.world = World::new();
        self.world.insert_resource(Gravity(9.8));
        self.world.insert_resource(TimeScale(1.0));
        self.arm = self.world.spawn();
        self.world.insert(
            self.arm,
            JointTargets {
                angles: self.robot.home.clone(),
                gripper: 0.0,
            },
        );
        self.world.insert(self.arm, JointTelemetry::default());
        for (i, prop) in self.scene.props.iter().enumerate() {
            let e = self.world.spawn();
            self.world.insert(
                e,
                Pose3 {
                    pos: placements.props[i],
                    ..Default::default()
                },
            );
            self.world.insert(e, SimObject);
            // props[0] (the task object) carries the DR draw; scene
            // clutter keeps its nominal physics.
            let pp = if i == 0 {
                PropPhys { density, friction }
            } else {
                prop.phys
            };
            self.world.insert(e, pp);
            self.world.insert(e, prop.shape);
            if i == 0 {
                self.obj = e;
            }
        }
        self.world.insert_resource(self.physics);
        self.sys = MultibodySystem::from_robot(self.robot_working.clone(), self.scene.to_scene3d())
            .unwrap_or_else(|e| panic!("reset：robot 构建失败：{e}"));

        // Settle: let the cube land and the chain stabilize, so every
        // episode starts from the same resting posture.
        for _ in 0..SETTLE_TICKS {
            self.sys.run(&self.ctx(), &mut self.world);
            self.tick += 1;
        }
        self.observe()
    }

    fn step(&mut self, action: &[f32]) -> StepOutcome {
        // NaN/Inf 拒绝（np.clip(NaN)=NaN 只靠包装层是假的——Rust 侧是
        // 最后防线；NN 策略发散时输出非有限值是真实场景）。
        if action.iter().any(|a| !a.is_finite()) {
            return StepOutcome {
                obs: self.observe(),
                reward: 0.0,
                terminated: false,
                truncated: false,
                success: false,
            };
        }
        let mut angles: Vec<f32> = self.robot.home.clone();
        for (i, a) in angles.iter_mut().enumerate() {
            *a = action.get(i).copied().unwrap_or(*a);
        }
        let gripper = action
            .get(self.robot.dof())
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        if let Some(t) = self.world.get_mut::<JointTargets>(self.arm) {
            t.angles = angles;
            t.gripper = gripper;
        }
        // Top-down camera at the control rate — frame t shows state_t,
        // the same state obs row t describes; one capture per step ⇒
        // frame count == row count by construction (the off-by-one of
        // the previous placement is gone).
        #[cfg(feature = "cam")]
        if let Some(view) = &self.cam {
            let obj_now = self
                .world
                .get::<Pose3>(self.obj)
                .copied()
                .unwrap_or_default();
            if let Ok(png) = self.frame_png(view, obj_now.pos) {
                self.frames.push((self.tick, png));
            }
        }

        self.sys.run(&self.ctx(), &mut self.world);
        self.tick += 1;
        self.steps += 1;

        let tel = self
            .world
            .get::<JointTelemetry>(self.arm)
            .cloned()
            .unwrap_or_default();
        let obj = self
            .world
            .get::<Pose3>(self.obj)
            .copied()
            .unwrap_or_default();
        let ee = tel.ee_pos;
        let d_ee_obj = ((obj.pos[0] - ee[0]).powi(2)
            + (obj.pos[1] - ee[1]).powi(2)
            + (obj.pos[2] - ee[2]).powi(2))
        .sqrt();
        let d_obj_goal = ((obj.pos[0] - self.goal[0]).powi(2)
            + (obj.pos[1] - self.goal[1]).powi(2)
            + (obj.pos[2] - self.goal[2]).powi(2))
        .sqrt();

        let s = StepState {
            ee,
            obj_pos: obj.pos,
            goal: self.goal,
            holding: tel.holding,
            was_holding: self.was_holding,
            d_ee_obj,
            d_obj_goal,
        };
        // Dense shaping (IsaacLab/ManiSkill tradition), per task; the
        // success bonus rides inside the reward (task trait).
        let reward = self.task_impl.reward(&s);
        let terminated = self.task_impl.success(&s);
        self.was_holding = tel.holding;
        let truncated = !terminated && self.steps >= self.max_steps;

        StepOutcome {
            obs: self.observe(),
            reward,
            terminated,
            truncated,
            success: terminated,
        }
    }

    fn fingerprint(&self) -> u64 {
        // 完整实验身份：sim 状态 + 臂参数（sim-engine review 补全）
        self.sys.fingerprint(&self.world)
    }
}

impl TableTopEnv {
    /// Current cube pose (introspection for baselines and tests).
    pub fn cube_pos(&self) -> [f32; 3] {
        self.world
            .get::<Pose3>(self.obj)
            .map(|p| p.pos)
            .unwrap_or([0.0; 3])
    }

    /// The task goal (introspection).
    pub fn goal(&self) -> [f32; 3] {
        self.goal
    }

    /// Whether the gripper currently holds the cube.
    pub fn holding(&self) -> bool {
        self.was_holding
    }

    /// End-effector position (introspection for the reach baselines).
    pub fn ee_pos(&self) -> [f32; 3] {
        self.world
            .get::<JointTelemetry>(self.arm)
            .map(|t| t.ee_pos)
            .unwrap_or([0.0; 3])
    }
}

mod embodied_env_defaults {
    /// Legacy observation column names, in order (the pre-M0a LeRobot
    /// features meta). M0a keeps the vector untouched; the layout
    /// metadata ([`crate::embodied_env::ObsLayout`]) now carries
    /// joint-name dims — a contract test pins the positional
    /// equivalence between this constant and the derived layout.
    pub const OBS_NAMES: [&str; 21] = [
        "j0_yaw",
        "j1_shoulder",
        "j2_elbow",
        "j3_wrist",
        "qd0",
        "qd1",
        "qd2",
        "qd3",
        "gripper",
        "ee_x",
        "ee_y",
        "ee_z",
        "obj_rel_x",
        "obj_rel_y",
        "obj_rel_z",
        "goal_rel_x",
        "goal_rel_y",
        "goal_rel_z",
        "holding",
        "obj_height",
        "dist_ee_obj",
    ];

    /// Legacy action column names, in order.
    pub const ACTION_NAMES: [&str; 5] = [
        "tgt_j0_yaw",
        "tgt_j1_shoulder",
        "tgt_j2_elbow",
        "tgt_j3_wrist",
        "gripper",
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiggle_policy(step: u64) -> Vec<f32> {
        // A deterministic placeholder policy: slow sinusoidal joint
        // wiggle, gripper pulsing every 40 ticks.
        let a = (step as f32 * 0.05).sin() * 0.3;
        let b = (step as f32 * 0.03).sin() * 0.4;
        vec![
            a,
            0.5 + b,
            -0.8 - a,
            0.2,
            if step.is_multiple_of(40) { 0.0 } else { 1.0 },
        ]
    }

    fn env_with_task(task: TaskKind) -> TableTopEnv {
        let mut e = TableTopEnv::new();
        e.task = task;
        e
    }

    #[test]
    fn same_seed_same_episode_bit_for_bit() {
        let mut e1 = TableTopEnv::new();
        let mut e2 = TableTopEnv::new();
        let o1 = e1.reset(7);
        let o2 = e2.reset(7);
        assert_eq!(o1.len(), 21);
        assert_eq!(o1, o2, "same seed ⇒ identical first observation");
        assert_eq!(e1.fingerprint(), e2.fingerprint(), "…and identical state");
        for i in 0..60 {
            let r1 = e1.step(&wiggle_policy(i));
            let r2 = e2.step(&wiggle_policy(i));
            assert_eq!(r1, r2, "step {i} must match bit-for-bit");
        }
        assert_eq!(e1.fingerprint(), e2.fingerprint());
    }

    #[test]
    fn different_sees_diverge() {
        let mut e1 = TableTopEnv::new();
        let mut e2 = TableTopEnv::new();
        let o1 = e1.reset(7);
        let o2 = e2.reset(8);
        assert_ne!(o1, o2, "different seeds ⇒ different placements");
    }

    #[test]
    fn dims_and_spec_are_consistent() {
        for task in [TaskKind::Reach, TaskKind::PickPlace, TaskKind::Push] {
            let env = env_with_task(task);
            let spec = env.spec();
            assert_eq!(spec.obs_dim, 21);
            assert_eq!(spec.action_dim, 5);
            assert_eq!(env.obs_names().len(), spec.obs_dim);
            assert_eq!(env.action_names().len(), spec.action_dim);
            assert!(spec.max_steps > 0 && spec.fps > 0.0);
        }
    }

    #[test]
    fn reward_is_finite_and_time_limit_truncates() {
        let mut env =
            TableTopEnv::with(ArmDescription::default(), Scene3D::default(), 1.0 / 60.0, 5);
        env.reset(3);
        let mut last = None;
        for i in 0..10 {
            last = Some(env.step(&wiggle_policy(i)));
        }
        let out = last.expect("stepped");
        assert!(out.truncated, "10 steps past max 5 must truncate");
        assert!(!out.terminated);
        assert_eq!(out.obs.len(), 21);
    }

    #[test]
    fn success_fires_when_the_cube_reaches_the_goal() {
        let mut env = TableTopEnv::new();
        env.reset(11);
        // Teleport the cube into the goal zone (external Pose3 write —
        // the adopted-write path of the L6 backend) and let it settle.
        if let Some(p) = env.world.get_mut::<Pose3>(env.obj) {
            p.pos = env.goal;
        }
        let out = env.step(&[0.0; 5]);
        assert!(out.success, "cube in zone ⇒ success: {:?}", out);
        assert!(out.terminated && !out.truncated);
        assert!(out.reward > 5.0, "success bonus must dominate");
    }

    #[test]
    fn reach_task_terminates_on_contact() {
        let mut env = env_with_task(TaskKind::Reach);
        env.reset(4);
        // Park the tip next to the cube via teleport-adopt: put the
        // cube where the tip already is.
        let ee = env.ee_pos();
        if let Some(p) = env.world.get_mut::<Pose3>(env.obj) {
            p.pos = [ee[0], ee[1] - 0.02, ee[2]];
        }
        let out = env.step(&[0.0; 5]);
        assert!(
            out.success,
            "tip within 5cm of cube ⇒ reach success: {:?}",
            out
        );
    }

    #[test]
    fn randomization_receipt_records_the_draws() {
        let mut env = TableTopEnv::new();
        env.reset(9);
        assert!(env.randomization_receipt().contains("cube_density="));
        assert!(env.randomization_receipt().contains("profile=default"));
        env.randomize = false;
        env.reset(9);
        assert!(env.randomization_receipt().contains("profile=off"));
    }

    #[test]
    fn arm_side_dr_changes_the_episode_and_is_recorded() {
        let mut a = TableTopEnv::new();
        let mut b = TableTopEnv::new();
        a.kp_jitter = 0.2;
        a.mass_jitter = 0.15;
        let o1 = a.reset(21);
        let o2 = b.reset(21);
        assert_ne!(o1, o2, "gain/mass jitter ⇒ different rollout");
        let note = a.randomization_receipt();
        assert!(note.contains("gain_x") && note.contains("mass_x"), "{note}");
        // Determinism: same seed + same knobs ⇒ identical episode.
        let mut c = TableTopEnv::new();
        c.kp_jitter = 0.2;
        c.mass_jitter = 0.15;
        c.reset(21);
        for i in 0..30 {
            assert_eq!(a.step(&wiggle_policy(i)), c.step(&wiggle_policy(i)));
        }
    }

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
    }

    #[test]
    fn registry_covers_the_family_and_builds_by_id() {
        let ids: Vec<&str> = task_registry().iter().map(|e| e.id).collect();
        assert_eq!(ids, ["reach@v1", "pick_place@v1", "push@v1"]);
        for id in ids {
            let t = make_task(id, TaskParams::default()).expect("id must build");
            assert_eq!(t.id(), id);
        }
        // bare family names resolve too; junk is an error listing ids
        assert_eq!(
            make_task("pick_place", TaskParams::default()).unwrap().id(),
            "pick_place@v1"
        );
        let err = match make_task("fly", TaskParams::default()) {
            Ok(_) => panic!("fly must not build"),
            Err(e) => e,
        };
        assert!(err.contains("合法值") && err.contains("pick_place@v1"), "{err}");
    }

    #[test]
    fn task_kind_is_an_alias_layer_over_the_registry() {
        for e in task_registry() {
            assert_eq!(e.kind.id(), e.id, "TaskKind ↔ registry id");
            assert_eq!(TaskKind::parse(e.id), Some(e.kind));
        }
    }

    #[test]
    fn obs_layout_matches_the_legacy_vector_by_position() {
        let robot = RobotSpec::from(&ArmDescription::default());
        let task = make_task("pick_place@v1", TaskParams::default()).unwrap();
        let layout = ObsLayout::derive(&robot, task.as_ref());
        assert_eq!(layout.total, 21);
        let flat = layout.flat_names();
        // positional semantic equivalence: legacy name ↔ derived name
        let pairs: [(&str, &str); 21] = [
            ("j0_yaw", "base_yaw.pos"),
            ("j1_shoulder", "shoulder.pos"),
            ("j2_elbow", "elbow.pos"),
            ("j3_wrist", "wrist_pitch.pos"),
            ("qd0", "base_yaw.vel"),
            ("qd1", "shoulder.vel"),
            ("qd2", "elbow.vel"),
            ("qd3", "wrist_pitch.vel"),
            ("gripper", "gripper.pos"),
            ("ee_x", "ee.x"),
            ("ee_y", "ee.y"),
            ("ee_z", "ee.z"),
            ("obj_rel_x", "obj_rel.x"),
            ("obj_rel_y", "obj_rel.y"),
            ("obj_rel_z", "obj_rel.z"),
            ("goal_rel_x", "goal_rel.x"),
            ("goal_rel_y", "goal_rel.y"),
            ("goal_rel_z", "goal_rel.z"),
            ("holding", "holding"),
            ("obj_height", "obj_height"),
            ("dist_ee_obj", "dist_ee_obj"),
        ];
        for (i, (legacy, derived)) in pairs.iter().enumerate() {
            assert_eq!(&flat[i], derived, "derived dim {i}");
            assert_eq!(OBS_NAMES[i], *legacy, "legacy dim {i}");
        }
        // groups: proprioceptive vs privileged (P-8)
        assert_eq!(layout.groups[0].name, "state");
        assert_eq!(layout.groups[1].name, "environment_state");
        // holding exemption index must stay 18 (obs-noise v2 contract)
        assert_eq!(layout.index_of("holding"), Some(18));
        // modality slices reproduce the legacy grouping exactly
        assert_eq!(
            layout.slices_with_offsets(),
            vec![
                ("arm".to_string(), 0, 4),
                ("arm_vel".to_string(), 4, 8),
                ("gripper".to_string(), 8, 9),
                ("ee_pose".to_string(), 9, 12),
                ("object_rel".to_string(), 12, 15),
                ("goal_rel".to_string(), 15, 18),
                ("flags".to_string(), 18, 21),
            ]
        );
        // action layout
        let act = action_layout(&robot);
        assert_eq!(act.total, 5);
        assert_eq!(
            act.slices_with_offsets(),
            vec![
                ("arm".to_string(), 0, 4),
                ("gripper".to_string(), 4, 5),
            ]
        );
    }

    #[test]
    fn layout_matches_the_env_construction() {
        let env = TableTopEnv::new();
        let robot = RobotSpec::from(&ArmDescription::default());
        let probe = make_task("pick_place@v1", TaskParams::default()).unwrap();
        assert_eq!(env.obs_layout(), &ObsLayout::derive(&robot, probe.as_ref()));
        assert_eq!(env.obs_layout().total, env.spec().obs_dim);
        assert_eq!(env.action_layout().total, env.spec().action_dim);
    }

    #[test]
    fn scenegraph_default_is_the_legacy_bench() {
        let g = SceneGraph::default();
        let s = g.to_scene3d();
        let legacy = Scene3D::default();
        assert_eq!(s.ground, legacy.ground);
        assert_eq!(s.table, legacy.table);
        // round-trip is exact
        assert_eq!(SceneGraph::from_scene3d(&legacy), SceneGraph::default());
        assert!(g.validate().is_ok(), "{:?}", g.validate());
    }

    #[test]
    fn scenegraph_validates_honestly() {
        let mut g = SceneGraph::default();
        g.fixtures.push(Fixture {
            name: "shelf".into(),
            center: [0.4, 0.9, 0.0],
            half: [0.2, 0.02, 0.2],
            friction: 0.9,
        });
        let err = g.validate().unwrap_err();
        assert!(err.contains("M0a"), "{err}");
        let mut g = SceneGraph::default();
        g.fixtures[1].friction = 0.5;
        assert!(g.validate().unwrap_err().contains("friction"));
        let mut g = SceneGraph::default();
        g.props[0].shape = PropShape::Cylinder {
            half_h: -0.02,
            radius: 0.025,
        };
        assert!(g.validate().unwrap_err().contains("shape"));
        let mut g = SceneGraph::default();
        g.props.clear();
        assert!(g.validate().unwrap_err().contains("props"));
    }

    #[test]
    fn cylinder_prop_runs_and_fingerprints_differently() {
        // G-3 core: one field in the spec swaps the cube for a cylinder;
        // the env must run end-to-end (same 21-dim contract).
        let mut g = SceneGraph::default();
        g.props[0].shape = PropShape::Cylinder {
            half_h: 0.025,
            radius: 0.025,
        };
        let mut env = TableTopEnv::from_parts(
            RobotSpec::from(&ArmDescription::default()),
            g,
            1.0 / 60.0,
            700,
        );
        env.task = TaskKind::PickPlace;
        let obs = env.reset(1000);
        assert_eq!(obs.len(), 21);
        let mut cube = TableTopEnv::new();
        let obs_cube = cube.reset(1000);
        assert_ne!(obs, obs_cube, "cylinder vs cube ⇒ different physics");
        for i in 0..40 {
            let o = env.step(&wiggle_policy(i));
            assert_eq!(o.obs.len(), 21);
            assert!(o.reward.is_finite());
        }
    }

    #[test]
    fn placement_fixed_draws_nothing_range_draws_x_then_z() {
        let mut rng = Lcg::new(5);
        let fixed = Placement::Fixed([0.2, 0.55, 0.03]);
        assert_eq!(fixed.sample(&mut rng), [0.2, 0.55, 0.03]);
        let mut r1 = Lcg::new(9);
        let mut r2 = Lcg::new(9);
        let p = Placement::Range {
            x: [0.0, 1.0],
            y: 0.55,
            z: [0.0, 1.0],
        };
        let a = p.sample(&mut r1);
        let x_draw = r2.next_f32(0.0, 1.0);
        let z_draw = r2.next_f32(0.0, 1.0);
        assert_eq!(a, [x_draw, 0.55, z_draw], "x first, then z");
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
        };
        // holding=false: -0.002 - 0.6*d_ee_obj; in-zone success bonus +10
        assert!((task.reward(&s) - (-0.002 - 0.6 * 0.1 + 10.0)).abs() < 1e-6);
        let s_hold = StepState {
            holding: true,
            ..s
        };
        // holding + hold edge (was_holding=false ⇒ grasp_event):
        // -0.002 - 1.2*d_obj_goal + 2*clamp(y-0.5) + 2.0 grasp bonus
        // (+10 in zone)
        assert!(
            (task.reward(&s_hold)
                - (-0.002 - 1.2 * 0.05 + 2.0 * (0.55 - 0.5) + 2.0 + 10.0))
            .abs()
                < 1e-6
        );
        // steady hold (no edge): no grasp bonus
        let s_holding_steady = StepState {
            was_holding: true,
            ..s_hold
        };
        assert!(
            (task.reward(&s_holding_steady)
                - (-0.002 - 1.2 * 0.05 + 2.0 * (0.55 - 0.5) + 10.0))
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

    #[cfg(feature = "cam")]
    #[test]
    fn camera_frames_are_deterministic_and_drained() {
        let mut e1 = TableTopEnv::new();
        let mut e2 = TableTopEnv::new();
        e1.set_camera(Some(CameraView::default()));
        e2.set_camera(Some(CameraView::default()));
        e1.reset(7);
        e2.reset(7);
        for i in 0..12 {
            e1.step(&wiggle_policy(i));
            e2.step(&wiggle_policy(i));
        }
        let f1 = e1.take_frames();
        assert_eq!(
            f1.len(),
            e2.take_frames().len(),
            "same seed ⇒ same frame count"
        );
        assert!(f1.len() >= 2, "12 ticks @ 30 Hz must capture ≥2 frames");
        assert!(f1.windows(2).all(|w| w[0].0 < w[1].0), "ticks must ascend");
        // PNG signature: deterministic bytes start with the magic.
        assert_eq!(&f1[0].1[0..8], b"\x89PNG\r\n\x1a\n");
        let drained = e1.take_frames();
        assert!(drained.is_empty(), "take_frames must drain");
    }

    #[cfg(feature = "cam")]
    #[test]
    fn cylinder_frames_draw_a_circle_not_a_square() {
        // G-3, render half: the cylinder footprint must be a filled
        // disc — the square's corners stay table-dark; the legacy cube
        // path keeps its bytes.
        let view = CameraView::default();
        let mut g = SceneGraph::default();
        g.props[0].shape = PropShape::Cylinder {
            half_h: 0.025,
            radius: 0.025,
        };
        let mut env = TableTopEnv::from_parts(
            RobotSpec::from(&ArmDescription::default()),
            g,
            1.0 / 60.0,
            700,
        );
        env.reset(1000);
        let pos = env.cube_pos();
        let png = env.frame_png(&view, pos).expect("render");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");

        // Decode-independent check: rebuild the RGB and probe pixels.
        let off = [
            view.center[0] - view.extent[0] * 8.0,
            0.0,
            view.center[1] - view.extent[1] * 8.0,
        ];
        let scene = env.scene.to_scene3d();
        let mut rgb = render_top_down(&view, &scene, &env.sys.chain_points(), off, env.goal());
        stamp_prop(&view, &mut rgb, pos, PropShape::Cylinder {
            half_h: 0.025,
            radius: 0.025,
        });
        let scale = |m: f32| (m / view.extent[0] * view.w as f32).round() as i32;
        let px = ((pos[0] - (view.center[0] - view.extent[0] / 2.0)) / view.extent[0]
            * view.w as f32)
            .round() as i32;
        let py = ((pos[2] - (view.center[1] - view.extent[1] / 2.0)) / view.extent[1]
            * view.h as f32)
            .round() as i32;
        let prop_colour = [255u8, 206, 110];
        let at = |x: i32, y: i32| {
            let i = (y as u32 * view.w + x as u32) as usize * 3;
            [rgb[i], rgb[i + 1], rgb[i + 2]]
        };
        let r = scale(0.025);
        assert_eq!(at(px, py), prop_colour, "centre is prop-coloured");
        assert_eq!(at(px + r - 1, py), prop_colour, "rim of the disc");
        assert_ne!(
            at(px + r + 1, py),
            prop_colour,
            "one px past the rim is NOT prop-coloured (disc, not square)"
        );
        assert_ne!(
            at(px + r, py + r),
            prop_colour,
            "the square's corner stays dark ⇒ circle outline"
        );
    }
}
