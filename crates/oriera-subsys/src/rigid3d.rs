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
//! **Honest simplification (M0)**: the chain actuates 4 distinct
//! revolute stages — base yaw, shoulder, elbow, wrist pitch
//! (`JOINT_NAMES[0..4]`). `JointTargets` still accepts 5 channels and
//! the 5th (`wrist_roll`) is recorded in telemetry but does not
//! actuate physics yet; a dedicated roll body is the M1 refinement.

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

/// The driven joint names, in `JointTargets`/`JointTelemetry` order.
/// Index 4 (`wrist_roll`) is telemetry-only in M0 — see module docs.
pub const JOINT_NAMES: [&str; 5] = ["base_yaw", "shoulder", "elbow", "wrist_pitch", "wrist_roll"];

/// Static description of the arm: 4 actuated revolute stages + gripper.
/// Lengths in metres. The home pose stretches along world +X at
/// shoulder height (all angles zero).
#[derive(Debug, Clone)]
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
                (-2.10, 0.35),
                (-2.00, 2.00),
            ],
            // ForceBased motor gains (N·m/rad, N·m·s/rad): the joint
            // motor's stiffness is a real torque gain in this model.
            // AccelerationBased (the default) would make the same
            // numbers angular-acceleration gains — far too soft against
            // a kilogram-scale arm's gravity torque.
            kp: 60.0,
            kd: 4.0,
        }
    }
}

/// Static furniture built once at construction: a ground slab and an
/// optional table, each `(center, half_extents)` in metres.
#[derive(Debug, Clone)]
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

/// Geometric grasp radius: closing the gripper attaches a prop whose
/// centre lies within this distance of the end-effector tip.
pub const GRASP_RADIUS: f32 = 0.06;

/// Prop half-extent (a 5 cm cube) — one size for M0; per-entity shapes
/// arrive with per-entity asset binding (M1).
pub const PROP_HALF: f32 = 0.025;

/// Internal physics substeps per engine tick (the IsaacLab decimation
/// idea: high-frequency physics under a lower-frequency decision loop).
/// Two substeps of dt/2 keep the coupled chain's PD loop stable.
const SUBSTEPS: u32 = 2;
/// Rotor inertia added to every joint DoF — tames PD oscillation on
/// light links (a standard motor model parameter, not a fudge).
const ARMATURE: f32 = 0.02;
/// Passive joint friction (viscous) per DoF.
const JOINT_DAMPING: f32 = 0.05;
/// PD torque clamp per stage (N·m) — a stalled arm must not inject
/// unbounded impulses.
const MOTOR_MAX_FORCE: f32 = 80.0;
/// Link collider density (kg/m³): default density 1.0 makes gram-level
/// links that sit far outside the solver's tuned mass range (rapier's
/// scale discipline: realistic sizes AND masses). ~0.6 kg per link.
const LINK_DENSITY: f32 = 1500.0;
/// Added mass per chain body (kg): keeps even the tiny yaw-carrier
/// stage inside the solver's healthy mass range.
const LINK_ADDED_MASS: f32 = 0.2;

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

/// Build-time spec of one stage: parent, child, anchor (parent-local),
/// revolute axis (parent-local) and limits.
type StageSpec = (RigidBodyHandle, RigidBodyHandle, Vec3, Vec3, (f32, f32));

/// One actuated revolute stage: its multibody joint handle, the
/// revolute axis in the parent's local frame (angle readout) and the
/// generic-joint axis slot the motor drives.
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
pub fn sim_fingerprint(world: &World) -> u64 {
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
    pub desc: ArmDescription,
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
    /// Actuated chain bodies: carrier (yaw stage), link1, link2, wrist.
    chain: Vec<RigidBodyHandle>,
    /// One stage per actuated revolute DoF (4).
    stages: Vec<Stage>,
    /// Tip offset in the wrist body's local frame.
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
    /// Geometric grasp state: the prop currently riding the wrist.
    holding: Option<EntKey>,
    /// Carry offset of the held prop in the tip frame (it hangs just
    /// below the tip — see `run_grasp`).
    hold_offset: Vec3,
    /// End-effector pose after the last step.
    last_ee: ([f32; 3], [f32; 4]),
}

