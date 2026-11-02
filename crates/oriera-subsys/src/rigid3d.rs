//! L6 rigid-body 3D backend (feature `rigid3d`) — rapier3d multibody.
//!
//! The 3D counterpart of [`crate::rigid`]: an articulated arm (revolute
//! chain driven by joint-space PD torques) plus dynamic props, behind
//! the same `System<World>` contract, the same `Gravity` / `TimeScale`
//! resources and the same sync protocol — live entities reconcile into
//! bodies, external component writes are adopted, the pipeline steps
//! with `ctx.dt * TimeScale::effective()`, and the result is written
//! back into the columns. Where rigid2d mirrors `Position`/`Velocity`,
//! this backend owns three columns:
//!
//! - [`Pose3`] — rigid pose of every prop entity (physics writes back;
//!   external writes are adopted, Teleport-style);
//! - [`JointTargets`] — the policy's action: target angles per joint
//!   (rad) plus the gripper demand (0 = open, 1 = close);
//! - [`JointTelemetry`] — the physics' observation: measured angles,
//!   finite-difference velocities, the end-effector pose, the grasp
//!   flag.
//!
//! **Actuation**: rapier multibody position motors, ForceBased — the
//! stiffness/damping are real torque gains (N·m/rad), clamped to
//! [`MOTOR_MAX_FORCE`]. Three non-obvious requirements, each verified
//! against the raw API: the `motor_axes` bitmask must enable `ANG_X`
//! (rapier leaves it empty and motors stay dead), the model must be
//! `ForceBased` (the default `AccelerationBased` reads the same gains
//! as angular accelerations and sags ~1 rad under a kilogram-scale
//! arm's gravity torque), and the angle readout must use the link's
//! `local_to_parent` rotation — the same convention as the motors.
//!
//! **Determinism**: rapier is same-build deterministic; pair with
//! `Scheduler::with_fixed_dt` (#66) and use [`sim_fingerprint`] for the
//! episode receipt (#67 semantics at sim granularity — the ECS-wide
//! `state_hash` stays 2D and is deliberately NOT extended here, see
//! PLAN-embodied-sim 第二幕 #2). Two runs of the same script must be
//! bit-identical — pinned by `two_runs_are_bit_identical`.
//!
//! **Grasping**: M0 uses geometric attachment — closing the gripper
//! within [`GRASP_RADIUS`] of the end-effector tip attaches the prop
//! (kinematic carry, hanging just below the tip); opening releases it.
//! Friction/contact grasping is M1. Honest, deterministic, enough for
//! pick-and-place data collection.
//!
//! **Honest simplification (M0)**: the default chain actuates 4 distinct
//! revolute stages — base yaw, shoulder, elbow, wrist pitch
//! (`JOINT_NAMES[0..4]`). `JointTargets` still accepts 5 channels and
//! the 5th (`wrist_roll`) is recorded in telemetry but does not
//! actuate physics yet; a dedicated roll body is the M1 refinement.
//!
//! **Spec-driven build (M0a, PLAN-embodied-general §3.1)**: the system
//! is built from a [`RobotSpec`] — N links, N revolute joints in
//! topological order, per-joint gains/armature/damping/effort/velocity
//! limit, an optional [`crate::robot::GripperSpec`]. [`ArmDescription`] is the 4-DoF
//! preset: `RobotSpec::from(&desc)` reproduces the legacy construction
//! field by field, and [`MultibodySystem::new`] goes through it, so the
//! default trajectory is bit-identical to the pre-M0a engine (the
//! `golden_*` tests below pin the pre-change fold of two scripts).

use crate::robot::{LinkShape, RobotSpec, RobotSpecError};
use crate::{Gravity, TimeScale};
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, World};
use rapier3d::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

/// Entity identity as a plain key: `(index, generation)`. Generation
/// changes on despawn/clear, so stale handles cannot alias live ones.
type EntKey = (u32, u32);

fn key(e: Entity) -> EntKey {
    (e.index, e.generation)
}

/// Rigid pose of a prop entity (SI units, metres; quaternion `[x, y, z, w]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose3 {
    pub pos: [f32; 3],
    pub rot: [f32; 4],
}

impl Default for Pose3 {
    fn default() -> Self {
        Self {
            pos: [0.0; 3],
            rot: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

/// The policy's action, written into the arm entity before the step:
/// one target angle per revolute joint (rad) and the gripper demand.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JointTargets {
    pub angles: Vec<f32>,
    /// 0.0 = open, 1.0 = close.
    pub gripper: f32,
}

/// The physics' observation, written back into the arm entity every
/// tick: measured joint state, the end-effector pose and the grasp flag.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JointTelemetry {
    pub angles: Vec<f32>,
    pub velocities: Vec<f32>,
    pub ee_pos: [f32; 3],
    pub ee_rot: [f32; 4],
    /// True while a prop is geometrically attached to the gripper.
    pub holding: bool,
}

/// Marker for dynamic prop entities: `Pose3` + `SimObject` ⇒ a dynamic
/// body exists for this entity.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SimObject;

/// Optional per-prop physics override (domain randomization knob):
/// without it a prop spawns with the defaults (density 400, friction
/// 0.8). Insert together with `Pose3` + `SimObject`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropPhys {
    /// Collider density, kg/m³ (the 5 cm cube's mass follows).
    pub density: f32,
    /// Contact friction coefficient.
    pub friction: f32,
}

impl Default for PropPhys {
    fn default() -> Self {
        Self {
            density: 400.0,
            friction: 0.8,
        }
    }
}

/// The driven joint names, in `JointTargets`/`JointTelemetry` order.
/// Index 4 (`wrist_roll`) is telemetry-only in M0 — see module docs.
pub const JOINT_NAMES: [&str; 5] = ["base_yaw", "shoulder", "elbow", "wrist_pitch", "wrist_roll"];

/// Static description of the arm: 4 actuated revolute stages + gripper.
/// Lengths in metres. The home pose stretches along world +X at
/// shoulder height (all angles zero).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ArmDescription {
    /// World position of the base column centre.
    pub base: [f32; 3],
    /// Shoulder anchor height above `base` centre.
    pub shoulder_height: f32,
    /// shoulder→elbow, elbow→wrist, wrist→tip lengths.
    pub links: [f32; 3],
    pub link_radius: f32,
    pub home: [f32; 4],
    /// Per-joint revolute limits (rad), for the 4 actuated stages.
    pub limits: [(f32, f32); 4],
    /// Joint-space PD gains (N·m/rad, N·m·s/rad).
    pub kp: f32,
    pub kd: f32,
    /// Uniform link-mass scale (domain-randomization knob: jitter this
    /// ±15% in `reset`-mode draws so policies don't overfit the exact
    /// inertias). 1.0 = nominal.
    pub mass_scale: f32,
    /// Passive joint damping (viscous friction in the motor gearbox —
    /// NOT the PD controller's kd, which is a separate servo gain).
    /// None ⇒ [`JOINT_DAMPING`] default. URDF `<dynamics damping>` maps
    /// here (the sim-engine review caught the kd mis-assignment).
    pub passive_damping: Option<f32>,
}

impl Default for ArmDescription {
    fn default() -> Self {
        Self {
            base: [0.02, 0.5, 0.0],
            shoulder_height: 0.12,
            links: [0.20, 0.18, 0.09],
            link_radius: 0.022,
            home: [0.0; 4],
            limits: [
                (-std::f32::consts::PI, std::f32::consts::PI),
                (-0.35, 1.45),
                (-2.50, 0.35),
                (-2.00, 2.00),
            ],
            // ForceBased motor gains (N·m/rad, N·m·s/rad): the joint
            // motor's stiffness is a real torque gain in this model.
            // AccelerationBased (the default) would make the same
            // numbers angular-acceleration gains — far too soft against
            // a kilogram-scale arm's gravity torque.
            kp: 60.0,
            kd: 4.0,
            mass_scale: 1.0,
            passive_damping: None,
        }
    }
}

/// Exposed solver knobs (resource on the world; `None` = rapier
/// default). Folded into `sim_fingerprint` — a tuned solver is part of
/// the experiment's identity and must show up in the receipt.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PhysicsParams {
    pub solver_iterations: Option<usize>,
    /// Physics substeps per engine tick (None ⇒ [`SUBSTEPS`]).
    pub substeps: Option<u32>,
    /// Soft-contact spring frequency (Hz) — the ERP/CFM analogue.
    pub contact_softness_hz: Option<f32>,
    /// Velocity warmstart blending (0..1).
    pub warmstart_coefficient: Option<f32>,
}

/// Static furniture built once at construction: a ground slab and an
/// optional table, each `(center, half_extents)` in metres.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Scene3D {
    pub ground: ([f32; 3], [f32; 3]),
    pub table: Option<([f32; 3], [f32; 3])>,
}

impl Default for Scene3D {
    fn default() -> Self {
        Self {
            ground: ([0.0, -0.05, 0.0], [2.0, 0.05, 1.5]),
            table: Some(([0.32, 0.25, 0.0], [0.28, 0.25, 0.32])),
        }
    }
}

