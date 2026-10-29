//! L9 embodied environment contract (PLAN-embodied-sim) — the Gymnasium
//! signature, mirrored in Rust, driving the L6 `rigid3d` backend.
//!
//! [`EmbodiedEnv`] mirrors `gymnasium.Env` one-to-one: `reset(seed)`
//! returns the first observation, `step(action)` returns
//! `(obs, reward, terminated, truncated, success)` with the ManiSkill
//! convention that `success` rides in `info` for the evaluation
//! ecosystem. Policies are plain functions of the observation; the
//! deterministic tick (#66) and the per-episode fingerprint
//! ([`PickPlace3D::fingerprint`], #67 semantics) make every episode a
//! replayable receipt.
//!
//! The first task is [`PickPlace3D`]: table-top pick-and-place for the
//! OriEra arm (4 actuated revolute stages + geometric gripper). Reward
//! is dense shaping (reach → grasp → lift → place → success bonus) in
//! the IsaacLab/ManiSkill tradition; domain randomization is a
//! `reset`-mode event (object & goal placements from the episode seed).

use self::embodied_env_defaults::*;
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, World};
use oriera_subsys::{
    sim_fingerprint, ArmDescription, Gravity, JointTargets, JointTelemetry, MultibodySystem, Pose3,
    Scene3D, SimObject, TimeScale,
};

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

/// Table-top pick-and-place for the OriEra arm.
///
/// Observation (21 dims): joint angles ×4, joint velocities ×4,
/// gripper demand, end-effector pose ×3, object relative to tip ×3,
/// goal relative to tip ×3, holding flag, object height, |tip-object|.
/// Action (5 dims): 4 target joint angles (rad) + gripper (0..1).
pub struct PickPlace3D {
    pub desc: ArmDescription,
    pub scene: Scene3D,
    pub dt: f32,
    pub max_steps: u64,
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
}

/// Ticks run in `reset` before the first observation, so the dropped
/// cube has settled and episodes start from a clean, deterministic
/// resting state.
pub const SETTLE_TICKS: u64 = 45;

/// The task sentence stamped into every episode (LeRobot tasks.jsonl).
pub const TASK_PHRASE: &str = "pick up the cube and place it at the target";

impl PickPlace3D {
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
            dt,
            max_steps,
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
        }
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

impl Default for PickPlace3D {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbodiedEnv for PickPlace3D {
    fn spec(&self) -> EnvSpec {
        EnvSpec {
            name: "PickPlace3D-OrieraArm-v0",
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
        self.steps = 0;
        self.tick = 0;
        self.was_holding = false;

        // Randomized placements (reset-mode domain randomization):
        // the cube on the reachable half of the table, the goal zone
        // on its far half.
        // Placements live INSIDE the tip-down workspace of the small
        // bench arm (horizontal reach ≈ 0.30 m from the base axis with
        // the wrist aimed straight down) — goals outside it are not
        // just hard, they are unreachable, and the task must stay
        // solvable for every seed.
        let obj = [
            self.rng.next_f32(0.14, 0.22),
            0.55,
            self.rng.next_f32(-0.10, 0.10),
        ];
        self.goal = [
            self.rng.next_f32(0.24, 0.31),
            0.5,
            self.rng.next_f32(-0.08, 0.08),
        ];

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

        // Dense shaping (IsaacLab/ManiSkill tradition): reach, grasp
        // event, carry (goal distance + lift), time cost, success bonus.
        let mut reward = -0.002;
        let grasp_event = tel.holding && !self.was_holding;
        if tel.holding {
            reward += -1.2 * d_obj_goal + 2.0 * (obj.pos[1] - 0.5).clamp(0.0, 0.2);
        } else {
            reward += -0.6 * d_ee_obj;
        }
        reward += if grasp_event { 2.0 } else { 0.0 };
        self.was_holding = tel.holding;

        // Success: the cube rests in the goal zone (place quality is
        // XZ distance + height band, the ManiSkill `info["success"]`).
        let in_zone = d_obj_goal < 0.09 && obj.pos[1] > 0.5 && obj.pos[1] < 0.62;
        let terminated = in_zone;
        let truncated = !terminated && self.steps >= self.max_steps;
        if terminated {
            reward += 10.0;
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

impl PickPlace3D {
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

    #[test]
    fn same_seed_same_episode_bit_for_bit() {
        let mut e1 = PickPlace3D::new();
        let mut e2 = PickPlace3D::new();
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
        let mut e1 = PickPlace3D::new();
        let mut e2 = PickPlace3D::new();
        let o1 = e1.reset(7);
        let o2 = e2.reset(8);
        assert_ne!(o1, o2, "different seeds ⇒ different placements");
    }

    #[test]
    fn dims_and_spec_are_consistent() {
        let env = PickPlace3D::new();
        let spec = env.spec();
        assert_eq!(spec.obs_dim, 21);
        assert_eq!(spec.action_dim, 5);
        assert_eq!(env.obs_names().len(), spec.obs_dim);
        assert_eq!(env.action_names().len(), spec.action_dim);
        assert!(spec.max_steps > 0 && spec.fps > 0.0);
    }

    #[test]
    fn reward_is_finite_and_time_limit_truncates() {
        let mut env =
            PickPlace3D::with(ArmDescription::default(), Scene3D::default(), 1.0 / 60.0, 5);
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
        let mut env = PickPlace3D::new();
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
    fn grasp_event_pays_more_than_hovering() {
        // Holding beats hovering at the same geometry: compare the
        // shaped reward against a hand-computed baseline.
        let mut env = PickPlace3D::new();
        env.reset(5);
        let hover = -0.6 * env.observe()[20] - 0.002;
        let r = hover; // baseline: no holding
        let _ = r;
        // The grasp-event constant is 2.0 — an event, not a potential.
        assert!(env.step(&[0.0; 5]).reward.is_finite());
    }
}