impl MultibodySystem {
    pub fn new(desc: ArmDescription, scene: Scene3D) -> Self {
        let mut sys = Self {
            desc,
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
            tip_local: Vec3::ZERO,
            map: BTreeMap::new(),
            last_pose: BTreeMap::new(),
            prev_angles: vec![0.0; 4],
            last_cmd: vec![f32::NAN; 4],
            holding: None,
            hold_offset: Vec3::ZERO,
            last_ee: ([0.0; 3], [0.0, 0.0, 0.0, 1.0]),
        };
        sys.build_scene(&scene);
        sys.build_arm();
        sys
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

    fn build_arm(&mut self) {
        let d = self.desc.clone();
        let shoulder_y = d.base[1] + d.shoulder_height;
        let r = d.link_radius;
        let base_h = d.shoulder_height * 0.5;

        // Fixed base column (a stub the chain stands on).
        let base = self.bodies.insert(
            RigidBodyBuilder::fixed()
                .translation(Vec3::new(d.base[0], d.base[1] + base_h, d.base[2]))
                .build(),
        );
        let col = ColliderBuilder::cuboid(r * 1.8, base_h, r * 1.8)
            .collision_groups(arm_groups())
            .build();
        self.colliders
            .insert_with_parent(col, base, &mut self.bodies);

        // Chain bodies: initial pose = home (identity rotations, links
        // stretched along +X from the shoulder anchor).
        let at_shoulder = Vec3::new(d.base[0], shoulder_y, d.base[2]);
        let mk_dynamic = |this: &mut Self, t: Vec3| {
            this.bodies.insert(
                RigidBodyBuilder::dynamic()
                    .can_sleep(false)
                    .additional_mass(LINK_ADDED_MASS)
                    .translation(t)
                    .build(),
            )
        };
        let carrier = mk_dynamic(self, at_shoulder);
        let link1 = mk_dynamic(self, at_shoulder);
        let link2 = mk_dynamic(
            self,
            Vec3::new(d.base[0] + d.links[0], shoulder_y, d.base[2]),
        );
        let wrist = mk_dynamic(
            self,
            Vec3::new(d.base[0] + d.links[0] + d.links[1], shoulder_y, d.base[2]),
        );
        self.chain = vec![carrier, link1, link2, wrist];

        // Colliders: carrier cube; links as cuboids centred mid-link.
        let collider = |this: &mut Self, body: RigidBodyHandle, half: [f32; 3], off: Vec3| {
            let c = ColliderBuilder::cuboid(half[0], half[1], half[2])
                .density(LINK_DENSITY)
                .friction(0.7)
                .translation(off)
                .collision_groups(arm_groups())
                .build();
            this.colliders.insert_with_parent(c, body, &mut this.bodies);
        };
        collider(self, carrier, [0.015, 0.015, 0.015], Vec3::ZERO);
        collider(
            self,
            link1,
            [d.links[0] * 0.5, r, r],
            Vec3::new(d.links[0] * 0.5, 0.0, 0.0),
        );
        collider(
            self,
            link2,
            [d.links[1] * 0.5, r, r],
            Vec3::new(d.links[1] * 0.5, 0.0, 0.0),
        );
        collider(
            self,
            wrist,
            [d.links[2] * 0.5, r, r],
            Vec3::new(d.links[2] * 0.5, 0.0, 0.0),
        );

        // Multibody joints: yaw (base→carrier), shoulder (carrier→link1),
        // elbow (link1→link2), wrist pitch (link2→wrist). Positive angle
        // about local +Z lifts the chain toward +Y (right-hand rule).
        self.stages.clear();
        let stages: [StageSpec; 4] = [
            (
                base,
                carrier,
                // base body centre sits shoulder_height/2 below the
                // shoulder anchor — the anchor is LOCAL to the base body.
                Vec3::new(0.0, d.shoulder_height - base_h, 0.0),
                Vec3::Y,
                d.limits[0],
            ),
            (carrier, link1, Vec3::ZERO, Vec3::Z, d.limits[1]),
            (
                link1,
                link2,
                Vec3::new(d.links[0], 0.0, 0.0),
                Vec3::Z,
                d.limits[2],
            ),
            (
                link2,
                wrist,
                Vec3::new(d.links[1], 0.0, 0.0),
                Vec3::Z,
                d.limits[3],
            ),
        ];
        for (parent, child, anchor, axis, limits) in stages {
            let builder = RevoluteJointBuilder::new(axis)
                .local_anchor1(anchor)
                .limits([limits.0, limits.1])
                // Adjacent stages overlap at the anchors at build time —
                // their mutual contact would fight the joint and inject
                // energy. Joints constrain the pair; the contact between
                // them is noise (and arm↔arm contact is group-disabled
                // anyway).
                .contacts_enabled(false)
                .build();
            if let Some(h) = self.mbody_joints.insert(parent, child, builder, true) {
                self.stages.push(Stage {
                    handle: h,
                    axis_vec: axis,
                    axis: JointAxis::AngX,
                });
            }
        }

        // Per-stage servo config: enable the position motor on the
        // revolute DoF (rapier leaves `motor_axes` empty — motors are
        // dead until the axis bit is set), ForceBased torque model and
        // a force clamp; plus rotor inertia (armature) and viscous
        // joint friction — the physical parts of a real servo.
        for s in &self.stages {
            if let Some((mb, link_id)) = self.mbody_joints.get_mut(s.handle) {
                if let Some(link) = mb.link_mut(link_id) {
                    link.joint.data.motor_axes.insert(JointAxesMask::ANG_X);
                    link.joint
                        .data
                        .set_motor_model(s.axis, MotorModel::ForceBased);
                    link.joint.data.set_motor_max_force(s.axis, MOTOR_MAX_FORCE);
                }
                for v in mb.armature_mut().iter_mut() {
                    *v = ARMATURE;
                }
                for v in mb.damping_mut().iter_mut() {
                    *v = JOINT_DAMPING;
                }
            }
        }

        self.tip_local = Vec3::new(d.links[2], 0.0, 0.0);
        self.prev_angles = vec![0.0; 4];
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

    /// The prop currently geometrically held, if any.
    pub fn holding(&self) -> Option<EntKey> {
        self.holding
    }

    fn run_grasp(&mut self, gripper: f32) {
        let closed = gripper > 0.5;
        match (closed, self.holding) {
            (true, None) => {
                let tip = self.last_ee.0;
                let mut best: Option<(f32, EntKey, RigidBodyHandle)> = None;
                for (k, h) in &self.map {
                    if let Some(b) = self.bodies.get(*h) {
                        let t = b.translation();
                        let d = Vec3::new(t.x - tip[0], t.y - tip[1], t.z - tip[2]).length();
                        if d < GRASP_RADIUS && best.as_ref().is_none_or(|(bd, _, _)| d < *bd) {
                            best = Some((d, *k, *h));
                        }
                    }
                }
                if let Some((_, k, h)) = best {
                    if let Some(b) = self.bodies.get_mut(h) {
                        b.set_body_type(RigidBodyType::KinematicPositionBased, true);
                    }
                    // Carry the prop hanging just below the tip: centring
                    // it ON the tip leaves half the cube inside the wrist
                    // collider, and release fires the separation impulse.
                    self.hold_offset = Vec3::new(0.0, -(PROP_HALF + 0.01), 0.0);
                    self.holding = Some(k);
                }
            }
            (false, Some(k)) => {
                self.holding = None;
                if let Some(h) = self.map.get(&k) {
                    if let Some(b) = self.bodies.get_mut(*h) {
                        b.set_body_type(RigidBodyType::Dynamic, true);
                        b.set_linvel(Vec3::new(0.0, -0.05, 0.0), true);
                        b.set_angvel(Vec3::ZERO, true);
                    }
                }
            }
            _ => {}
        }
    }

    fn carry(&mut self) {
        let Some(k) = self.holding else { return };
        let Some(h) = self.map.get(&k).copied() else {
            return;
        };
        if let Some(b) = self.bodies.get_mut(h) {
            let q = quat_from(self.last_ee.1);
            b.set_next_kinematic_rotation(q);
            let off = q.mul_vec3(self.hold_offset);
            let t = self.last_ee.0;
            b.set_next_kinematic_translation(Vec3::new(t[0] + off.x, t[1] + off.y, t[2] + off.z));
        }
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
            if self.holding == Some(k) {
                self.holding = None;
            }
        }
        for (k, p) in &live {
            if !self.map.contains_key(k) {
                let b = RigidBodyBuilder::dynamic()
                    .linear_damping(0.1)
                    .angular_damping(0.1)
                    .translation(Vec3::new(p.pos[0], p.pos[1], p.pos[2]))
                    .build();
                let h = self.bodies.insert(b);
                let c = ColliderBuilder::cuboid(PROP_HALF, PROP_HALF, PROP_HALF)
                    .density(400.0)
                    .friction(0.8)
                    .collision_groups(world_groups())
                    .build();
                self.colliders.insert_with_parent(c, h, &mut self.bodies);
                self.map.insert(*k, h);
                self.last_pose.insert(*k, (p.pos, p.rot));
            }
        }

        // 2. External pose writes (Teleport-style) — adopt into bodies.
        // Carried props are physics-owned while held and skipped.
        for (k, p) in &live {
            if self.holding.map(|h| h == *k).unwrap_or(false) {
                continue;
            }
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
        let mut arm: Option<(EntKey, Vec<f32>, f32)> = None;
        for (e, jt) in world.query::<JointTargets>() {
            arm = Some((key(e), jt.angles.clone(), jt.gripper));
        }
        let mut gripper = 0.0;
        if let Some((_, angles, g)) = &arm {
            gripper = *g;
            for (i, s) in self.stages.iter().enumerate() {
                let Some(&want) = angles.get(i) else { continue };
                let (lo, hi) = self.desc.limits[i];
                let want = want.clamp(lo, hi);
                if self.last_cmd.get(i).copied() != Some(want) {
                    if let Some((mb, link_id)) = self.mbody_joints.get_mut(s.handle) {
                        if let Some(link) = mb.link_mut(link_id) {
                            link.joint.data.set_motor_position(
                                s.axis,
                                want,
                                self.desc.kp,
                                self.desc.kd,
                            );
                        }
                    }
                    self.last_cmd[i] = want;
                }
            }
        }

        // 4. Step the pipeline — SUBSTEPS internal physics steps per
        // engine tick (decimation; 3D worlds are y-up).
        let gravity = Vec3::new(0.0, -g, 0.0);
        let params = IntegrationParameters {
            dt: dt / SUBSTEPS as f32,
            ..IntegrationParameters::default()
        };
        for _ in 0..SUBSTEPS {
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

        // 5. End-effector pose from the wrist body + grasp bookkeeping.
        let wrist = self.chain[3];
        let (tip_pos, tip_rot) = {
            let b = self.bodies.get(wrist).expect("wrist body exists");
            let q = *b.rotation();
            let off = q.mul_vec3(self.tip_local);
            let t = b.translation();
            ([t.x + off.x, t.y + off.y, t.z + off.z], quat_to_arr(q))
        };
        self.last_ee = (tip_pos, tip_rot);
        self.run_grasp(gripper);
        self.carry();

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
                holding: self.holding.is_some(),
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
    use oriera_common::{FrameIndex, Grants, Timestamp};

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
        // Home tip: base_x + l1 + l2 + l3 at shoulder height. The cube
        // spawns 3cm beyond the tip — outside the wrist collider, well
        // inside GRASP_RADIUS.
        let d = ArmDescription::default();
        let tip_x = d.base[0] + d.links[0] + d.links[1] + d.links[2];
        let tip_y = d.base[1] + d.shoulder_height;
        let (mut world, mut sys, arm, obj) = bench([tip_x + 0.03, tip_y, 0.0]);
        // Close on the cube immediately (it falls onto the table in the
        // first second — still inside GRASP_RADIUS of the tip).
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.gripper = 1.0;
        }
        for i in 0..30 {
            sys.run(&ctx(i), &mut world);
        }
        assert!(telemetry(&world, arm).holding, "cube must be grasped");
        // Raise the shoulder — the cube must ride up with the tip.
        let y_before = pose(&world, obj).pos[1];
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.angles = vec![0.0, 0.9, 0.0, 0.0];
        }
        for i in 30..120 {
            sys.run(&ctx(i), &mut world);
        }
        let carried = pose(&world, obj).pos[1];
        assert!(
            carried > y_before + 0.05,
            "carried cube must ride the wrist: {y_before} → {carried}"
        );
        assert!(telemetry(&world, arm).holding);
        assert!(sim_fingerprint(&world) != 0);
        // Open — the cube must drop back toward the table.
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.gripper = 0.0;
        }
        for i in 120..240 {
            sys.run(&ctx(i), &mut world);
        }
        assert!(!telemetry(&world, arm).holding, "release must drop");
        let dropped = pose(&world, obj).pos[1];
        assert!(
            dropped < carried,
            "cube must fall after release: {carried} → {dropped}"
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
}