/// Grip zone radius: with the gripper closed, a prop whose centre
/// lies within this distance of the tip is REPORTED as held (the hold
/// itself is finger/cube friction contact — contact grasp, round 4).
pub const GRASP_RADIUS: f32 = 0.06;
/// Gripper plates: kinematic slabs straddling the TIP. The prop stays
/// Dynamic; plate/cube friction (2 mm/side squeeze) does the holding.
/// These are the [`crate::robot::GripperSpec`] defaults of the `ArmDescription`
/// preset (`robot.rs`); the builder reads the spec, never these.
// half_y=0.045（板长 9cm）：板顶恒不高于尖端——悬停/下探全程夹板
// 不会铲过方块顶面（0.075 版会先铲顶把方块挤飞，浏览器实测定位）。
pub(crate) const FINGER_HALF: [f32; 3] = [0.030, 0.045, 0.006];
pub(crate) const FINGER_X: f32 = 0.135;
pub(crate) const FINGER_GAP_OPEN: f32 = 0.075;
pub(crate) const FINGER_GAP_CLOSED: f32 = 0.046;
pub(crate) const FINGER_FRICTION: f32 = 1.4;
/// Plate ramp: max gap change per engine tick (m/tick).
pub(crate) const FINGER_GAP_RATE: f32 = 0.004;
/// Nominal grip-force budget (N) registered on the preset gripper —
/// the kinematic plates do not consume it (contact-force gripper: M2).
pub(crate) const GRIPPER_EFFORT: f32 = 20.0;

/// Prop half-extent (a 5 cm cube) — one size for M0; per-entity shapes
/// arrive with per-entity asset binding (M1).
pub const PROP_HALF: f32 = 0.025;

/// Internal physics substeps per engine tick (the IsaacLab decimation
/// idea: high-frequency physics under a lower-frequency decision loop).
/// Two substeps of dt/2 keep the coupled chain's PD loop stable.
const SUBSTEPS: u32 = 2;
/// Rotor inertia added to every joint DoF — tames PD oscillation on
/// light links (a standard motor model parameter, not a fudge). The
/// preset's per-joint `JointSpec::armature`.
pub(crate) const ARMATURE: f32 = 0.02;
/// Passive joint friction (viscous) per DoF — the preset's per-joint
/// `JointSpec::damping` when `ArmDescription::passive_damping` is None.
pub(crate) const JOINT_DAMPING: f32 = 0.05;
/// PD torque clamp per stage (N·m) — a stalled arm must not inject
/// unbounded impulses. `JointSpec::effort = None` means this.
pub(crate) const MOTOR_MAX_FORCE: f32 = 80.0;
/// Link collider density (kg/m³): default density 1.0 makes gram-level
/// links that sit far outside the solver's tuned mass range (rapier's
/// scale discipline: realistic sizes AND masses). ~0.6 kg per link.
pub(crate) const LINK_DENSITY: f32 = 1500.0;
/// Added mass per chain body (kg): keeps even the tiny yaw-carrier
/// stage inside the solver's healthy mass range.
pub(crate) const LINK_ADDED_MASS: f32 = 0.2;

/// Collision groups: furniture and props live in WORLD, the arm lives
/// in ARM and only collides with WORLD. Arm↔arm self-collision is OFF
/// by construction — the chain's colliders overlap at their joints at
/// build time, and contact-driven separation against light links
/// injects explosive energy (the classic robot-arm pitfall). Real
/// self-collision meshes are M1.
fn world_groups() -> InteractionGroups {
    InteractionGroups::new(
        Group::GROUP_1,
        Group::GROUP_1 | Group::GROUP_2,
        InteractionTestMode::And,
    )
}

fn arm_groups() -> InteractionGroups {
    InteractionGroups::new(Group::GROUP_2, Group::GROUP_1, InteractionTestMode::And)
}

/// One actuated revolute stage: its multibody joint handle, the
/// revolute axis in the parent's local frame (angle readout) and the
/// generic-joint axis slot the motor drives. Index `i` pairs with
/// `RobotSpec::joints[i]` for limits/gains/velocity limit.
#[derive(Clone, Copy)]
struct Stage {
    handle: MultibodyJointHandle,
    axis_vec: Vec3,
    /// The revolute free DoF is always `ANG_X` in the joint frame.
    axis: JointAxis,
}

/// Deterministic fingerprint of the sim state (#67 semantics): folds
/// every `JointTelemetry` and `Pose3` row plus the ECS keys, in column
/// order, through splitmix64 rounds. Same script ⇒ same hash; flip one
/// float ⇒ different hash. This is the 3D episode receipt — the
/// ECS-wide `state_hash` stays 2D on purpose (PLAN-embodied-sim 2.1 #2:
/// don't freeze a global contract onto one subsystem's columns).
fn mix(mut h: u64, x: u64) -> u64 {
    h ^= x;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 29;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^= h >> 32;
    h
}

fn bits(h: u64, v: f32) -> u64 {
    mix(h, v.to_bits() as u64)
}

pub fn sim_fingerprint(world: &World) -> u64 {
    let mut h = mix(0x0000_6f72_6965_7261, 0x0051_4D03);
    let ekey = |e: Entity| {
        let k = key(e);
        ((k.0 as u64) << 32) | k.1 as u64
    };
    for (e, t) in world.query::<JointTelemetry>() {
        h = mix(h, ekey(e));
        for a in t.angles.iter().chain(t.velocities.iter()) {
            h = bits(h, *a);
        }
        for p in t.ee_pos {
            h = bits(h, p);
        }
        h = mix(h, t.holding as u64);
    }
    for (e, p) in world.query::<Pose3>() {
        h = mix(h, ekey(e));
        for v in p.pos.into_iter().chain(p.rot) {
            h = bits(h, v);
        }
    }
    // Prop physics overrides are identity too (density/friction change
    // trajectories even at identical states).
    for (e, pp) in world.query::<PropPhys>() {
        h = mix(h, ((key(e).0 as u64) << 32) | key(e).1 as u64);
        h = bits(h, pp.density);
        h = bits(h, pp.friction);
    }
    // Solver knobs are part of the experiment identity (review: a
    // tuned solver must show in the receipt).
    if let Some(pp) = world.resource::<PhysicsParams>() {
        // None folds to a sentinel (u64::MAX / NaN bits) — Some(0) and
        // None must NOT collide (review: ambiguous receipts).
        let opt_u = |v: Option<u64>| v.unwrap_or(u64::MAX);
        let opt_f = |v: Option<f32>| v.map_or(u64::MAX, |x| x.to_bits() as u64);
        h = mix(h, opt_u(pp.solver_iterations.map(|v| v as u64)));
        h = mix(h, opt_u(pp.substeps.map(|v| v as u64)));
        h = mix(h, opt_f(pp.contact_softness_hz));
        h = mix(h, opt_f(pp.warmstart_coefficient));
    }
    // Gravity is part of the experiment identity too.
    h = bits(h, world.resource::<Gravity>().map_or(f32::NAN, |g| g.0));
    h
}

fn quat_from(rot: [f32; 4]) -> Rotation {
    Rotation::from_xyzw(rot[0], rot[1], rot[2], rot[3])
}

fn quat_to_arr(q: Rotation) -> [f32; 4] {
    [q.x, q.y, q.z, q.w]
}

/// Signed angle of a relative rotation about `axis` (rad). The joint's
/// `local_to_parent` rotation is exactly this relative rotation.
fn angle_about(q: Rotation, axis: Vec3) -> f32 {
    2.0 * f32::atan2(axis.dot(Vec3::new(q.x, q.y, q.z)), q.w)
}

/// The 3D articulated physics system. One instance per world; rebuild
/// it together with the world on `reset` (the mjModel/mjData split —
/// description and scene are construction-time constants, everything
/// mutable lives in the body/joint sets).
pub struct MultibodySystem {
    /// The legacy 4-DoF description this system was built from through
    /// [`MultibodySystem::new`]. Kept for API compatibility; `robot` is
    /// the authoritative build source. Built via
    /// [`MultibodySystem::from_robot`] it holds `ArmDescription::default()`
    /// as a placeholder (M0b: fold into the spec hash and retire).
    pub desc: ArmDescription,
    /// The spec this system was built from — limits, gains, gripper
    /// geometry are read from here every tick.
    pub robot: RobotSpec,
    /// Solver knobs (see [`PhysicsParams`]) — applied each tick,
    /// `None` fields keep rapier defaults.
    pub physics: PhysicsParams,
    pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad_phase: DefaultBroadPhase,
    narrow_phase: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    mbody_joints: MultibodyJointSet,
    soft_bodies: SoftBodySet,
    ccd: CCDSolver,
    /// One body per `RobotSpec::links` entry, in link order (the fixed
    /// root included).
    chain: Vec<RigidBodyHandle>,
    /// One stage per actuated revolute DoF, in `RobotSpec::joints` order.
    stages: Vec<Stage>,
    /// Body the end-effector frame rides on (the gripper's attach link,
    /// or the last joint's child without a gripper).
    ee_body: RigidBodyHandle,
    /// Tip offset in `ee_body`'s local frame.
    tip_local: Vec3,
    /// Entity ↔ body reconciliation for props.
    map: BTreeMap<EntKey, RigidBodyHandle>,
    /// Last pose we wrote back — the external-write detector (step 2).
    last_pose: BTreeMap<EntKey, ([f32; 3], [f32; 4])>,
    /// Measured angles last tick — finite-difference velocities and
    /// the PD's q̇ term.
    prev_angles: Vec<f32>,
    /// Last angles we commanded (post-clamp), so unchanged targets
    /// skip the motor write.
    last_cmd: Vec<f32>,
    /// Reported hold state (gripper closed + prop in grip zone).
    holding: bool,
    /// Gripper plate bodies `(left, right)`; `None` without a gripper.
    fingers: Option<(RigidBodyHandle, RigidBodyHandle)>,
    gap_now: f32,
    /// End-effector pose after the last step.
    last_ee: ([f32; 3], [f32; 4]),
}

