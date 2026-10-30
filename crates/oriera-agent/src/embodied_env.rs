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
//! **Task family** ([`TaskKind`]): `reach` (curriculum start),
//! `pick_place` (the flagship), `push` (non-prehensile baseline) — one
//! env, three lab tasks, all sharing the 21-dim observation / 5-dim
//! action contract. **Domain randomization** (reset-mode, IsaacLab
//! tradition): cube placement, goal placement, cube density and
//! friction — every episode's draw is recorded in the randomization
//! receipt. **Top-down camera** (feature `cam`): a deterministic
//! overhead RGB track at half the control rate, the modality
//! vision-based IL policies actually consume.
//!
//! The env type is [`TableTopEnv`]; `PickPlace3D` is kept as a
//! deprecated alias — the struct was the flagship task before the
//! family landed.

use self::embodied_env_defaults::*;
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, World};
use oriera_subsys::{
    sim_fingerprint, ArmDescription, Gravity, JointTargets, JointTelemetry, MultibodySystem, Pose3,
    PropPhys, Scene3D, SimObject, TimeScale,
};

#[cfg(feature = "cam")]
use oriera_subsys::camera::{capture_png, CameraView};

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
    /// Column names for `observation.state` (LeRobot features meta).
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
        match s {
            "reach" => Some(Self::Reach),
            "pick_place" | "pickplace" => Some(Self::PickPlace),
            "push" => Some(Self::Push),
            _ => None,
        }
    }
    /// Language→task resolution (the NL→embodied pivot): a natural
    /// phrase maps back to the task that promises it.
    pub fn from_phrase(phrase: &str) -> Option<Self> {
        [Self::Reach, Self::PickPlace, Self::Push]
            .into_iter()
            .find(|t| {
                phrase
                    .to_ascii_lowercase()
                    .contains(&t.phrase().to_ascii_lowercase())
                    || t.phrase()
                        .to_ascii_lowercase()
                        .split(' ')
                        .filter(|w| w.len() > 3)
                        .any(|w| phrase.to_ascii_lowercase().contains(w))
            })
    }
    pub fn phrase(self) -> &'static str {
        match self {
            Self::Reach => "reach the cube with the gripper",
            Self::PickPlace => "pick up the cube and place it at the target",
            Self::Push => "push the cube into the target zone",
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

/// Table-top manipulation environment for the OriEra arm.
///
/// Observation (21 dims): joint angles ×4, joint velocities ×4,
/// gripper demand, end-effector pose ×3, object relative to tip ×3,
/// goal relative to tip ×3, holding flag, object height, |tip-object|.
/// Action (5 dims): 4 target joint angles (rad) + gripper (0..1).
pub struct TableTopEnv {
    pub desc: ArmDescription,
    pub scene: Scene3D,
    pub task: TaskKind,
    pub dt: f32,
    pub max_steps: u64,
    /// Enable reset-mode physics randomization (cube density/friction
    /// jitter in addition to placement draws). Recorded in the
    /// randomization receipt either way.
    pub randomize: bool,
    /// Where the cube must end up (task goal), world frame.
    pub goal: [f32; 3],
    world: World,
    sys: MultibodySystem,
    arm: Entity,
    obj: Entity,
    rng: Lcg,
    steps: u64,
    was_holding: bool,
    tick: u64,
    rand_note: String,
    /// Top-down camera (feature `cam`): Some ⇒ frames captured at half
    /// the control rate (30 Hz under the 60 Hz loop).
    #[cfg(feature = "cam")]
    cam: Option<CameraView>,
    /// (tick, PNG bytes) of the open episode; drained by
    /// [`TableTopEnv::take_frames`] on export.
    frames: Vec<(u64, Vec<u8>)>,
}

/// Ticks run in `reset` before the first observation, so the dropped
/// cube has settled and episodes start from a clean, deterministic
/// resting state.
pub const SETTLE_TICKS: u64 = 45;

/// Success zone: XZ distance tolerance and height band (pick_place /
/// push); the reach radius shares the tolerance semantic.
pub const GOAL_XZ_TOL: f32 = 0.09;
pub const GOAL_Y_MIN: f32 = 0.5;
pub const GOAL_Y_MAX: f32 = 0.62;
/// Reach termination radius (tip-to-cube distance).
pub const REACH_RADIUS: f32 = 0.055;

/// Camera capture cadence: every control tick — the track MUST run at
/// the control rate, because HF lerobot indexes video frames by
/// `timestamp × video_fps`; a half-rate track desyncs (the 30 Hz
/// variant was a live bug found in review, 2026-10-30).
#[cfg(feature = "cam")]
pub const CAM_EVERY: u64 = 1;

/// `PickPlace3D` — the flagship task, kept as the pre-family name.
pub type PickPlace3D = TableTopEnv;

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
        Self {
            desc,
            scene,
            task: TaskKind::PickPlace,
            dt,
            max_steps,
            randomize: true,
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
            #[cfg(feature = "cam")]
            cam: None,
            frames: Vec::new(),
        }
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

    fn observe(&self) -> Vec<f32> {
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
        let rel = |p: [f32; 3]| [p[0] - ee[0], p[1] - ee[1], p[2] - ee[2]];
        let o = rel(obj.pos);
        let g = rel(self.goal);
        let dist = (o[0] * o[0] + o[1] * o[1] + o[2] * o[2]).sqrt();
        let gripper = self
            .world
            .get::<JointTargets>(self.arm)
            .map(|t| t.gripper)
            .unwrap_or(0.0);
        let mut obs = Vec::with_capacity(21);
        obs.extend_from_slice(&tel.angles);
        obs.extend_from_slice(&tel.velocities);
        obs.push(gripper);
        obs.extend_from_slice(&ee);
        obs.extend_from_slice(&o);
        obs.extend_from_slice(&g);
        obs.push(tel.holding as i32 as f32);
        obs.push(obj.pos[1]);
        obs.push(dist);
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
}

impl Default for TableTopEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbodiedEnv for TableTopEnv {
    fn spec(&self) -> EnvSpec {
        EnvSpec {
            name: self.task.env_name(),
            fps: (1.0 / self.dt).round(),
            obs_dim: 21,
            action_dim: 5,
            max_steps: self.max_steps,
        }
    }

    fn obs_names(&self) -> Vec<String> {
        OBS_NAMES.iter().map(|s| s.to_string()).collect()
    }

    fn action_names(&self) -> Vec<String> {
        ACTION_NAMES.iter().map(|s| s.to_string()).collect()
    }

    fn reset(&mut self, seed: u64) -> Vec<f32> {
        self.rng = Lcg::new(seed ^ 0x5eed_00c0_ffee_0001);
        let _ = &mut self.rng; // draw order: obj first, then goal
        self.steps = 0;
        self.tick = 0;
        self.was_holding = false;
        self.frames.clear();

        // Placements live INSIDE the tip-down workspace of the small
        // bench arm (horizontal reach ≈ 0.30 m from the base axis with
        // the wrist aimed straight down) — goals outside it are not
        // just hard, they are unreachable, and the task must stay
        // solvable for every seed.
        let (obj, goal) = match self.task {
            TaskKind::Reach => (
                [
                    self.rng.next_f32(0.14, 0.22),
                    0.55,
                    self.rng.next_f32(-0.10, 0.10),
                ],
                [0.0; 3], // unused: the goal IS the cube (obs goal_rel mirrors obj_rel)
            ),
            TaskKind::PickPlace => (
                [
                    self.rng.next_f32(0.14, 0.22),
                    0.55,
                    self.rng.next_f32(-0.10, 0.10),
                ],
                [
                    self.rng.next_f32(0.24, 0.31),
                    0.5,
                    self.rng.next_f32(-0.08, 0.08),
                ],
            ),
            TaskKind::Push => (
                [
                    self.rng.next_f32(0.14, 0.20),
                    0.55,
                    self.rng.next_f32(-0.07, 0.07),
                ],
                [
                    self.rng.next_f32(0.24, 0.31),
                    0.5,
                    self.rng.next_f32(-0.08, 0.08),
                ],
            ),
        };
        // Reset-mode physics randomization (IsaacLab EventTerm): cube
        // density/friction jitter — the two knobs sim2real reviewers
        // look for first. Draws recorded in the receipt.
        let (density, friction) = if self.randomize {
            (self.rng.next_f32(240.0, 640.0), self.rng.next_f32(0.5, 1.1))
        } else {
            (400.0, 0.8)
        };
        self.rand_note = format!(
            "cube_density={density:.0}kg/m3 cube_friction={friction:.2} {}",
            if self.randomize {
                "profile=default"
            } else {
                "profile=off"
            }
        );
        self.goal = goal;

        self.world = World::new();
        self.world.insert_resource(Gravity(9.8));
        self.world.insert_resource(TimeScale(1.0));
        self.arm = self.world.spawn();
        self.world.insert(
            self.arm,
            JointTargets {
                angles: self.desc.home.to_vec(),
                gripper: 0.0,
            },
        );
        self.world.insert(self.arm, JointTelemetry::default());
        self.obj = self.world.spawn();
        self.world.insert(
            self.obj,
            Pose3 {
                pos: obj,
                ..Default::default()
            },
        );
        self.world.insert(self.obj, SimObject);
        self.world.insert(self.obj, PropPhys { density, friction });
        self.sys = MultibodySystem::new(self.desc.clone(), self.scene.clone());

        // Settle: let the cube land and the chain stabilize, so every
        // episode starts from the same resting posture.
        for _ in 0..SETTLE_TICKS {
            self.sys.run(&self.ctx(), &mut self.world);
            self.tick += 1;
        }
        self.observe()
    }

    fn step(&mut self, action: &[f32]) -> StepOutcome {
        let mut angles: Vec<f32> = self.desc.home.to_vec();
        for (i, a) in angles.iter_mut().enumerate() {
            *a = action.get(i).copied().unwrap_or(*a);
        }
        let gripper = action.get(4).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        if let Some(t) = self.world.get_mut::<JointTargets>(self.arm) {
            t.angles = angles;
            t.gripper = gripper;
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

        // Dense shaping (IsaacLab/ManiSkill tradition), per task.
        let mut reward = -0.002;
        let terminated;
        match self.task {
            TaskKind::Reach => {
                reward -= d_ee_obj;
                terminated = d_ee_obj < REACH_RADIUS;
                if terminated {
                    reward += 5.0;
                }
            }
            TaskKind::PickPlace => {
                let grasp_event = tel.holding && !self.was_holding;
                if tel.holding {
                    reward += -1.2 * d_obj_goal + 2.0 * (obj.pos[1] - 0.5).clamp(0.0, 0.2);
                } else {
                    reward -= 0.6 * d_ee_obj;
                }
                reward += if grasp_event { 2.0 } else { 0.0 };
                terminated =
                    d_obj_goal < GOAL_XZ_TOL && obj.pos[1] > GOAL_Y_MIN && obj.pos[1] < GOAL_Y_MAX;
                if terminated {
                    reward += 10.0;
                }
            }
            TaskKind::Push => {
                let grasp_event = tel.holding && !self.was_holding;
                reward -= d_obj_goal;
                reward += if grasp_event { 1.0 } else { 0.0 };
                terminated =
                    d_obj_goal < GOAL_XZ_TOL && obj.pos[1] > GOAL_Y_MIN && obj.pos[1] < GOAL_Y_MAX;
                if terminated {
                    reward += 10.0;
                }
            }
        }
        self.was_holding = tel.holding;
        let truncated = !terminated && self.steps >= self.max_steps;

        // Top-down camera at half the control rate (30 Hz @ 60 Hz loop).
        #[cfg(feature = "cam")]
        if let Some(view) = &self.cam {
            if self.tick.is_multiple_of(crate::embodied_env::CAM_EVERY) {
                let chain = self.sys.chain_points();
                if let Ok(png) = capture_png(view, &self.scene, &chain, obj.pos, self.goal) {
                    self.frames.push((self.tick, png));
                }
            }
        }

        StepOutcome {
            obs: self.observe(),
            reward,
            terminated,
            truncated,
            success: terminated,
        }
    }

    fn fingerprint(&self) -> u64 {
        sim_fingerprint(&self.world)
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
    /// Observation column names, in order (LeRobot features meta).
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

    /// Action column names, in order.
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
    fn task_kind_parse_covers_the_lab_family() {
        assert_eq!(TaskKind::parse("reach"), Some(TaskKind::Reach));
        assert_eq!(TaskKind::parse("pick_place"), Some(TaskKind::PickPlace));
        assert_eq!(TaskKind::parse("push"), Some(TaskKind::Push));
        assert_eq!(TaskKind::parse("fly"), None);
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
}