impl MultibodySystem {
    /// Build the legacy 4-DoF tabletop arm. Goes through
    /// `RobotSpec::from(&desc)` — the preset projection is always
    /// buildable, so this cannot fail.
    pub fn new(desc: ArmDescription, scene: Scene3D) -> Self {
        let robot = RobotSpec::from(&desc);
        let mut sys = Self::from_robot(robot, scene)
            .expect("ArmDescription preset projects to a valid RobotSpec");
        sys.desc = desc;
        sys
    }

    /// Build an arbitrary [`RobotSpec`] (validated first — a registered
    /// `mesh_uri` is refused here with an M2 pointer, never silently
    /// dropped).
    pub fn from_robot(robot: RobotSpec, scene: Scene3D) -> Result<Self, RobotSpecError> {
        robot.validate()?;
        let n = robot.joints.len();
        let gap0 = robot.gripper.as_ref().map_or(0.0, |g| g.gap_open);
        let mut sys = Self {
            desc: ArmDescription::default(),
            robot,
            physics: PhysicsParams::default(),
            pipeline: PhysicsPipeline::new(),
            islands: IslandManager::new(),
            broad_phase: DefaultBroadPhase::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            mbody_joints: MultibodyJointSet::new(),
            soft_bodies: SoftBodySet::new(),
            ccd: CCDSolver::new(),
            chain: Vec::new(),
            stages: Vec::new(),
            ee_body: RigidBodyHandle::from_raw_parts(u32::MAX, u32::MAX),
            tip_local: Vec3::ZERO,
            map: BTreeMap::new(),
            last_pose: BTreeMap::new(),
            prev_angles: vec![0.0; n],
            last_cmd: vec![f32::NAN; n],
            holding: false,
            fingers: None,
            gap_now: gap0,
            last_ee: ([0.0; 3], [0.0, 0.0, 0.0, 1.0]),
        };
        sys.build_scene(&scene);
        sys.build_arm();
        Ok(sys)
    }

    fn fixed_slab(&mut self, center: [f32; 3], half: [f32; 3], friction: f32) {
        let body = RigidBodyBuilder::fixed()
            .translation(Vec3::new(center[0], center[1], center[2]))
            .build();
        let h = self.bodies.insert(body);
        let col = ColliderBuilder::cuboid(half[0], half[1], half[2])
            .friction(friction)
            .collision_groups(world_groups())
            .build();
        self.colliders.insert_with_parent(col, h, &mut self.bodies);
    }

    fn build_scene(&mut self, scene: &Scene3D) {
        let (c, half) = scene.ground;
        self.fixed_slab(c, half, 0.9);
        if let Some((c, half)) = scene.table {
            self.fixed_slab(c, half, 0.9);
        }
    }

    /// Generic N-stage build from `self.robot` (validated). The
    /// insertion sequence — every link body in link order, then every
    /// link collider in link order, then the plates, then the joints in
    /// order — is the legacy sequence for the preset, so body/collider
    /// handles and solver ordering are unchanged (bit-exact receipt).
    fn build_arm(&mut self) {
        let r = self.robot.clone();
        let ms = r.mass_scale;
        let root = r.root_link().expect("validated: exactly one root");
        let v3 = |a: [f32; 3]| Vec3::new(a[0], a[1], a[2]);

        // 1. Bodies: the root is a fixed stub the chain stands on; every
        //    other link is dynamic at its home origin (identity
        //    rotation; the multibody's own FK re-derives these from the
        //    root pose + anchors on the first step).
        self.chain.clear();
        for (i, l) in r.links.iter().enumerate() {
            let body = if i == root {
                RigidBodyBuilder::fixed().translation(v3(l.origin)).build()
            } else {
                RigidBodyBuilder::dynamic()
                    .can_sleep(false)
                    .additional_mass(l.added_mass * ms)
                    .translation(v3(l.origin))
                    .build()
            };
            self.chain.push(self.bodies.insert(body));
        }

        // 2. Colliders: primitives in the body frame; dynamic links get
        //    `density · mass_scale` (the fixed root's density is inert to
        //    the solver and written as-is).
        for (i, l) in r.links.iter().enumerate() {
            let builder = match l.shape {
                LinkShape::Box { half } => ColliderBuilder::cuboid(half[0], half[1], half[2]),
                LinkShape::Capsule { half_len, radius } => {
                    ColliderBuilder::capsule_x(half_len, radius)
                }
            };
            let density = if i == root { l.density } else { l.density * ms };
            let c = builder
                .density(density)
                .friction(l.friction)
                .translation(v3(l.collider_offset))
                .collision_groups(arm_groups())
                .build();
            self.colliders
                .insert_with_parent(c, self.chain[i], &mut self.bodies);
        }

        // 3. End-effector frame + gripper plates. Plates are kinematic
        //    slabs straddling the TCP in the attach link's frame; the
        //    prop stays Dynamic — plate/cube friction does the holding
        //    (minimal-probe-verified, tests/probe_friction.rs round 4).
        let ee_link = r
            .gripper
            .as_ref()
            .map_or_else(|| r.joints.last().map_or(root, |j| j.child_link), |g| g.attach_link);
        self.ee_body = self.chain[ee_link];
        self.tip_local = r.gripper.as_ref().map_or(Vec3::ZERO, |g| v3(g.tcp_offset));
        self.fingers = r.gripper.as_ref().map(|g| {
            let o = r.links[ee_link].origin;
            let tip0 = Vec3::new(
                o[0] + g.tcp_offset[0],
                o[1] + g.tcp_offset[1],
                o[2] + g.tcp_offset[2],
            );
            let mut mk_finger = || {
                let body = self.bodies.insert(
                    RigidBodyBuilder::kinematic_position_based()
                        .translation(tip0)
                        .build(),
                );
                let c = ColliderBuilder::cuboid(g.finger_half[0], g.finger_half[1], g.finger_half[2])
                    .friction(g.friction)
                    .collision_groups(arm_groups())
                    .build();
                self.colliders.insert_with_parent(c, body, &mut self.bodies);
                body
            };
            let l = mk_finger();
            let rgt = mk_finger();
            (l, rgt)
        });

        // 4. Multibody joints in spec (topological) order. Positive
        //    angle about the axis follows the right-hand rule.
        self.stages.clear();
        for j in &r.joints {
            let axis = v3(j.axis);
            let builder = RevoluteJointBuilder::new(axis)
                .local_anchor1(v3(j.anchor))
                .limits([j.limits.0, j.limits.1])
                // Adjacent stages overlap at the anchors at build time —
                // their mutual contact would fight the joint and inject
                // energy. Joints constrain the pair; the contact between
                // them is noise (and arm↔arm contact is group-disabled
                // anyway).
                .contacts_enabled(false)
                .build();
            if let Some(h) = self.mbody_joints.insert(
                self.chain[j.parent_link],
                self.chain[j.child_link],
                builder,
                true,
            ) {
                self.stages.push(Stage {
                    handle: h,
                    axis_vec: axis,
                    axis: JointAxis::AngX,
                });
            }
        }

        // 5. Per-stage servo config: enable the position motor on the
        //    revolute DoF (rapier leaves `motor_axes` empty — motors are
        //    dead until the axis bit is set), ForceBased torque model and
        //    the effort clamp; plus this joint's rotor inertia (armature)
        //    and viscous friction on its own DoF slice. (At build time
        //    the root still carries a 6-DoF free joint; rapier drops
        //    those rows on the first step, so the slices below are the
        //    only ones that survive — identical to the legacy fill-all.)
        for (s, j) in self.stages.iter().zip(r.joints.iter()) {
            if let Some((mb, link_id)) = self.mbody_joints.get_mut(s.handle) {
                let dofs = mb.link(link_id).map(|link| {
                    let a = link.assembly_id();
                    a..a + link.joint().ndofs()
                });
                if let Some(link) = mb.link_mut(link_id) {
                    link.joint.data.motor_axes.insert(JointAxesMask::ANG_X);
                    link.joint
                        .data
                        .set_motor_model(s.axis, MotorModel::ForceBased);
                    link.joint
                        .data
                        .set_motor_max_force(s.axis, j.effort.unwrap_or(MOTOR_MAX_FORCE));
                }
                if let Some(dofs) = dofs {
                    for k in dofs {
                        if let Some(v) = mb.armature_mut().get_mut(k) {
                            *v = j.armature;
                        }
                        if let Some(v) = mb.damping_mut().get_mut(k) {
                            *v = j.damping;
                        }
                    }
                }
            }
        }

        self.prev_angles = vec![0.0; r.joints.len()];
    }

    /// End-effector (tip) world pose after the last step.
    pub fn ee_pose(&self) -> ([f32; 3], [f32; 4]) {
        self.last_ee
    }

    /// Measured joint angles (rad), read from each link's
    /// `local_to_parent` rotation — the multibody's own generalized
    /// coordinates, in the same sign convention the joint motors use.
    pub fn angles(&self) -> Vec<f32> {
        self.stages
            .iter()
            .map(|s| {
                self.mbody_joints
                    .get(s.handle)
                    .and_then(|(mb, link_id)| mb.link(link_id))
                    .map(|link| angle_about(link.local_to_parent().rotation, s.axis_vec))
                    .unwrap_or(0.0)
            })
            .collect()
    }

    /// Reported hold state.
    pub fn holding(&self) -> bool {
        self.holding
    }

    /// World points of the actuated chain — the skeleton the top-down
    /// camera draws: the origin of every joint's child link **except
    /// the first joint's** (the base-yaw carrier is co-located with the
    /// shoulder), then the tip. For the preset that is `[shoulder,
    /// elbow, wrist, tip]`. Body origins sit exactly on their joint
    /// anchors by construction, so plain translations are the joint
    /// positions.
    pub fn chain_points(&self) -> Vec<[f32; 3]> {
        let origin = |h: RigidBodyHandle| -> [f32; 3] {
            self.bodies
                .get(h)
                .map(|b| {
                    let t = b.translation();
                    [t.x, t.y, t.z]
                })
                .unwrap_or([0.0; 3])
        };
        let mut pts: Vec<[f32; 3]> = self
            .robot
            .joints
            .iter()
            .skip(1)
            .map(|j| origin(self.chain[j.child_link]))
            .collect();
        pts.push(self.last_ee.0);
        pts
    }

    /// Drive the gripper plates (ramped) toward the demand — kinematic
    /// slabs straddling the tip in the attach link's frame. No-op
    /// without a gripper.
    fn drive_fingers(&mut self, gripper: f32) {
        let Some((fl, fr)) = self.fingers else { return };
        let Some(g) = self.robot.gripper.as_ref() else {
            return;
        };
        let (gap_open, gap_closed, rate, finger_x) = (g.gap_open, g.gap_closed, g.gap_rate, g.finger_x);
        let g_target = gap_open + (gap_closed - gap_open) * gripper.clamp(0.0, 1.0);
        self.gap_now += (g_target - self.gap_now).clamp(-rate, rate);
        let (wp, wr) = {
            let b = self.bodies.get(self.ee_body).expect("attach body exists");
            (b.translation(), *b.rotation())
        };
        let centre = wp + wr.mul_vec3(Vec3::new(finger_x, 0.0, 0.0));
        let side = wr.mul_vec3(Vec3::new(0.0, 0.0, self.gap_now * 0.5));
        for (h, sign) in [(fl, 1.0), (fr, -1.0)] {
            if let Some(b) = self.bodies.get_mut(h) {
                b.set_next_kinematic_rotation(wr);
                b.set_next_kinematic_translation(centre + sign * side);
            }
        }
    }

    /// Reporting heuristic: gripper closed + a prop centre inside the
    /// grip zone; the contact solve does the actual holding. Always
    /// false without a gripper.
    fn update_holding(&mut self, gripper: f32) {
        self.holding = self.fingers.is_some() && gripper > 0.5 && {
            let tip = self.last_ee.0;
            self.map.values().any(|h| {
                self.bodies.get(*h).is_some_and(|b| {
                    let t = b.translation();
                    Vec3::new(t.x - tip[0], t.y - tip[1], t.z - tip[2]).length() < GRASP_RADIUS
                })
            })
        };
    }

    /// Complete experiment fingerprint: sim state + solver knobs +
    /// prop physics + arm identity (kp/kd/mass_scale). Two runs with
    /// different servo params that happen to converge to identical
    /// states are NOT the same experiment (sim-engine review: identity
    /// completeness).
    ///
    /// M0a keeps the legacy fold — `joints[0]`'s kp/kd (the preset's
    /// uniform gains) and `mass_scale` — so the default receipt value
    /// is unchanged. Per-joint gain differences, link geometry and the
    /// joint count are NOT folded yet; the canonical spec hash that
    /// covers them is the M0b `fingerprint_scheme` bump.
    pub fn fingerprint(&self, world: &World) -> u64 {
        let r = &self.robot;
        let (kp, kd) = r.joints.first().map_or((0.0, 0.0), |j| (j.kp, j.kd));
        let h = sim_fingerprint(world);
        mix(
            mix(mix(h, kp.to_bits() as u64), kd.to_bits() as u64),
            r.mass_scale.to_bits() as u64,
        )
    }

    /// Contact-inspection probe (temporary): plate world positions and
    /// per-prop contact-pair counts. Plates read `NaN` without a gripper.
    pub fn debug_contact_state(&self) -> (Vec<[f32; 3]>, Vec<usize>) {
        let plate = |h: Option<RigidBodyHandle>| {
            h.and_then(|h| self.bodies.get(h))
                .map(|b| {
                    let t = b.translation();
                    [t.x, t.y, t.z]
                })
                .unwrap_or([f32::NAN; 3])
        };
        let mut counts = Vec::new();
        for h in self.map.values() {
            let mut n = 0usize;
            if let Some(b) = self.bodies.get(*h) {
                for ch in b.colliders() {
                    n += self.narrow_phase.contact_pairs_with(*ch).count();
                }
            }
            counts.push(n);
        }
        let (fl, fr) = match self.fingers {
            Some((l, r)) => (Some(l), Some(r)),
            None => (None, None),
        };
        (vec![plate(fl), plate(fr)], counts)
    }
}

impl System<World> for MultibodySystem {
    fn name(&self) -> &str {
        "rigid3d"
    }

    fn run(&mut self, ctx: &FrameCtx, world: &mut World) {
        let g = world.resource::<Gravity>().map_or(9.8, |g| g.0);
        let scale = world.resource_or_default::<TimeScale>().effective();
        let dt = ctx.dt * scale;

        // 1. Reconcile prop bodies with live Pose3+SimObject entities.
        let live: Vec<(EntKey, Pose3)> = world
            .query2::<Pose3, SimObject>()
            .map(|(e, p, _)| (key(e), *p))
            .collect();
        let live_keys: BTreeSet<EntKey> = live.iter().map(|(k, _)| *k).collect();
        let stale: Vec<EntKey> = self
            .map
            .keys()
            .filter(|k| !live_keys.contains(k))
            .copied()
            .collect();
        for k in stale {
            if let Some(h) = self.map.remove(&k) {
                self.bodies.remove(
                    h,
                    &mut self.islands,
                    &mut self.colliders,
                    &mut self.impulse_joints,
                    &mut self.mbody_joints,
                    &mut self.soft_bodies,
                    true,
                );
            }
            self.last_pose.remove(&k);
        }
        // Optional per-prop physics overrides (domain randomization).
        let phys: BTreeMap<EntKey, PropPhys> = world
            .query::<PropPhys>()
            .map(|(e, pp)| (key(e), *pp))
            .collect();
        for (k, p) in &live {
            if !self.map.contains_key(k) {
                let pp = phys.get(k).copied().unwrap_or_default();
                let b = RigidBodyBuilder::dynamic()
                    .linear_damping(0.1)
                    .angular_damping(0.1)
                    .translation(Vec3::new(p.pos[0], p.pos[1], p.pos[2]))
                    .build();
                let h = self.bodies.insert(b);
                let c = ColliderBuilder::cuboid(PROP_HALF, PROP_HALF, PROP_HALF)
                    .density(pp.density)
                    .friction(pp.friction)
                    .collision_groups(world_groups())
                    .build();
                self.colliders.insert_with_parent(c, h, &mut self.bodies);
                self.map.insert(*k, h);
                self.last_pose.insert(*k, (p.pos, p.rot));
            }
        }

        // 2. External pose writes (Teleport-style) — adopt into bodies.
        for (k, p) in &live {
            let Some(&h) = self.map.get(k) else { continue };
            if self.last_pose.get(k).copied() != Some((p.pos, p.rot)) {
                if let Some(b) = self.bodies.get_mut(h) {
                    b.set_rotation(quat_from(p.rot), true);
                    b.set_translation(Vec3::new(p.pos[0], p.pos[1], p.pos[2]), true);
                }
            }
        }

        // 3. Arm: adopt JointTargets into the joints' position motors
        // (ForceBased PD; unchanged targets skip the motor write).
        // Limits/gains are per joint from the spec; a `velocity_limit`
        // rate-clamps the commanded target by `v·dt` per tick from the
        // previous command (first tick: from the measured angle).
        let mut arm: Option<(EntKey, Vec<f32>, f32)> = None;
        for (e, jt) in world.query::<JointTargets>() {
            arm = Some((key(e), jt.angles.clone(), jt.gripper));
        }
        let mut gripper = 0.0;
        if let Some((_, angles, g)) = &arm {
            gripper = *g;
            for (i, s) in self.stages.iter().enumerate() {
                let Some(&want) = angles.get(i) else { continue };
                let j = &self.robot.joints[i];
                let (lo, hi) = j.limits;
                let mut want = want.clamp(lo, hi);
                if let Some(v) = j.velocity_limit {
                    let prev = self.last_cmd[i];
                    let from = if prev.is_finite() {
                        prev
                    } else {
                        self.prev_angles[i]
                    };
                    let step = v * dt;
                    want = from + (want - from).clamp(-step, step);
                }
                if self.last_cmd.get(i).copied() != Some(want) {
                    if let Some((mb, link_id)) = self.mbody_joints.get_mut(s.handle) {
                        if let Some(link) = mb.link_mut(link_id) {
                            link.joint
                                .data
                                .set_motor_position(s.axis, want, j.kp, j.kd);
                        }
                    }
                    self.last_cmd[i] = want;
                }
            }
        }

        // 3.5 Drive gripper plates (kinematic) for THIS tick's step.
        self.drive_fingers(gripper);

        // 4. Step the pipeline — internal physics substeps per engine
        // tick (decimation; 3D worlds are y-up). The substep count is a
        // live PhysicsParams knob (the dead-constant version made the
        // receipt lie — sim-engine review).
        let gravity = Vec3::new(0.0, -g, 0.0);
        let substeps = self.physics.substeps.unwrap_or(SUBSTEPS).max(1);
        let mut params = IntegrationParameters {
            dt: dt / substeps as f32,
            ..IntegrationParameters::default()
        };
        let p = self.physics;
        if let Some(v) = p.solver_iterations {
            params.num_solver_iterations = v;
        }
        if let Some(v) = p.contact_softness_hz {
            params.contact_softness.natural_frequency = v;
        }
        if let Some(v) = p.warmstart_coefficient {
            params.warmstart_coefficient = v;
        }
        for _ in 0..substeps {
            self.pipeline.step(
                gravity,
                &params,
                &mut self.islands,
                &mut self.broad_phase,
                &mut self.narrow_phase,
                &mut self.bodies,
                &mut self.colliders,
                &mut self.impulse_joints,
                &mut self.mbody_joints,
                &mut self.soft_bodies,
                &mut self.ccd,
                &(),
                &(),
            );
        }

        // 5. End-effector pose from the attach body + grasp bookkeeping.
        let (tip_pos, tip_rot) = {
            let b = self.bodies.get(self.ee_body).expect("attach body exists");
            let q = *b.rotation();
            let off = q.mul_vec3(self.tip_local);
            let t = b.translation();
            ([t.x + off.x, t.y + off.y, t.z + off.z], quat_to_arr(q))
        };
        self.last_ee = (tip_pos, tip_rot);
        self.update_holding(gripper);

        // 6. Write back: joint telemetry into the arm entity, prop
        // poses through a scratch table (one column pass, #100 style).
        let angles = self.angles();
        let vels: Vec<f32> = angles
            .iter()
            .zip(self.prev_angles.iter())
            .map(|(a, p)| (a - p) / dt.max(1e-6))
            .collect();
        self.prev_angles.copy_from_slice(&angles);
        if arm.is_some() {
            let tel = JointTelemetry {
                angles,
                velocities: vels,
                ee_pos: self.last_ee.0,
                ee_rot: self.last_ee.1,
                holding: self.holding,
            };
            world.for_each_mut::<JointTelemetry>(|_, t| {
                *t = tel.clone();
            });
        }
        let mut wb: BTreeMap<EntKey, ([f32; 3], [f32; 4])> = BTreeMap::new();
        for (k, _) in &live {
            let Some(&h) = self.map.get(k) else { continue };
            let Some(b) = self.bodies.get(h) else {
                continue;
            };
            let t = b.translation();
            let pose = ([t.x, t.y, t.z], quat_to_arr(*b.rotation()));
            self.last_pose.insert(*k, pose);
            wb.insert(*k, pose);
        }
        if !wb.is_empty() {
            world.for_each2_mut::<Pose3, SimObject>(|e, p, _| {
                if let Some((pos, rot)) = wb.get(&key(e)) {
                    p.pos = *pos;
                    p.rot = *rot;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::robot::{GripperKind, GripperSpec, JointSpec, LinkSpec};
    use oriera_common::{FrameIndex, Grants, Timestamp};
    use std::f32::consts::PI;

    const DT: f32 = 1.0 / 60.0;

    fn ctx(step: u64) -> FrameCtx<'static> {
        FrameCtx {
            frame: FrameIndex(step),
            dt: DT,
            now: Timestamp(0),
            grants: Grants::EMPTY,
        }
    }

    /// World + arm entity + one prop. Returns (world, sys, arm, prop).
    /// Props spawn at table-rest height 0.55 (the home arm sits at
    /// shoulder height 0.62 — never spawn a prop inside a link).
    fn bench(prop: [f32; 3]) -> (World, MultibodySystem, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        world.insert_resource(TimeScale(1.0));
        let arm = world.spawn();
        world.insert(
            arm,
            JointTargets {
                angles: vec![0.0; 4],
                gripper: 0.0,
            },
        );
        world.insert(arm, JointTelemetry::default());
        let obj = world.spawn();
        world.insert(
            obj,
            Pose3 {
                pos: prop,
                ..Default::default()
            },
        );
        world.insert(obj, SimObject);
        let sys = MultibodySystem::new(ArmDescription::default(), Scene3D::default());
        (world, sys, arm, obj)
    }

    fn pose(world: &World, e: Entity) -> Pose3 {
        *world.get::<Pose3>(e).expect("prop pose")
    }

    fn telemetry(world: &World, e: Entity) -> JointTelemetry {
        world
            .get::<JointTelemetry>(e)
            .expect("arm telemetry")
            .clone()
    }

    #[test]
    fn cube_falls_and_rests_on_the_table() {
        let (mut world, mut sys, _arm, obj) = bench([0.30, 0.55, 0.0]);
        for i in 0..180 {
            sys.run(&ctx(i), &mut world);
        }
        let p = pose(&world, obj);
        // table top y=0.5, cube half 0.025 → rest centre ≈ 0.525
        assert!(
            (p.pos[1] - 0.525).abs() < 0.012,
            "cube must rest on the table, got y={}",
            p.pos[1]
        );
        assert!(
            (p.pos[0] - 0.30).abs() < 0.03 && p.pos[2].abs() < 0.03,
            "cube must not slide away, got {:?}",
            p.pos
        );
    }

    #[test]
    fn joint_targets_drive_the_chain() {
        let (mut world, mut sys, arm, _obj) = bench([0.30, 0.55, 0.0]);
        let start_ee = sys.ee_pose();
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.angles = vec![0.0, 0.9, -0.5, 0.0];
        }
        for i in 0..150 {
            sys.run(&ctx(i), &mut world);
        }
        let tel = telemetry(&world, arm);
        assert!(
            (tel.angles[1] - 0.9).abs() < 0.08,
            "shoulder must reach its target, got {}",
            tel.angles[1]
        );
        assert!(
            (tel.angles[2] - (-0.5)).abs() < 0.08,
            "elbow must reach its target, got {}",
            tel.angles[2]
        );
        assert!(
            tel.ee_pos[1] > start_ee.0[1] + 0.02,
            "tip must rise with the shoulder, {} → {}",
            start_ee.0[1],
            tel.ee_pos[1]
        );
        assert!(tel.velocities.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn two_runs_are_bit_identical() {
        let script = |sys: &mut MultibodySystem, world: &mut World| {
            let mut out = Vec::new();
            for i in 0..240 {
                if i % 30 == 0 {
                    let a = (i as f32) * 0.004;
                    let arm_ent = world.query::<JointTargets>().next().map(|(e, _)| e);
                    if let Some(arm) = arm_ent {
                        let t = [a, 0.5 + a, -0.8 - a, 0.2];
                        if let Some(tg) = world.get_mut::<JointTargets>(arm) {
                            tg.angles = t.to_vec();
                            tg.gripper = if (i / 30) % 2 == 1 { 1.0 } else { 0.0 };
                        }
                    }
                }
                sys.run(&ctx(i), world);
                let tel = world
                    .query::<JointTelemetry>()
                    .next()
                    .map(|(_, t)| t.clone())
                    .unwrap();
                let obj = world
                    .query2::<Pose3, SimObject>()
                    .next()
                    .map(|(_, p, _)| *p)
                    .unwrap();
                out.push((tel, obj, sim_fingerprint(world)));
            }
            out
        };
        let (mut w1, mut s1, _, _) = bench([0.30, 0.55, 0.0]);
        let (mut w2, mut s2, _, _) = bench([0.30, 0.55, 0.0]);
        let r1 = script(&mut s1, &mut w1);
        let r2 = script(&mut s2, &mut w2);
        assert_eq!(r1, r2, "#66+#67 at sim granularity: runs must be bit-equal");
    }

    #[test]
    fn grasp_carries_and_release_drops() {
        // Contact grasp (round 4, probe-verified mechanism): descend so
        // the TIP reaches the cube CENTRE (full plate-face overlap, the
        // exact minimal-probe configuration), close, lift straight up
        // via IK, open. The cube rides on plate/cube friction.
        let (mut world, mut sys, arm, obj) = bench([0.30, 0.55, 0.0]);
        let ik = |target: [f32; 3]| -> [f32; 4] {
            let d = ArmDescription::default();
            let (l1, l2) = (d.links[0], d.links[1]);
            let sh = d.base[1] + d.shoulder_height;
            let dx = target[0] - d.base[0];
            let dz = target[2] - d.base[2];
            let yaw = f32::atan2(-dz, dx);
            let r = (dx * dx + dz * dz).sqrt().clamp(0.06, l1 + l2 - 0.005);
            let h = (target[1] + d.links[2] - sh).clamp(-0.38, 0.30);
            let ce = ((r * r + h * h - l1 * l1 - l2 * l2) / (2.0 * l1 * l2)).clamp(-1.0, 1.0);
            let el = -ce.acos();
            let sh2 = f32::atan2(h, r) - f32::atan2(l2 * el.sin(), l1 + l2 * el.cos());
            let wr = -std::f32::consts::FRAC_PI_2 - sh2 - el;
            [
                yaw.clamp(d.limits[0].0, d.limits[0].1),
                sh2.clamp(d.limits[1].0, d.limits[1].1),
                el.clamp(d.limits[2].0, d.limits[2].1),
                wr.clamp(d.limits[3].0, d.limits[3].1),
            ]
        };
        let cube = [0.30f32, 0.55, 0.0];
        let cy = 0.525; // settled cube-centre height on the table
        let phases: &[([f32; 3], u64, f32)] = &[
            ([cube[0], cy + 0.10, cube[2]], 90, 0.0),
            ([cube[0], cy + 0.025, cube[2]], 90, 0.0),
            ([cube[0], cy + 0.025, cube[2]], 60, 1.0),
            ([cube[0], cy + 0.145, cube[2]], 90, 1.0),
            ([cube[0], cy + 0.025, cube[2]], 60, 0.0),
        ];
        let mut i = 0u64;
        let mut carried = 0.0f32;
        for (target, ticks, grip) in phases {
            let angles = ik(*target);
            for _ in 0..*ticks {
                if let Some(t) = world.get_mut::<JointTargets>(arm) {
                    t.angles = angles.to_vec();
                    t.gripper = *grip;
                }
                sys.run(&ctx(i), &mut world);
                i += 1;
            }
            if *grip > 0.5 {
                carried = pose(&world, obj).pos[1];
                let (plates, counts) = sys.debug_contact_state();
                eprintln!(
                    "PROBE grip@i={i}: carried={carried:.3} contacts={counts:?} plates={plates:?} cube={:?} hold={}",
                    pose(&world, obj).pos,
                    telemetry(&world, arm).holding
                );
            }
        }
        assert!(
            carried > 0.55 + 0.04,
            "cube must ride the gripper on friction (carried {carried:.3})"
        );
        let dropped = pose(&world, obj).pos[1];
        assert!(
            dropped < carried - 0.02,
            "cube must fall after release: {carried:.3} → {dropped:.3}"
        );
    }

    #[test]
    fn external_pose_writes_are_adopted() {
        let (mut world, mut sys, _arm, obj) = bench([0.30, 0.55, 0.0]);
        for i in 0..60 {
            sys.run(&ctx(i), &mut world);
        }
        let teleport = Pose3 {
            pos: [0.40, 0.70, 0.10],
            rot: [0.0, 0.0, 0.0, 1.0],
        };
        if let Some(p) = world.get_mut::<Pose3>(obj) {
            *p = teleport;
        }
        sys.run(&ctx(60), &mut world);
        sys.run(&ctx(61), &mut world);
        let p = pose(&world, obj);
        assert!(
            (p.pos[0] - 0.40).abs() < 0.02 && (p.pos[2] - 0.10).abs() < 0.02,
            "teleport must be adopted, got {:?}",
            p.pos
        );
    }

    #[test]
    fn fingerprint_tracks_state_and_identity() {
        let (w1, _s1, _, _) = bench([0.30, 0.55, 0.0]);
        let (w2, _s2, _, _) = bench([0.30, 0.55, 0.0]);
        let (w3, _s3, _, _) = bench([0.31, 0.55, 0.0]);
        assert_eq!(
            sim_fingerprint(&w1),
            sim_fingerprint(&w2),
            "same state ⇒ same hash"
        );
        assert_ne!(
            sim_fingerprint(&w1),
            sim_fingerprint(&w3),
            "one float ⇒ new hash"
        );
    }

    // --- M0a spec-driven generalization ---------------------------------

    /// THE "no behaviour change" guard: the default path
    /// (`ArmDescription::default()`) must reproduce the pre-M0a engine
    /// bit for bit. The constants below were captured from the legacy
    /// builder **before this rewrite** (a probe test folding the same
    /// two scripts over every telemetry float, the prop pose and the
    /// per-tick `sim_fingerprint`; PLAN-embodied-general M0a 工作包 A):
    /// (A) 240-tick swing + gripper-toggle script through
    ///     `MultibodySystem::new` — plus the same script through
    ///     `from_robot(RobotSpec::from(&desc))` must match it exactly.
    /// (B) 390-tick IK grasp → lift → release script — the contact-
    ///     friction carry path, the most sensitive part of the sim.
    #[test]
    fn golden_default_path_is_bit_identical_to_pre_m0a() {
        // Script A: swing + gripper toggle.
        const A_FOLD: u64 = 0x72cf_b4ee_ecd9_8dd9;
        const A_CPS: [u64; 4] = [
            0xf169_cfc9_6935_32ed,
            0x03de_e8cf_5e6e_c5f5,
            0x9d84_5604_8e18_811b,
            0xf837_a167_7ec9_b712,
        ];
        const A_SYSFP: u64 = 0x4438_f5c4_dcf1_1942;
        const A_ANGLE_BITS: [u32; 4] = [0x3f57_0a37, 0x3fa6_6b99, 0xbfd5_3b21, 0x3e49_6398];
        const A_EE_BITS: [u32; 3] = [0x3e68_8d8c, 0x3f3b_b395, 0xbe6c_9863];
        // Script B: IK grasp/lift/release (per-phase checkpoint fps).
        const B_FOLD: u64 = 0x5699_d804_97fb_9d3d;
        const B_CPS: [u64; 5] = [
            0xb9de_19f0_c174_f7b8,
            0x007e_3aea_ac69_1beb,
            0x8483_e4a8_c1e8_30b5,
            0x413e_cbd7_36cf_7380,
            0x31ce_ede3_6287_0ed8,
        ];
        const B_SYSFP: u64 = 0x1efb_5258_7556_7771;
        const B_OBJ_BITS: [u32; 3] = [0x3ea3_c7ca, 0x3f06_191e, 0x3b7a_4eb0];

        // (A) the 240-tick swing script through `new` …
        let (mut world, mut sys, arm, _obj) = bench([0.30, 0.55, 0.0]);
        let script_a = |i: u64, w: &mut World| {
            if i.is_multiple_of(30) {
                let a = (i as f32) * 0.004;
                if let Some(tg) = w.get_mut::<JointTargets>(arm) {
                    tg.angles = vec![a, 0.5 + a, -0.8 - a, 0.2];
                    tg.gripper = if (i / 30) % 2 == 1 { 1.0 } else { 0.0 };
                }
            }
        };
        let (ha, cps_a) = fold_episode(&mut sys, &mut world, 240, &script_a);
        assert_eq!(hex(ha), hex(A_FOLD), "script A fold diverged (new path)");
        assert_eq!(
            cps_a.iter().map(|c| hex(*c)).collect::<Vec<_>>(),
            A_CPS.iter().map(|c| hex(*c)).collect::<Vec<_>>(),
            "script A checkpoint fingerprints diverged"
        );
        assert_eq!(
            hex(sys.fingerprint(&world)),
            hex(A_SYSFP),
            "script A system fingerprint diverged"
        );
        let tel = telemetry(&world, arm);
        assert_eq!(
            tel.angles.iter().map(|a| fbits(*a)).collect::<Vec<_>>(),
            A_ANGLE_BITS.to_vec(),
            "script A final angles diverged"
        );
        assert_eq!(
            tel.ee_pos.iter().map(|a| fbits(*a)).collect::<Vec<_>>(),
            A_EE_BITS.to_vec(),
            "script A final EE diverged"
        );

        // (A′) … and through `from_robot` — the generic entry point must
        // be trajectory-identical to `new` (the preset projection IS the
        // legacy builder).
        let (mut world2, mut sys2, arm2, _obj2) = bench_from_spec([0.30, 0.55, 0.0]);
        let script_a2 = |i: u64, w: &mut World| {
            if i.is_multiple_of(30) {
                let a = (i as f32) * 0.004;
                if let Some(tg) = w.get_mut::<JointTargets>(arm2) {
                    tg.angles = vec![a, 0.5 + a, -0.8 - a, 0.2];
                    tg.gripper = if (i / 30) % 2 == 1 { 1.0 } else { 0.0 };
                }
            }
        };
        let (ha2, cps_a2) = fold_episode(&mut sys2, &mut world2, 240, &script_a2);
        assert_eq!(
            (hex(ha2), hexs(&cps_a2)),
            (hex(ha), hexs(&cps_a)),
            "new ≠ from_robot(preset)"
        );
        assert_eq!(hex(sys2.fingerprint(&world2)), hex(A_SYSFP));

        // (B) the 390-tick grasp/lift/release script, folded continuously
        // exactly as captured (one seed-carrying fold across all five
        // phases, checkpoint after each phase).
        let cube = [0.30f32, 0.55, 0.0];
        let cy = 0.525f32; // settled cube-centre height on the table
        let phases: &[([f32; 3], u64, f32)] = &[
            ([cube[0], cy + 0.10, cube[2]], 90, 0.0),
            ([cube[0], cy + 0.025, cube[2]], 90, 0.0),
            ([cube[0], cy + 0.025, cube[2]], 60, 1.0),
            ([cube[0], cy + 0.145, cube[2]], 90, 1.0),
            ([cube[0], cy + 0.025, cube[2]], 60, 0.0),
        ];
        let ik = ik_fn();
        let (mut world, mut sys, arm, obj) = bench([0.30, 0.55, 0.0]);
        // One fold step over the full state + receipt (the probe's fold).
        let fold_step = |seed: u64, world: &World| -> u64 {
            let t = world
                .query::<JointTelemetry>()
                .next()
                .map(|(_, t)| t.clone())
                .unwrap();
            let o = world
                .query2::<Pose3, SimObject>()
                .next()
                .map(|(_, p, _)| *p)
                .unwrap();
            let mut h = seed;
            for a in t.angles.iter().chain(t.velocities.iter()) {
                h = bits(h, *a);
            }
            for v in t.ee_pos.iter().chain(t.ee_rot.iter()) {
                h = bits(h, *v);
            }
            h = mix(h, t.holding as u64);
            for v in o.pos.iter().chain(o.rot.iter()) {
                h = bits(h, *v);
            }
            mix(h, sim_fingerprint(world))
        };
        let mut h_b = 0u64;
        let mut i_b = 0u64;
        let mut phase = 0usize;
        for (target, ticks, grip) in phases {
            let angles = ik(*target);
            for _ in 0..*ticks {
                if let Some(t) = world.get_mut::<JointTargets>(arm) {
                    t.angles = angles.to_vec();
                    t.gripper = *grip;
                }
                sys.run(&ctx(i_b), &mut world);
                i_b += 1;
                h_b = fold_step(h_b, &world);
            }
            assert_eq!(
                hex(sim_fingerprint(&world)),
                hex(B_CPS[phase]),
                "script B phase {phase} checkpoint diverged"
            );
            phase += 1;
        }
        assert_eq!(phase, 5);
        assert_eq!(i_b, 390);
        assert_eq!(hex(h_b), hex(B_FOLD), "script B fold diverged (new path)");
        assert_eq!(
            hex(sys.fingerprint(&world)),
            hex(B_SYSFP),
            "script B system fingerprint diverged"
        );
        let obj_now = pose(&world, obj);
        assert_eq!(
            obj_now.pos.iter().map(|a| fbits(*a)).collect::<Vec<_>>(),
            B_OBJ_BITS.to_vec(),
            "script B final cube pose diverged"
        );
    }

    /// Rolling bit-fold of every telemetry/prop value plus the per-tick
    /// `sim_fingerprint`, over a whole scripted episode; a checkpoint
    /// `sim_fingerprint` is sampled every 60 ticks.
    fn fold_episode(
        sys: &mut MultibodySystem,
        world: &mut World,
        ticks: u64,
        script: &dyn Fn(u64, &mut World),
    ) -> (u64, Vec<u64>) {
        let mut h = 0u64;
        let mut cps = Vec::new();
        for i in 0..ticks {
            script(i, world);
            sys.run(&ctx(i), world);
            let tel = world
                .query::<JointTelemetry>()
                .next()
                .map(|(_, t)| t.clone())
                .unwrap();
            let obj = world
                .query2::<Pose3, SimObject>()
                .next()
                .map(|(_, p, _)| *p)
                .unwrap();
            for a in tel.angles.iter().chain(tel.velocities.iter()) {
                h = bits(h, *a);
            }
            for v in tel.ee_pos.iter().chain(tel.ee_rot.iter()) {
                h = bits(h, *v);
            }
            h = mix(h, tel.holding as u64);
            for v in obj.pos.iter().chain(obj.rot.iter()) {
                h = bits(h, *v);
            }
            h = mix(h, sim_fingerprint(world));
            if (i + 1) % 60 == 0 {
                cps.push(sim_fingerprint(world));
            }
        }
        (h, cps)
    }

    fn hex(v: u64) -> String {
        format!("0x{v:016x}")
    }

    fn hexs(v: &[u64]) -> Vec<String> {
        v.iter().map(|x| hex(*x)).collect()
    }

    fn fbits(v: f32) -> u32 {
        v.to_bits()
    }

    /// Benchmark fixture built through the generic entry point
    /// (`from_robot` on the preset projection).
    fn bench_from_spec(prop: [f32; 3]) -> (World, MultibodySystem, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        world.insert_resource(TimeScale(1.0));
        let arm = world.spawn();
        world.insert(
            arm,
            JointTargets {
                angles: vec![0.0; 4],
                gripper: 0.0,
            },
        );
        world.insert(arm, JointTelemetry::default());
        let obj = world.spawn();
        world.insert(
            obj,
            Pose3 {
                pos: prop,
                ..Default::default()
            },
        );
        world.insert(obj, SimObject);
        let spec = RobotSpec::from(&ArmDescription::default());
        let sys = MultibodySystem::from_robot(spec, Scene3D::default()).expect("preset builds");
        (world, sys, arm, obj)
    }

    /// The legacy IK used by the grasp tests/golden (default arm).
    fn ik_fn() -> impl Fn([f32; 3]) -> [f32; 4] {
        |target: [f32; 3]| {
            let d = ArmDescription::default();
            let (l1, l2) = (d.links[0], d.links[1]);
            let sh = d.base[1] + d.shoulder_height;
            let dx = target[0] - d.base[0];
            let dz = target[2] - d.base[2];
            let yaw = f32::atan2(-dz, dx);
            let r = (dx * dx + dz * dz).sqrt().clamp(0.06, l1 + l2 - 0.005);
            let h = (target[1] + d.links[2] - sh).clamp(-0.38, 0.30);
            let ce = ((r * r + h * h - l1 * l1 - l2 * l2) / (2.0 * l1 * l2)).clamp(-1.0, 1.0);
            let el = -ce.acos();
            let sh2 = f32::atan2(h, r) - f32::atan2(l2 * el.sin(), l1 + l2 * el.cos());
            let wr = -std::f32::consts::FRAC_PI_2 - sh2 - el;
            [
                yaw.clamp(d.limits[0].0, d.limits[0].1),
                sh2.clamp(d.limits[1].0, d.limits[1].1),
                el.clamp(d.limits[2].0, d.limits[2].1),
                wr.clamp(d.limits[3].0, d.limits[3].1),
            ]
        }
    }

    /// Synthetic 6-axis arm (base yaw Y + shoulder/elbow/pitch Z +
    /// roll X + distal yaw Y), gripper on the flange. Every origin is
    /// consistent at home (the chain stretches along +X at shoulder
    /// height), anchors chain the links in sequence.
    fn six_axis_spec() -> RobotSpec {
        let sx = 0.02f32;
        let sy = 0.62f32; // 0.5 base y + 0.12 shoulder height
        let base_h = 0.06f32;
        let r = 0.02f32;
        let link = |name: &str, half: [f32; 3], origin: [f32; 3], off: [f32; 3]| LinkSpec {
            name: name.into(),
            shape: LinkShape::Box { half },
            origin,
            collider_offset: off,
            density: LINK_DENSITY,
            friction: 0.7,
            added_mass: LINK_ADDED_MASS,
            mesh_uri: None,
        };
        let links = vec![
            LinkSpec {
                name: "base".into(),
                shape: LinkShape::Box {
                    half: [0.04, base_h, 0.04],
                },
                origin: [sx, 0.5 + base_h, 0.0],
                collider_offset: [0.0; 3],
                density: 1.0,
                friction: 0.5,
                added_mass: 0.0,
                mesh_uri: None,
            },
            link("carrier", [0.015; 3], [sx, sy, 0.0], [0.0; 3]),
            link("upper_arm", [0.1, r, r], [sx, sy, 0.0], [0.1, 0.0, 0.0]),
            link("forearm", [0.09, r, r], [sx + 0.2, sy, 0.0], [0.09, 0.0, 0.0]),
            link("wrist", [0.03, r, r], [sx + 0.38, sy, 0.0], [0.03, 0.0, 0.0]),
            link("roll", [0.02, r, r], [sx + 0.44, sy, 0.0], [0.02, 0.0, 0.0]),
            link("flange", [0.015, r, r], [sx + 0.48, sy, 0.0], [0.015, 0.0, 0.0]),
        ];
        let j = |name: &str, p: usize, c: usize, anchor: [f32; 3], axis: [f32; 3], limits: (f32, f32), kp: f32, kd: f32| {
            JointSpec {
                name: name.into(),
                parent_link: p,
                child_link: c,
                anchor,
                axis,
                limits,
                effort: None,
                velocity_limit: None,
                kp,
                kd,
                armature: ARMATURE,
                damping: JOINT_DAMPING,
            }
        };
        let joints = vec![
            j("base_yaw", 0, 1, [0.0, 0.12 - base_h, 0.0], [0.0, 1.0, 0.0], (-PI, PI), 60.0, 4.0),
            j("shoulder", 1, 2, [0.0; 3], [0.0, 0.0, 1.0], (-0.35, 1.45), 60.0, 4.0),
            j("elbow", 2, 3, [0.2, 0.0, 0.0], [0.0, 0.0, 1.0], (-2.5, 0.35), 50.0, 3.0),
            j("wrist_pitch", 3, 4, [0.18, 0.0, 0.0], [0.0, 0.0, 1.0], (-2.0, 2.0), 30.0, 2.0),
            j("wrist_roll", 4, 5, [0.06, 0.0, 0.0], [1.0, 0.0, 0.0], (-PI, PI), 25.0, 2.0),
            j("wrist_yaw", 5, 6, [0.04, 0.0, 0.0], [0.0, 1.0, 0.0], (-2.0, 2.0), 25.0, 2.0),
        ];
        let gripper = GripperSpec {
            attach_link: 6,
            finger_x: 0.05,
            tcp_offset: [0.03, 0.0, 0.0],
            finger_half: [0.03, 0.045, 0.006],
            gap_open: 0.075,
            gap_closed: 0.046,
            friction: 1.4,
            effort: GRIPPER_EFFORT,
            gap_rate: FINGER_GAP_RATE,
            kind: GripperKind::ParallelPlates,
        };
        RobotSpec {
            name: "tabletop-6dof".into(),
            base: [sx, 0.5, 0.0],
            links,
            joints,
            gripper: Some(gripper),
            home: vec![0.0; 6],
            mass_scale: 1.0,
            action_scale: vec![2.0; 6],
        }
    }

    /// N-level generalization: a 6-axis spec builds, settles, and its
    /// motors track per-joint targets with bounded error; telemetry,
    /// skeleton and receipt all scale with N.
    #[test]
    fn six_axis_spec_settles_and_tracks_targets() {
        let spec = six_axis_spec();
        spec.validate().expect("6-axis spec is valid");
        let n = spec.dof();
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        world.insert_resource(TimeScale(1.0));
        let arm = world.spawn();
        world.insert(
            arm,
            JointTargets {
                angles: vec![0.0; n],
                gripper: 0.0,
            },
        );
        world.insert(arm, JointTelemetry::default());
        let mut sys = MultibodySystem::from_robot(spec, Scene3D::default()).expect("builds");

        // Settle at home.
        for i in 0..120u64 {
            sys.run(&ctx(i), &mut world);
        }
        let tel = telemetry(&world, arm);
        assert_eq!(tel.angles.len(), n, "telemetry scales with N");
        assert_eq!(tel.velocities.len(), n);
        let pts = sys.chain_points();
        assert_eq!(pts.len(), n, "skeleton: (N-1) joint origins + tip");
        assert!(pts.iter().all(|p| p.iter().all(|v| v.is_finite())));

        // Track a full-pose target.
        let target = [0.3f32, 0.6, -0.9, 0.4, 0.8, -0.5];
        for i in 120..400u64 {
            if let Some(t) = world.get_mut::<JointTargets>(arm) {
                t.angles = target.to_vec();
                t.gripper = 0.0;
            }
            sys.run(&ctx(i), &mut world);
        }
        let tel = telemetry(&world, arm);
        for (k, (got, want)) in tel.angles.iter().zip(target).enumerate() {
            assert!(
                (got - want).abs() < 0.1,
                "joint {k} must track its target: got {got}, want {want}"
            );
        }
        assert!(tel.velocities.iter().all(|v| v.is_finite()));
        assert!(tel.ee_pos.iter().all(|v| v.is_finite()));
    }

    /// `velocity_limit`: Some(v) rate-clamps the commanded target by
    /// `v·dt` per tick; None (the default path) is unlimited.
    #[test]
    fn velocity_limit_rate_clamps_the_command() {
        let mut spec = RobotSpec::from(&ArmDescription::default());
        let v = 0.5f32; // rad/s
        spec.joints[1].velocity_limit = Some(v);
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        world.insert_resource(TimeScale(1.0));
        let arm = world.spawn();
        world.insert(
            arm,
            JointTargets {
                angles: vec![0.0; 4],
                gripper: 0.0,
            },
        );
        world.insert(arm, JointTelemetry::default());
        let mut sys = MultibodySystem::from_robot(spec, Scene3D::default()).expect("builds");

        // Command the shoulder to 1.0 rad; the clamp is v·dt per tick.
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.angles = vec![0.0, 1.0, 0.0, 0.0];
        }
        let mut measured = Vec::new();
        for i in 0..30u64 {
            sys.run(&ctx(i), &mut world);
            measured.push(telemetry(&world, arm).angles[1]);
        }
        let cmd_after_30 = sys.last_cmd[1];
        let bound = 30.0 * v * DT;
        assert!(
            cmd_after_30 <= bound + 1e-5,
            "commanded target must be rate-clamped: {} > {bound}",
            cmd_after_30
        );
        assert!(
            (cmd_after_30 - bound).abs() < 2e-4,
            "clamp must saturate: got {cmd_after_30}, bound {bound}"
        );
        assert!(
            measured[29] < bound + 0.1,
            "measured angle follows the clamped command: {}",
            measured[29]
        );

        // And it converges once the tick budget adds up.
        for i in 30..600u64 {
            sys.run(&ctx(i), &mut world);
        }
        assert!(
            (sys.last_cmd[1] - 1.0).abs() < 1e-4,
            "rate-limited command converges to the target, got {}",
            sys.last_cmd[1]
        );
        let tel = telemetry(&world, arm);
        assert!(
            (tel.angles[1] - 1.0).abs() < 0.1,
            "rate-limited motor still reaches the target: {}",
            tel.angles[1]
        );

        // The unlimited path (velocity_limit None, bit-pinned by the
        // golden test) commands much faster: at tick 30 it is far beyond
        // the clamped command.
        let (mut w2, mut s2, arm2, _o) = bench([0.30, 0.55, 0.0]);
        if let Some(t) = w2.get_mut::<JointTargets>(arm2) {
            t.angles = vec![0.0, 1.0, 0.0, 0.0];
        }
        for i in 0..30u64 {
            s2.run(&ctx(i), &mut w2);
        }
        assert!(
            s2.last_cmd[1] > cmd_after_30 + 0.3,
            "None must not clamp: {} vs {}",
            s2.last_cmd[1],
            cmd_after_30
        );
    }

    /// GripperSpec geometry drives plate placement: non-default
    /// `finger_x` / `gap_open` / `gap_closed` land the plates per the
    /// spec, not the legacy constants.
    #[test]
    fn gripper_spec_places_the_plates() {
        let mut spec = RobotSpec::from(&ArmDescription::default());
        let finger_x = 0.09f32;
        let gap_open = 0.10f32;
        let gap_closed = 0.05f32;
        {
            let g = spec.gripper.as_mut().unwrap();
            g.finger_x = finger_x;
            g.gap_open = gap_open;
            g.gap_closed = gap_closed;
        }
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        world.insert_resource(TimeScale(1.0));
        let arm = world.spawn();
        world.insert(
            arm,
            JointTargets {
                angles: vec![0.0; 4],
                gripper: 0.0,
            },
        );
        world.insert(arm, JointTelemetry::default());
        let mut sys = MultibodySystem::from_robot(spec, Scene3D::default()).expect("builds");

        for i in 0..120u64 {
            sys.run(&ctx(i), &mut world);
        }
        // Settled at demand 0 ⇒ gap == gap_open; the plates hang at the
        // one-tick-lagged wrist pose, so tolerances cover the sag.
        let (plates, _counts) = sys.debug_contact_state();
        let gap = (plates[0][2] - plates[1][2]).abs();
        assert!(
            (gap - gap_open).abs() < 1e-3,
            "open gap must follow the spec: got {gap}, want {gap_open}"
        );
        let b = sys.bodies.get(sys.ee_body).expect("attach body");
        let centre = b.translation() + b.rotation().mul_vec3(Vec3::new(finger_x, 0.0, 0.0));
        let mid = Vec3::new(
            (plates[0][0] + plates[1][0]) * 0.5,
            (plates[0][1] + plates[1][1]) * 0.5,
            (plates[0][2] + plates[1][2]) * 0.5,
        );
        assert!(
            (mid - centre).length() < 2e-3,
            "plate centre must sit at finger_x along the attach link: {mid:?} vs {centre:?}"
        );

        // Close: the ramp drives the gap to gap_closed.
        for i in 120..240u64 {
            if let Some(t) = world.get_mut::<JointTargets>(arm) {
                t.gripper = 1.0;
            }
            sys.run(&ctx(i), &mut world);
        }
        let (plates, _) = sys.debug_contact_state();
        let gap = (plates[0][2] - plates[1][2]).abs();
        assert!(
            (gap - gap_closed).abs() < 1e-3,
            "closed gap must follow the spec: got {gap}, want {gap_closed}"
        );
    }

    /// A registered `mesh_uri` is an honest build error pointing at M2,
    /// never a silently dropped collider.
    #[test]
    fn mesh_uri_build_fails_with_m2_pointer() {
        let mut spec = RobotSpec::from(&ArmDescription::default());
        spec.links[2].mesh_uri = Some("file:///panda/link2.stl".into());
        let err = match MultibodySystem::from_robot(spec, Scene3D::default()) {
            Err(e) => e,
            Ok(_) => panic!("mesh collider must be refused in M0a"),
        };
        let msg = err.to_string();
        assert!(msg.contains("M2"), "error must point at M2: {msg}");
        assert!(msg.contains("mesh"), "error must name the cause: {msg}");

        // And an unchanged spec rebuilds fine.
        let clean = RobotSpec::from(&ArmDescription::default());
        assert!(MultibodySystem::from_robot(clean, Scene3D::default()).is_ok());
    }

    /// Gripper-less robot: builds, no plates, holding stays false, and
    /// the EE frame rides the last joint's child at its origin.
    #[test]
    fn gripperless_spec_has_no_plates_and_tip_at_last_link() {
        let mut spec = RobotSpec::from(&ArmDescription::default());
        spec.gripper = None;
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        world.insert_resource(TimeScale(1.0));
        let arm = world.spawn();
        world.insert(
            arm,
            JointTargets {
                angles: vec![0.0; 4],
                gripper: 0.0,
            },
        );
        world.insert(arm, JointTelemetry::default());
        let mut sys = MultibodySystem::from_robot(spec, Scene3D::default()).expect("builds");
        for i in 0..60u64 {
            sys.run(&ctx(i), &mut world);
        }
        let tel = telemetry(&world, arm);
        assert!(!tel.holding, "no gripper ⇒ never holding");
        // EE = the attach body origin (tip_local zero).
        let b = sys.bodies.get(sys.ee_body).expect("ee body");
        let t = b.translation();
        assert!(
            (tel.ee_pos[0] - t.x).abs() < 1e-5
                && (tel.ee_pos[1] - t.y).abs() < 1e-5
                && (tel.ee_pos[2] - t.z).abs() < 1e-5,
            "EE must be the attach body origin without a gripper"
        );
        let (plates, _) = sys.debug_contact_state();
        assert!(plates.iter().all(|p| p.iter().all(|v| v.is_nan())));
    }
}
