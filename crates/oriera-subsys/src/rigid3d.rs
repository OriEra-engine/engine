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
//!
//! **M0b (PLAN-embodied-general §四 M0b)**: the gripper plates live in
//! a spec-defined *plate frame* — `GripperSpec::{approach_axis,
//! gap_axis}` in the attach link's frame, legacy `+X`/`+Z` when unset —
//! so an external gripper hanging off a URDF flange is placed by data,
//! not by the preset's convention. Receipts gain a complete-identity
//! fingerprint ([`MultibodySystem::fingerprint_v2`], scheme `"v2"`)
//! next to the legacy triple fold ([`MultibodySystem::fingerprint`],
//! scheme `"v1"`, kept bit-for-bit); [`state_hash_from_v1`] bridges old
//! anchors onto the pure state axis.
//!
//! **M2 物理保真 (PLAN §四 M2 / §9.1 U-1/U-3/U-6/U-9)** — every item an
//! opt-in spec field, defaults reproducing the legacy build bit for
//! bit:
//! - **U-1 real inertia**: `LinkSpec::inertial` (Some) hands the full
//!   tensor to rapier `MassProperties::with_inertia_matrix` on the
//!   body, with the collider contributing zero mass; `None` keeps the
//!   density/`added_mass` derivation. Armature has fed the multibody
//!   DoFs since M0a (per-joint, step 5 of [`MultibodySystem::build_arm`]).
//! - **U-3 mesh collision**: `LinkShape::ConvexHull` builds a rapier
//!   convex hull (the *chain-body* policy — stable manifold contacts),
//!   `LinkShape::Mesh` a trimesh (gripper contact faces via
//!   `GripperSpec::contact_mesh`); [`collision_report`] prints the
//!   human-readable collider volume/AABB summary.
//! - **U-9 self collision**: `RobotSpec::self_collision =
//!   AdjacentExempt` switches the chain to per-link collision-group
//!   bits — non-adjacent links may touch, joint-joined pairs stay
//!   exempt; `Off` (default) keeps the legacy groups untouched.
//! - **U-6 contact-level grasp**: `GripperSpec::holding_criterion =
//!   Contact { min_normal_force, max_slip }` decides `holding` from
//!   the solver's per-tick plate→prop normal impulses and relative
//!   speed instead of the `GRASP_RADIUS` tip heuristic.
//!
//! **M3 扩展体 (PLAN §四 M3 移动底盘 / 真 fixture 通用化)** — again
//! opt-in, defaults bit-identical:
//! - **Prismatic joints**: [`crate::robot::JointKind::Prismatic`] builds
//!   a rapier multibody *prismatic* joint (free DoF `LinX` along the
//!   spec axis, limits in metres) with the same ForceBased position
//!   motor, effort clamp, armature and damping wiring as a revolute
//!   stage — every scalar in the slide's own units (N/m, N, kg, N·s/m;
//!   table on `JointSpec`). The readout is the slide displacement: the
//!   link's `local_to_parent` translation, minus the joint anchor,
//!   projected on the unit axis — the same frame the motor acts in.
//!   Telemetry `angles`/`velocities` therefore carry metres / m/s in a
//!   prismatic slot.
//! - **Fixtures**: [`MultibodySystem::from_robot_fixtures`] builds any
//!   number of static slabs ([`FixtureSlab`]), each with its own
//!   friction. [`Scene3D`] stays the legacy two-slab description and
//!   projects onto that list ([`Scene3D::fixture_slabs`]: ground, then
//!   table, both at [`LEGACY_FIXTURE_FRICTION`]) — `from_robot` goes
//!   through it, so the default build sequence is the pre-M3 one.

use crate::robot::{
    HoldingCriterion, Inertial, JointKind, LinkShape, RobotSpec, RobotSpecError, SelfCollision,
};
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
/// 0.8). Insert together with `Pose3` + `SimObject`. (serde derive:
/// M0a puts the nominal prop physics into the scene spec.)
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
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

/// Collider primitive of a prop entity (PLAN-embodied-general M0a:
/// props enter the scene spec). Optional component next to `Pose3 +
/// SimObject`; absent ⇒ the legacy 5 cm cube
/// (`Cuboid { half: [PROP_HALF; 3] }`), so every pre-M0a world builds
/// bit-identically. Metres; `Cylinder` stands on its axis (local Y,
/// the world up axis), `Sphere` is a ball.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)
)]
pub enum PropShape {
    Cuboid { half: [f32; 3] },
    Cylinder { half_h: f32, radius: f32 },
    Sphere { r: f32 },
}

impl Default for PropShape {
    fn default() -> Self {
        Self::Cuboid {
            half: [PROP_HALF; 3],
        }
    }
}

impl PropShape {
    /// Top-down footprint half extents `(x, z)` (renderer hint).
    pub fn footprint(&self) -> (f32, f32) {
        match *self {
            Self::Cuboid { half } => (half[0], half[2]),
            Self::Cylinder { radius, .. } => (radius, radius),
            Self::Sphere { r } => (r, r),
        }
    }

    /// Every dimension finite and positive.
    pub fn is_valid(&self) -> bool {
        let ok = |v: f32| v.is_finite() && v > 0.0;
        match *self {
            Self::Cuboid { half } => half.iter().all(|v| ok(*v)),
            Self::Cylinder { half_h, radius } => ok(half_h) && ok(radius),
            Self::Sphere { r } => ok(r),
        }
    }
}

/// Dynamic prop collider from its shape + physics override. The
/// default shape reproduces the legacy `ColliderBuilder::cuboid(
/// PROP_HALF, PROP_HALF, PROP_HALF)` call exactly.
fn prop_collider(shape: PropShape, pp: PropPhys, groups: InteractionGroups) -> Collider {
    let b = match shape {
        PropShape::Cuboid { half } => ColliderBuilder::cuboid(half[0], half[1], half[2]),
        PropShape::Cylinder { half_h, radius } => ColliderBuilder::cylinder(half_h, radius),
        PropShape::Sphere { r } => ColliderBuilder::ball(r),
    };
    b.density(pp.density)
        .friction(pp.friction)
        .collision_groups(groups)
        .build()
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

/// Contact friction of the legacy ground/table slabs — the value the
/// pre-M3 `build_scene` hardcoded for both.
pub const LEGACY_FIXTURE_FRICTION: f32 = 0.9;

/// One static furniture slab (M3 真 fixture 通用化, the M2 leftover
/// "每 fixture friction"): an axis-aligned box, world frame, metres,
/// with its own contact friction. [`MultibodySystem::from_robot_fixtures`]
/// builds any number of them in list order (rails, shelves, bins,
/// cabinet walls, a drawer runner…); the agent-side `SceneGraph.fixtures`
/// maps onto this list one to one.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FixtureSlab {
    /// Slab centre, world frame (m).
    pub center: [f32; 3],
    /// Half extents (m), all > 0.
    pub half: [f32; 3],
    /// Contact friction coefficient (≥ 0).
    pub friction: f32,
}

impl FixtureSlab {
    /// Why this slab cannot be built (`None` = buildable): finite
    /// centre, finite positive half extents, finite non-negative
    /// friction.
    pub fn problem(&self) -> Option<String> {
        if !self.center.iter().all(|v| v.is_finite()) {
            return Some(format!("center {:?} must be finite", self.center));
        }
        if !self.half.iter().all(|v| v.is_finite() && *v > 0.0) {
            return Some(format!("half {:?} must be finite and > 0", self.half));
        }
        if !(self.friction.is_finite() && self.friction >= 0.0) {
            return Some(format!(
                "friction {} must be finite and >= 0",
                self.friction
            ));
        }
        None
    }
}

impl Scene3D {
    /// The legacy pair as the generic fixture list, in the pre-M3 build
    /// order: `ground`, then `table` (when present), both at
    /// [`LEGACY_FIXTURE_FRICTION`]. `from_robot(robot, scene)` builds
    /// exactly this list — bit-identical to the old hardcoded path.
    pub fn fixture_slabs(&self) -> Vec<FixtureSlab> {
        let slab = |(center, half): ([f32; 3], [f32; 3])| FixtureSlab {
            center,
            half,
            friction: LEGACY_FIXTURE_FRICTION,
        };
        std::iter::once(slab(self.ground))
            .chain(self.table.map(slab))
            .collect()
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
/// injects explosive energy (the classic robot-arm pitfall). Opt-in
/// per-link self collision: [`SelfCollision::AdjacentExempt`] (U-9) —
/// see [`exempt_link_groups`].
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

/// Identity bit of chain link `i` under `AdjacentExempt`: bits 2..31
/// (0/1 are the world/plate families); chains longer than 30 links
/// share the last bit and lose mutual contacts there (documented
/// limitation, `docs/PHYSICS-FIDELITY.md`).
fn link_group_bit(i: usize) -> Group {
    Group::from_bits_retain(1u32 << (i + 2).min(31))
}

/// The joint-joined link pairs (each joint's parent/child, unordered).
fn adjacency(spec: &RobotSpec) -> BTreeSet<(usize, usize)> {
    spec.joints
        .iter()
        .map(|j| {
            (
                j.parent_link.min(j.child_link),
                j.parent_link.max(j.child_link),
            )
        })
        .collect()
}

/// Collision groups of chain link `i` under [`SelfCollision::AdjacentExempt`]
/// (U-9): membership is the link's own identity bit; the filter is the
/// world family plus every *non-adjacent* link's bit — so link↔link
/// contacts exist exactly for pairs no joint joins (the per-joint
/// `contacts_enabled(false)` stays as the adjacent-pair backstop).
/// Plates (GROUP_2, filter GROUP_1) never meet links in this scheme,
/// same as the legacy mode.
fn exempt_link_groups(
    i: usize,
    spec: &RobotSpec,
    adj: &BTreeSet<(usize, usize)>,
) -> InteractionGroups {
    let mut filter = Group::GROUP_1;
    for j in 0..spec.links.len() {
        let pair = (i.min(j), i.max(j));
        if j != i && !adj.contains(&pair) {
            filter |= link_group_bit(j);
        }
    }
    InteractionGroups::new(link_group_bit(i), filter, InteractionTestMode::And)
}

/// World-side (furniture + prop) groups under `AdjacentExempt`: meet
/// the world family, the plates, and every link identity bit (a link's
/// membership is only its own bit, so the filter must name it).
fn exempt_world_groups(spec: &RobotSpec) -> InteractionGroups {
    let mut filter = Group::GROUP_1 | Group::GROUP_2;
    for j in 0..spec.links.len() {
        filter |= link_group_bit(j);
    }
    InteractionGroups::new(Group::GROUP_1, filter, InteractionTestMode::And)
}

/// Solver-side mass properties of one chain body, read back from the
/// multibody (U-1 acceptance: `body.mass_properties()` vs the URDF
/// table). `principal_inertia` are the eigenvalues of the body-frame
/// tensor (sorted descending by rapier's symmetric eigen), and
/// `principal_frame` is the rotation from body axes into the principal
/// frame — reconstruct with `R · diag(λ) · Rᵀ` to compare against a
/// URDF tensor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkMassReadback {
    pub mass: f32,
    /// Centre of mass in the link body's local frame.
    pub com: [f32; 3],
    /// Principal angular inertias (kg·m²).
    pub principal_inertia: [f32; 3],
    /// Body→principal-frame rotation quaternion `[x, y, z, w]`.
    pub principal_frame: [f32; 4],
}

/// The 8 corners + 12 triangles of an axis-aligned box (the
/// `contact_mesh` plate collider). Wound consistently outward.
fn box_trimesh(half: [f32; 3]) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let [hx, hy, hz] = half;
    let verts = vec![
        Vec3::new(-hx, -hy, -hz),
        Vec3::new(hx, -hy, -hz),
        Vec3::new(hx, hy, -hz),
        Vec3::new(-hx, hy, -hz),
        Vec3::new(-hx, -hy, hz),
        Vec3::new(hx, -hy, hz),
        Vec3::new(hx, hy, hz),
        Vec3::new(-hx, hy, hz),
    ];
    let idx = [
        [0, 2, 1],
        [0, 3, 2], // -z
        [4, 5, 6],
        [4, 6, 7], // +z
        [0, 1, 5],
        [0, 5, 4], // -y
        [3, 6, 2],
        [3, 7, 6], // +y
        [1, 2, 6],
        [1, 6, 5], // +x
        [0, 4, 7],
        [0, 7, 3], // -x
    ];
    (verts, idx.to_vec())
}

/// Spec inertial tensor → rapier `MassProperties` (U-1): mass and
/// tensor scaled by `mass_scale` (inertia ∝ mass at fixed geometry),
/// principal-axis decomposition via the symmetric eigen. A zero
/// (degenerate) tensor is left as-is — rapier treats infinite inverse
/// inertia components conservatively and the multibody joint
/// constraints keep the DoF sane.
fn inertial_mass_properties(i: &Inertial, ms: f32) -> MassProperties {
    let m = i.mass * ms;
    // Symmetric: row- vs column-major is the same matrix.
    let inertia = Mat3::from_cols_array_2d(&[
        [i.ixx * ms, i.ixy * ms, i.ixz * ms],
        [i.ixy * ms, i.iyy * ms, i.iyz * ms],
        [i.ixz * ms, i.iyz * ms, i.izz * ms],
    ]);
    MassProperties::with_inertia_matrix(Vec3::new(i.com[0], i.com[1], i.com[2]), m, inertia)
}

/// Human-readable collider summary of a spec (U-3 acceptance: 外部 STL
/// 臂碰撞体积/AABB 人话报告): per link one line — shape, collider
/// volume (analytic for the primitives, hull-derived for a
/// ConvexHull, "—" for an open Mesh) and the AABB half extents of the
/// shape around its own origin.
pub fn collision_report(r: &RobotSpec) -> String {
    let mut lines = vec![format!(
        "collision report — {} ({} links, self_collision={:?})",
        r.name,
        r.links.len(),
        r.self_collision
    )];
    for l in &r.links {
        let shape = match &l.shape {
            LinkShape::Box { half } => format!("box half {half:?}"),
            LinkShape::Capsule { half_len, radius } => {
                format!("capsule half_len {half_len} radius {radius}")
            }
            LinkShape::ConvexHull { points } => format!("convex_hull {} points", points.len()),
            LinkShape::Mesh { vertices, indices } => {
                format!("mesh {} verts / {} tris", vertices.len(), indices.len())
            }
        };
        // Volume: analytic for the primitives; a hull at unit density
        // weighs its own volume; a triangle mesh has no well-defined
        // closed volume here.
        let volume = match &l.shape {
            LinkShape::Box { half } => 8.0 * half[0] * half[1] * half[2],
            LinkShape::Capsule { half_len, radius } => {
                std::f32::consts::PI * radius * radius * (2.0 * half_len)
                    + 4.0 / 3.0 * std::f32::consts::PI * radius.powi(3)
            }
            LinkShape::ConvexHull { points } => {
                let pts: Vec<Vec3> = points.iter().map(|p| Vec3::new(p[0], p[1], p[2])).collect();
                ColliderBuilder::convex_hull(&pts).map_or(f32::NAN, |b| {
                    b.density(1.0).build().mass_properties().mass()
                })
            }
            LinkShape::Mesh { .. } => f32::NAN,
        };
        let vol_s = if volume.is_finite() {
            format!("{volume:.3e} m^3")
        } else {
            "-".into()
        };
        let aabb = shape_aabb_half(&l.shape);
        lines.push(format!(
            "  {:<14} {:<38} volume {:<12} aabb_half [{:.4}, {:.4}, {:.4}]",
            l.name, shape, vol_s, aabb[0], aabb[1], aabb[2]
        ));
    }
    lines.join("\n")
}

/// AABB half extents of a shape around its own origin (link frame).
fn shape_aabb_half(s: &LinkShape) -> [f32; 3] {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    let mut acc = |p: [f32; 3]| {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    };
    match s {
        LinkShape::Box { half } => {
            for x in [-half[0], half[0]] {
                for y in [-half[1], half[1]] {
                    for z in [-half[2], half[2]] {
                        acc([x, y, z]);
                    }
                }
            }
        }
        LinkShape::Capsule { half_len, radius } => {
            for sx in [-1.0, 1.0] {
                acc([sx * (half_len + radius), *radius, *radius]);
                acc([sx * (half_len + radius), -*radius, -*radius]);
            }
        }
        LinkShape::ConvexHull { points }
        | LinkShape::Mesh {
            vertices: points, ..
        } => {
            for p in points {
                acc(*p);
            }
        }
    }
    [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]].map(|v| v * 0.5)
}

/// One actuated stage: its multibody joint handle, the joint axis in
/// the parent's local frame (readout) and the generic-joint axis slot
/// the motor drives. Index `i` pairs with `RobotSpec::joints[i]` for
/// limits/gains/velocity limit.
#[derive(Clone, Copy)]
struct Stage {
    handle: MultibodyJointHandle,
    /// Revolute: the axis exactly as written in the spec (the legacy
    /// angle readout, bit-pinned). Prismatic: the *unit* slide direction.
    axis_vec: Vec3,
    /// The free DoF slot in the joint frame: `AngX` (revolute) /
    /// `LinX` (prismatic).
    axis: JointAxis,
    /// Prismatic only: the joint anchor in the parent frame — removed
    /// from `local_to_parent`'s translation before the axis projection
    /// (zero and unused for revolute stages).
    anchor: Vec3,
    prismatic: bool,
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

/// Receipt tag of the legacy fingerprint fold ([`MultibodySystem::fingerprint`]).
pub const FINGERPRINT_SCHEME_V1: &str = "v1";
/// Receipt tag of the complete-identity fold ([`MultibodySystem::fingerprint_v2`]).
pub const FINGERPRINT_SCHEME_V2: &str = "v2";

/// Fold a 3-vector into the hash.
fn bits3(mut h: u64, v: [f32; 3]) -> u64 {
    for x in v {
        h = bits(h, x);
    }
    h
}

/// Fold an optional float: `None` is a sentinel distinct from every
/// finite bit pattern (NaN bits would alias; `u64::MAX` cannot).
fn bits_opt(h: u64, v: Option<f32>) -> u64 {
    mix(h, v.map_or(u64::MAX, |x| x.to_bits() as u64))
}

/// Complete robot identity (scheme v2): DoF count, link count, mass
/// scale, every joint's `(parent, child, anchor, axis, limits, effort,
/// velocity_limit, kp, kd, armature, damping)`, every link's `(shape,
/// origin, collider_offset, density, friction, added_mass)` and the
/// gripper summary `(attach_link, finger_x, tcp_offset, finger_half,
/// gaps, friction, effort, gap_rate, kind, approach/gap axes)` — the
/// axes folded in their *resolved* form, so an explicitly written
/// default equals an omitted one (same canonicalization as
/// `GripperSpec::canonicalize_axes`). Names and `mesh_uri` are not
/// identity (a renamed link is the same physics; the uri is collider
/// provenance since M2). **M2 fields fold only when non-default**
/// (`inertial` Some, `ConvexHull`/`Mesh` shapes, `contact_mesh`,
/// `Contact` criterion, `AdjacentExempt`), so every pre-M2 identity
/// hash — and the pinned goldens — are bit-stable. **M3**: a prismatic
/// joint folds a kind tag (revolute folds nothing, same discipline).
pub fn robot_identity_hash(r: &RobotSpec) -> u64 {
    let mut h = mix(0x0000_6f72_6965_7261, 0x0052_4f42_4f54); // "ROBOT"
    h = mix(h, r.joints.len() as u64);
    h = mix(h, r.links.len() as u64);
    h = bits(h, r.mass_scale);
    h = bits3(h, r.base);
    for j in &r.joints {
        h = mix(h, j.parent_link as u64);
        h = mix(h, j.child_link as u64);
        h = bits3(h, j.anchor);
        h = bits3(h, j.axis);
        h = bits(h, j.limits.0);
        h = bits(h, j.limits.1);
        h = bits_opt(h, j.effort);
        h = bits_opt(h, j.velocity_limit);
        h = bits(h, j.kp);
        h = bits(h, j.kd);
        h = bits(h, j.armature);
        h = bits(h, j.damping);
        // M3: the joint kind folds ONLY when non-default (prismatic) —
        // a revolute joint (every pre-M3 robot) hashes exactly as before.
        if j.kind == JointKind::Prismatic {
            h = mix(h, 0x9815_3A71); // "PRISMATIC" tag
        }
    }
    for l in &r.links {
        match &l.shape {
            LinkShape::Capsule { half_len, radius } => {
                h = mix(h, 1);
                h = bits(h, *half_len);
                h = bits(h, *radius);
            }
            LinkShape::Box { half } => {
                h = mix(h, 2);
                h = bits3(h, *half);
            }
            // M2 shapes: the resolved geometry IS identity (folded
            // wholesale — these variants never existed pre-M2, so no
            // back-compat concern).
            LinkShape::ConvexHull { points } => {
                h = mix(h, 3);
                h = mix(h, points.len() as u64);
                for p in points {
                    h = bits3(h, *p);
                }
            }
            LinkShape::Mesh { vertices, indices } => {
                h = mix(h, 4);
                h = mix(h, vertices.len() as u64);
                h = mix(h, indices.len() as u64);
                for p in vertices {
                    h = bits3(h, *p);
                }
                for t in indices {
                    for k in t {
                        h = mix(h, *k as u64);
                    }
                }
            }
        }
        h = bits3(h, l.origin);
        h = bits3(h, l.collider_offset);
        h = bits(h, l.density);
        h = bits(h, l.friction);
        h = bits(h, l.added_mass);
        // M2 fidelity fields fold ONLY when non-default, so every
        // pre-M2 spec's v2 identity (and the pinned goldens) is
        // bit-stable.
        if let Some(i) = &l.inertial {
            h = mix(h, 0x1143_A1A1); // "INERTIAL" tag
            h = bits(h, i.mass);
            h = bits3(h, i.com);
            for v in [i.ixx, i.iyy, i.izz, i.ixy, i.ixz, i.iyz] {
                h = bits(h, v);
            }
        }
    }
    match &r.gripper {
        None => h = mix(h, u64::MAX),
        Some(g) => {
            h = mix(h, g.attach_link as u64);
            h = bits(h, g.finger_x);
            h = bits3(h, g.tcp_offset);
            h = bits3(h, g.finger_half);
            h = bits(h, g.gap_open);
            h = bits(h, g.gap_closed);
            h = bits(h, g.friction);
            h = bits(h, g.effort);
            h = bits(h, g.gap_rate);
            h = mix(
                h,
                match g.kind {
                    crate::robot::GripperKind::ParallelPlates => 1,
                },
            );
            h = bits3(h, g.approach_axis_or_default());
            h = bits3(h, g.gap_axis_or_default());
            // M2 gripper fidelity: same default-omission discipline as
            // the link fields above.
            if g.contact_mesh {
                h = mix(h, 0xC011_7A7E); // "CONTACT-MESH" tag
            }
            if let HoldingCriterion::Contact {
                min_normal_force,
                max_slip,
            } = g.holding_criterion
            {
                h = mix(h, 0x10C1_D200); // "HOLDING-CONTACT" tag
                h = bits(h, min_normal_force);
                h = bits(h, max_slip);
            }
        }
    }
    // M2 self collision: folded only when on (Off is the legacy world).
    if r.self_collision == SelfCollision::AdjacentExempt {
        h = mix(h, 0x5E1F_C011); // "SELF-COLL" tag
    }
    h
}

/// Scheme-v2 fold from its three parts — `state` is a
/// [`sim_fingerprint`] value, `identity` a [`robot_identity_hash`],
/// `spec_hash` the agent-side canonical spec hash when known.
pub fn fingerprint_v2_from_parts(state: u64, identity: u64, spec_hash: Option<u64>) -> u64 {
    let h = mix(state, 0x0000_0000_4650_5632); // "FPV2"
    let h = mix(h, identity);
    mix(h, spec_hash.unwrap_or(u64::MAX))
}

/// Inverse of one `mix` round for a *known* second operand: given
/// `y = mix(h, x)` and `x`, return `h`. The round is `h ^ x` followed
/// by a bijective finalizer (two odd multiplies — invertible mod 2⁶⁴ —
/// and two xor-shifts), so this is exact.
pub fn mix_inverse_h(y: u64, x: u64) -> u64 {
    const C1: u64 = 0xff51_afd7_ed55_8ccd;
    const C2: u64 = 0xc4ce_b9fe_1a85_ec53;
    // Modular inverse of an odd constant by Newton iteration (6 rounds
    // double the correct bits from 3 → 64+).
    fn inv_odd(c: u64) -> u64 {
        let mut inv = c;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(c.wrapping_mul(inv)));
        }
        inv
    }
    // Inverse of `t ^= t >> s`: each pass recovers the next `s` bits
    // from the top; `ceil(64/s)` passes suffice.
    fn unxorshift(y: u64, s: u32) -> u64 {
        let mut t = y;
        for _ in 0..64u32.div_ceil(s) {
            t = y ^ (t >> s);
        }
        t
    }
    let t = unxorshift(y, 32);
    let t = t.wrapping_mul(inv_odd(C2));
    let t = unxorshift(t, 29);
    let t = t.wrapping_mul(inv_odd(C1));
    t ^ x
}

/// Recover the pure world-state hash ([`sim_fingerprint`]) folded into
/// a scheme-v1 receipt value ([`MultibodySystem::fingerprint`]) when the
/// identity triple it was folded with is known — `(joints[0].kp,
/// joints[0].kd, mass_scale)` of the robot the episode ran with. This is
/// how a pre-M0b `episode_state_hashes` anchor is compared on the state
/// axis against a v2 receipt without re-deriving old binaries.
pub fn state_hash_from_v1(fp_v1: u64, kp: f32, kd: f32, mass_scale: f32) -> u64 {
    let h = mix_inverse_h(fp_v1, mass_scale.to_bits() as u64);
    let h = mix_inverse_h(h, kd.to_bits() as u64);
    mix_inverse_h(h, kp.to_bits() as u64)
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
    /// The spec this system was built from — limits, gains, gripper
    /// geometry are read from here every tick. (M0b: the legacy
    /// `desc: ArmDescription` placeholder is gone — the spec is the one
    /// build source; `MultibodySystem::new(desc, …)` still accepts the
    /// 4-DoF preset and projects it through `RobotSpec::from`.)
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
    /// Unit plate approach / separation axes in the attach link's frame
    /// (M0b `GripperSpec::{approach_axis, gap_axis}`, legacy `+X`/`+Z`
    /// when unset — normalizing those is exact, so the default path is
    /// bit-identical).
    approach_vec: Vec3,
    gap_vec: Vec3,
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
    /// Plate-pair centre the previous tick — the finite-difference
    /// plate velocity for the Contact holding criterion (kinematic
    /// bodies driven by `set_next_kinematic_translation` do not carry a
    /// usable solver linvel).
    plate_prev_centre: Option<Vec3>,
    /// Per-tick plate-centre velocity (finite difference), updated by
    /// [`Self::drive_fingers`].
    plate_vel: Vec3,
    /// Physics substep dt of the last `run` — converts the solver's
    /// normal impulses into per-tick forces for
    /// [`HoldingCriterion::Contact`].
    last_substep_dt: f32,
    /// End-effector pose after the last step.
    last_ee: ([f32; 3], [f32; 4]),
}

impl MultibodySystem {
    /// Build the legacy 4-DoF tabletop arm. Goes through
    /// `RobotSpec::from(&desc)` — the preset projection is always
    /// buildable, so this cannot fail.
    pub fn new(desc: ArmDescription, scene: Scene3D) -> Self {
        let robot = RobotSpec::from(&desc);
        Self::from_robot(robot, scene).expect("ArmDescription preset projects to a valid RobotSpec")
    }

    /// Build an arbitrary [`RobotSpec`] (validated first — an
    /// unbuildable collider shape is a named [`RobotSpecError::BadShape`],
    /// never a silently dropped collider; a `mesh_uri` is provenance
    /// metadata and never an error since M2) into the legacy two-slab
    /// [`Scene3D`] — i.e. [`Self::from_robot_fixtures`] over
    /// [`Scene3D::fixture_slabs`] (bit-identical to the pre-M3 build).
    pub fn from_robot(robot: RobotSpec, scene: Scene3D) -> Result<Self, RobotSpecError> {
        Self::from_robot_fixtures(robot, &scene.fixture_slabs())
    }

    /// Build a [`RobotSpec`] into a world of arbitrary static
    /// [`FixtureSlab`]s (M3 真 fixture 通用化): any count — including
    /// none, a furniture-free world for pure articulated dynamics — each
    /// slab with its own friction, built in list order. An unbuildable
    /// slab is a named [`RobotSpecError::BadFixture`].
    pub fn from_robot_fixtures(
        robot: RobotSpec,
        fixtures: &[FixtureSlab],
    ) -> Result<Self, RobotSpecError> {
        robot.validate()?;
        if let Some((index, reason)) = fixtures
            .iter()
            .enumerate()
            .find_map(|(i, f)| f.problem().map(|p| (i, p)))
        {
            return Err(RobotSpecError::BadFixture { index, reason });
        }
        let n = robot.joints.len();
        let gap0 = robot.gripper.as_ref().map_or(0.0, |g| g.gap_open);
        let unit = |a: [f32; 3]| Vec3::new(a[0], a[1], a[2]).normalize();
        let approach_vec = robot
            .gripper
            .as_ref()
            .map_or(Vec3::X, |g| unit(g.approach_axis_or_default()));
        let gap_vec = robot
            .gripper
            .as_ref()
            .map_or(Vec3::Z, |g| unit(g.gap_axis_or_default()));
        let mut sys = Self {
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
            approach_vec,
            gap_vec,
            map: BTreeMap::new(),
            last_pose: BTreeMap::new(),
            prev_angles: vec![0.0; n],
            last_cmd: vec![f32::NAN; n],
            holding: false,
            fingers: None,
            gap_now: gap0,
            plate_prev_centre: None,
            plate_vel: Vec3::ZERO,
            last_substep_dt: (1.0 / 60.0) / SUBSTEPS as f32,
            last_ee: ([0.0; 3], [0.0, 0.0, 0.0, 1.0]),
        };
        sys.build_scene(fixtures);
        sys.build_arm()?;
        Ok(sys)
    }

    fn fixed_slab(
        &mut self,
        center: [f32; 3],
        half: [f32; 3],
        friction: f32,
        groups: InteractionGroups,
    ) {
        let body = RigidBodyBuilder::fixed()
            .translation(Vec3::new(center[0], center[1], center[2]))
            .build();
        let h = self.bodies.insert(body);
        let col = ColliderBuilder::cuboid(half[0], half[1], half[2])
            .friction(friction)
            .collision_groups(groups)
            .build();
        self.colliders.insert_with_parent(col, h, &mut self.bodies);
    }

    /// Static furniture, one fixed body + cuboid collider per slab, in
    /// list order with the slab's own friction (M3). The legacy
    /// [`Scene3D`] reaches here as `[ground@0.9, table@0.9]` — the exact
    /// pre-M3 call sequence, so the default world is bit-identical.
    fn build_scene(&mut self, fixtures: &[FixtureSlab]) {
        let world = match self.robot.self_collision {
            SelfCollision::Off => world_groups(),
            SelfCollision::AdjacentExempt => exempt_world_groups(&self.robot),
        };
        for f in fixtures {
            self.fixed_slab(f.center, f.half, f.friction, world);
        }
    }

    /// Generic N-stage build from `self.robot` (validated). The
    /// insertion sequence — every link body in link order, then every
    /// link collider in link order, then the plates, then the joints in
    /// order — is the legacy sequence for the preset, so body/collider
    /// handles and solver ordering are unchanged (bit-exact receipt).
    fn build_arm(&mut self) -> Result<(), RobotSpecError> {
        let r = self.robot.clone();
        let ms = r.mass_scale;
        let root = r.root_link().expect("validated: exactly one root");
        let v3 = |a: [f32; 3]| Vec3::new(a[0], a[1], a[2]);
        let adj = adjacency(&r);

        // 1. Bodies: the root is a fixed stub the chain stands on; every
        //    other link is dynamic at its home origin (identity
        //    rotation; the multibody's own FK re-derives these from the
        //    root pose + anchors on the first step). U-1: a link with
        //    `inertial` gets the real (mass-scaled) tensor via
        //    `additional_mass_properties` — its collider then
        //    contributes zero mass (step 2), so the body total is
        //    exactly the spec values; the default path keeps the legacy
        //    `added_mass` write bit for bit.
        self.chain.clear();
        for (i, l) in r.links.iter().enumerate() {
            let body = if i == root {
                RigidBodyBuilder::fixed().translation(v3(l.origin)).build()
            } else {
                let mut b = RigidBodyBuilder::dynamic().can_sleep(false);
                b = match l.inertial {
                    Some(iner) => b.additional_mass_properties(inertial_mass_properties(&iner, ms)),
                    None => b.additional_mass(l.added_mass * ms),
                };
                b.translation(v3(l.origin)).build()
            };
            self.chain.push(self.bodies.insert(body));
        }

        // 2. Colliders: shape per spec (U-3: analytic primitives, or a
        //    resolved convex hull / triangle mesh), dynamic links get
        //    `density · mass_scale` unless the real inertial replaced
        //    the mass model (the collider then contributes zero —
        //    rapier's zero `MassProperties` short-circuits the combine,
        //    so the body keeps exactly the tensor set in step 1).
        //    Self-collision (U-9): `Off` keeps the legacy arm groups;
        //    `AdjacentExempt` gives every link its own group bit.
        for (i, l) in r.links.iter().enumerate() {
            let shape_err = |reason: String| RobotSpecError::BadShape {
                link: l.name.clone(),
                reason,
            };
            let builder = match &l.shape {
                LinkShape::Box { half } => ColliderBuilder::cuboid(half[0], half[1], half[2]),
                LinkShape::Capsule { half_len, radius } => {
                    ColliderBuilder::capsule_x(*half_len, *radius)
                }
                LinkShape::ConvexHull { points } => {
                    let pts: Vec<Vec3> = points.iter().map(|p| v3(*p)).collect();
                    let hull = ColliderBuilder::convex_hull(&pts).ok_or_else(|| {
                        shape_err(
                            "convex hull is degenerate (coplanar/degenerate point cloud)".into(),
                        )
                    })?;
                    // rapier tolerates coplanar clouds by returning a
                    // zero-volume sliver — refuse it here so a broken
                    // STL never becomes an invisible collider.
                    let vol = hull.density(1.0).build().mass_properties().mass();
                    if !(vol.is_finite() && vol > 1e-12) {
                        return Err(shape_err(format!(
                            "convex hull has degenerate volume {vol} m³ (coplanar points?)"
                        )));
                    }
                    ColliderBuilder::convex_hull(&pts).ok_or_else(|| {
                        shape_err(
                            "convex hull is degenerate (coplanar/degenerate point cloud)".into(),
                        )
                    })?
                }
                LinkShape::Mesh { vertices, indices } => {
                    let verts: Vec<Vec3> = vertices.iter().map(|p| v3(*p)).collect();
                    ColliderBuilder::trimesh(verts, indices.clone())
                        .map_err(|e| shape_err(format!("trimesh rejected: {e}")))?
                }
            };
            let density = if i == root { l.density } else { l.density * ms };
            let c = match l.inertial {
                Some(_) => builder.mass_properties(MassProperties::default()),
                None => builder.density(density),
            };
            let groups = match r.self_collision {
                SelfCollision::Off => arm_groups(),
                SelfCollision::AdjacentExempt => exempt_link_groups(i, &r, &adj),
            };
            let c = c
                .friction(l.friction)
                .translation(v3(l.collider_offset))
                .collision_groups(groups)
                .build();
            self.colliders
                .insert_with_parent(c, self.chain[i], &mut self.bodies);
        }

        // 3. End-effector frame + gripper plates. Plates are kinematic
        //    slabs straddling the plate centre in the attach link's frame;
        //    the prop stays Dynamic — plate/cube friction does the holding
        //    (minimal-probe-verified, tests/probe_friction.rs round 4).
        //    They are born at their HOME pose (centre = finger_x along the
        //    approach axis, ±gap_open/2 along the gap axis, identity link
        //    rotation): a kinematic body that teleports on its first
        //    drive sweeps through whatever sits between — an external
        //    gripper with a prop already under its TCP would eject it.
        //    (Pre-M0b both plates were born AT the tip; the preset's cube
        //    spawns far from the plates so that tick-1 sweep never met a
        //    body — the golden bit-tests below pin that nothing moved.)
        let ee_link = r.gripper.as_ref().map_or_else(
            || r.joints.last().map_or(root, |j| j.child_link),
            |g| g.attach_link,
        );
        self.ee_body = self.chain[ee_link];
        self.tip_local = r.gripper.as_ref().map_or(Vec3::ZERO, |g| v3(g.tcp_offset));
        let (approach_vec, gap_vec) = (self.approach_vec, self.gap_vec);
        self.fingers = r
            .gripper
            .as_ref()
            .map(|g| {
                let o = v3(r.links[ee_link].origin);
                let centre = o + approach_vec * g.finger_x;
                let side = gap_vec * (g.gap_open * 0.5);
                // U-3 opt-in: the plate as a triangle mesh (the same box,
                // tessellated) instead of the analytic cuboid collider.
                let plate_collider =
                    |g: &crate::robot::GripperSpec| -> Result<Collider, RobotSpecError> {
                        let base = if g.contact_mesh {
                            let (verts, idx) = box_trimesh(g.finger_half);
                            ColliderBuilder::trimesh(verts, idx).map_err(|e| {
                                RobotSpecError::BadShape {
                                    link: "gripper-plate(contact_mesh)".to_string(),
                                    reason: format!("trimesh rejected: {e}"),
                                }
                            })?
                        } else {
                            ColliderBuilder::cuboid(
                                g.finger_half[0],
                                g.finger_half[1],
                                g.finger_half[2],
                            )
                        };
                        Ok(base
                            .friction(g.friction)
                            .collision_groups(arm_groups())
                            .build())
                    };
                let mut mk_finger = |at: Vec3| -> Result<RigidBodyHandle, RobotSpecError> {
                    let body = self.bodies.insert(
                        RigidBodyBuilder::kinematic_position_based()
                            .translation(at)
                            .build(),
                    );
                    let c = plate_collider(g)?;
                    self.colliders.insert_with_parent(c, body, &mut self.bodies);
                    Ok(body)
                };
                let l = mk_finger(centre + side)?;
                let rgt = mk_finger(centre - side)?;
                Ok((l, rgt))
            })
            .transpose()?;
        self.plate_prev_centre = None;

        // 4. Multibody joints in spec (topological) order. Positive
        //    angle about the axis follows the right-hand rule; a
        //    prismatic joint (M3) slides along the unit axis, positive
        //    displacement along +axis, limits in metres.
        //    Adjacent stages overlap at the anchors at build time —
        //    their mutual contact would fight the joint and inject
        //    energy. Joints constrain the pair; the contact between them
        //    is noise (and arm↔arm contact is group-disabled anyway).
        self.stages.clear();
        for j in &r.joints {
            let (generic, slot, axis_vec, anchor, prismatic): (GenericJoint, _, _, _, _) =
                match j.kind {
                    // The legacy revolute stage, call for call (the
                    // golden tests pin this path).
                    JointKind::Revolute => {
                        let axis = v3(j.axis);
                        let builder = RevoluteJointBuilder::new(axis)
                            .local_anchor1(v3(j.anchor))
                            .limits([j.limits.0, j.limits.1])
                            .contacts_enabled(false)
                            .build();
                        (builder.into(), JointAxis::AngX, axis, Vec3::ZERO, false)
                    }
                    JointKind::Prismatic => {
                        let axis = v3(j.axis).normalize();
                        let builder = PrismaticJointBuilder::new(axis)
                            .local_anchor1(v3(j.anchor))
                            .limits([j.limits.0, j.limits.1])
                            .contacts_enabled(false)
                            .build();
                        (builder.into(), JointAxis::LinX, axis, v3(j.anchor), true)
                    }
                };
            if let Some(h) = self.mbody_joints.insert(
                self.chain[j.parent_link],
                self.chain[j.child_link],
                generic,
                true,
            ) {
                self.stages.push(Stage {
                    handle: h,
                    axis_vec,
                    axis: slot,
                    anchor,
                    prismatic,
                });
            }
        }

        // 5. Per-stage servo config: enable the position motor on the
        //    stage's free DoF (rapier leaves `motor_axes` empty — motors
        //    are dead until the axis bit is set; `ANG_X` for revolute,
        //    `LIN_X` for prismatic), ForceBased force model and the
        //    effort clamp; plus this joint's rotor inertia (armature)
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
                    let motor_bit = if s.prismatic {
                        JointAxesMask::LIN_X
                    } else {
                        JointAxesMask::ANG_X
                    };
                    link.joint.data.motor_axes.insert(motor_bit);
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
        Ok(())
    }

    /// Per-tick plate-pair velocity (finite difference of the plate
    /// centre) — 0 before the first drive. Written by
    /// [`Self::drive_fingers`]; consumed by the Contact holding
    /// criterion.
    fn plate_velocity(&self) -> Vec3 {
        self.plate_vel
    }

    /// End-effector (tip) world pose after the last step.
    pub fn ee_pose(&self) -> ([f32; 3], [f32; 4]) {
        self.last_ee
    }

    /// Measured joint coordinates, read from each link's
    /// `local_to_parent` pose — the multibody's own generalized
    /// coordinates, in the same sign convention the joint motors use.
    /// Revolute: the angle about the axis (rad). Prismatic (M3): the
    /// slide displacement (m) — `local_to_parent` translation minus the
    /// joint anchor, projected on the unit axis (the joint frame has no
    /// rotation for a slide, so this is exactly the motor's coordinate).
    pub fn angles(&self) -> Vec<f32> {
        self.stages
            .iter()
            .map(|s| {
                self.mbody_joints
                    .get(s.handle)
                    .and_then(|(mb, link_id)| mb.link(link_id))
                    .map(|link| {
                        let l2p = link.local_to_parent();
                        if s.prismatic {
                            (l2p.translation - s.anchor).dot(s.axis_vec)
                        } else {
                            angle_about(l2p.rotation, s.axis_vec)
                        }
                    })
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
    /// slabs straddling the plate centre in the attach link's frame:
    /// centre = `finger_x` along the approach axis, plates at `±gap/2`
    /// along the gap axis (M0b plate frame; legacy `+X`/`+Z` when the
    /// spec leaves the axes unset). Also refreshes the finite-difference
    /// plate velocity (Contact holding criterion). No-op without a
    /// gripper.
    fn drive_fingers(&mut self, gripper: f32, dt: f32) {
        let Some((fl, fr)) = self.fingers else { return };
        let Some(g) = self.robot.gripper.as_ref() else {
            return;
        };
        let (gap_open, gap_closed, rate, finger_x) =
            (g.gap_open, g.gap_closed, g.gap_rate, g.finger_x);
        let g_target = gap_open + (gap_closed - gap_open) * gripper.clamp(0.0, 1.0);
        self.gap_now += (g_target - self.gap_now).clamp(-rate, rate);
        let (wp, wr) = {
            let b = self.bodies.get(self.ee_body).expect("attach body exists");
            (b.translation(), *b.rotation())
        };
        let centre = wp + wr.mul_vec3(self.approach_vec * finger_x);
        self.plate_vel = match self.plate_prev_centre {
            Some(prev) if dt > 0.0 => (centre - prev) / dt,
            _ => Vec3::ZERO,
        };
        self.plate_prev_centre = Some(centre);
        let side = wr.mul_vec3(self.gap_vec * (self.gap_now * 0.5));
        for (h, sign) in [(fl, 1.0), (fr, -1.0)] {
            if let Some(b) = self.bodies.get_mut(h) {
                b.set_next_kinematic_rotation(wr);
                b.set_next_kinematic_translation(centre + sign * side);
            }
        }
    }

    /// Decide `holding` per the spec's criterion (U-6). Always false
    /// without a gripper.
    ///
    /// - [`HoldingCriterion::TipRadius`] (default, v1): gripper closed +
    ///   a prop centre inside the grip zone; the contact solve does the
    ///   actual holding.
    /// - [`HoldingCriterion::Contact`]: both plates touch the *same*
    ///   prop, each with per-tick normal force ≥ `min_normal_force` and
    ///   plate↔prop relative speed ≤ `max_slip`.
    fn update_holding(&mut self, gripper: f32) {
        let Some(g) = self.robot.gripper.as_ref() else {
            self.holding = false;
            return;
        };
        self.holding = gripper > 0.5
            && match g.holding_criterion {
                HoldingCriterion::TipRadius => {
                    let tip = self.last_ee.0;
                    self.map.values().any(|h| {
                        self.bodies.get(*h).is_some_and(|b| {
                            let t = b.translation();
                            Vec3::new(t.x - tip[0], t.y - tip[1], t.z - tip[2]).length()
                                < GRASP_RADIUS
                        })
                    })
                }
                HoldingCriterion::Contact {
                    min_normal_force,
                    max_slip,
                } => self.contact_hold(min_normal_force, max_slip),
            };
    }

    /// Per-tick normal force (N) a plate collider applies to each prop
    /// body, from the solver's contact impulses: the sum of the pair's
    /// manifold point impulses (signed so positive = pushing the prop)
    /// divided by the physics substep dt. Mixed normals within one
    /// plate-prop pair are near-identical under a squeeze, so the
    /// absolute value after summing is the squeeze force.
    fn plate_prop_forces(&self) -> BTreeMap<EntKey, [f32; 2]> {
        let mut out: BTreeMap<EntKey, [f32; 2]> = BTreeMap::new();
        let Some((fl, fr)) = self.fingers else {
            return out;
        };
        let dt = self.last_substep_dt.max(1e-9);
        let prop_of = |h: RigidBodyHandle| -> Option<EntKey> {
            self.map
                .iter()
                .find(|(_, body)| **body == h)
                .map(|(k, _)| *k)
        };
        for (plate_idx, plate_body) in [fl, fr].into_iter().enumerate() {
            let Some(pb) = self.bodies.get(plate_body) else {
                continue;
            };
            for ch in pb.colliders() {
                for pair in self.narrow_phase.contact_pairs_with(*ch) {
                    let other = if pair.collider1 == *ch {
                        pair.collider2
                    } else {
                        pair.collider1
                    };
                    let Some(other_parent) = self.colliders.get(other).and_then(|c| c.parent())
                    else {
                        continue;
                    };
                    let Some(prop_key) = prop_of(other_parent) else {
                        continue;
                    };
                    let Some(rigid) = pair.rigid() else {
                        continue;
                    };
                    // `impulse` acts on collider1's body along its normal;
                    // the prop feels the opposite when the plate is first.
                    let sign: f32 = if pair.collider1 == *ch { -1.0 } else { 1.0 };
                    let mut sum = 0.0f32;
                    for m in rigid.manifolds.iter() {
                        for p in m.points.iter() {
                            sum += sign * p.data.impulse;
                        }
                    }
                    out.entry(prop_key).or_insert([0.0; 2])[plate_idx] += sum.abs() / dt;
                }
            }
        }
        out
    }

    /// The Contact criterion body: same prop squeezed by both plates
    /// (each ≥ `min_normal_force`) and slow relative to them.
    fn contact_hold(&self, min_normal_force: f32, max_slip: f32) -> bool {
        let pv = self.plate_velocity();
        self.plate_prop_forces().into_iter().any(|(k, forces)| {
            let both_pressed = forces.iter().all(|f| *f >= min_normal_force);
            if !both_pressed {
                return false;
            }
            self.map
                .get(&k)
                .and_then(|h| self.bodies.get(*h))
                .is_some_and(|b| {
                    let v = b.linvel() - pv;
                    v.length() <= max_slip
                })
        })
    }

    /// Legacy (scheme **v1**) experiment fingerprint: sim state + solver
    /// knobs + prop physics + the arm identity *triple*
    /// (`joints[0]` kp/kd, `mass_scale`). This is the value every
    /// pre-M0b receipt (`episode_state_hashes`) and the WP-A golden
    /// constants pin — it is kept bit-for-bit. Its identity coverage is
    /// deliberately thin (per-joint gains, geometry, DoF count and the
    /// gripper are not folded); the complete identity is
    /// [`Self::fingerprint_v2`]. The pure world-state part is
    /// recoverable with [`state_hash_from_v1`] — the fold is a bijection
    /// per round, so old receipts can be compared on the state axis.
    pub fn fingerprint(&self, world: &World) -> u64 {
        let r = &self.robot;
        let (kp, kd) = r.joints.first().map_or((0.0, 0.0), |j| (j.kp, j.kd));
        let h = sim_fingerprint(world);
        mix(
            mix(mix(h, kp.to_bits() as u64), kd.to_bits() as u64),
            r.mass_scale.to_bits() as u64,
        )
    }

    /// Scheme **v2** fingerprint (M0b, PLAN §9.1 P-10): the pure state
    /// hash ([`sim_fingerprint`]) folded with the *complete* robot
    /// identity ([`robot_identity_hash`]: N, every joint's gains /
    /// armature / damping / limits / axis / anchor, every link's
    /// geometry and mass, the gripper summary incl. the plate frame) and
    /// the canonical spec hash when the caller has one (`None` folds a
    /// sentinel, so "no spec" never collides with a real hash). Two
    /// robots that converge to identical states with different
    /// per-joint gains are different experiments here, where v1 could
    /// not tell. Receipts carry `fingerprint_scheme = "v2"` next to it.
    pub fn fingerprint_v2(&self, world: &World, spec_hash: Option<u64>) -> u64 {
        fingerprint_v2_from_parts(
            sim_fingerprint(world),
            robot_identity_hash(&self.robot),
            spec_hash,
        )
    }

    /// The robot-identity part of [`Self::fingerprint_v2`].
    pub fn identity_hash(&self) -> u64 {
        robot_identity_hash(&self.robot)
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

    /// Current plate gap (m) — the ramped `gap_now` (M1 leftover: a
    /// read-only view so renderers stop mirroring the ramp).
    pub fn gap(&self) -> f32 {
        self.gap_now
    }

    /// World positions of the gripper plates `(left, right)`, or `None`
    /// without a gripper.
    pub fn plate_positions(&self) -> Option<[[f32; 3]; 2]> {
        let (fl, fr) = self.fingers?;
        let at = |h: RigidBodyHandle| {
            self.bodies.get(h).map(|b| {
                let t = b.translation();
                [t.x, t.y, t.z]
            })
        };
        Some([at(fl)?, at(fr)?])
    }

    /// Solver-side mass properties of chain link `link` (U-1
    /// acceptance readback); `None` for an out-of-range index.
    pub fn link_mass_properties(&self, link: usize) -> Option<LinkMassReadback> {
        let h = *self.chain.get(link)?;
        let b = self.bodies.get(h)?;
        let mp = &b.mass_properties().local_mprops;
        let pi = mp.principal_inertia();
        let rf = mp.principal_inertia_local_frame;
        Some(LinkMassReadback {
            mass: mp.mass(),
            com: [mp.local_com.x, mp.local_com.y, mp.local_com.z],
            principal_inertia: [pi.x, pi.y, pi.z],
            principal_frame: [rf.x, rf.y, rf.z, rf.w],
        })
    }

    /// The multibody's live armature (reflected rotor inertia) of joint
    /// `i` — proof the per-joint value really entered the solver.
    pub fn joint_armature(&self, i: usize) -> f32 {
        self.stages
            .get(i)
            .and_then(|s| self.mbody_joints.get(s.handle))
            .and_then(|(mb, link_id)| {
                let a = mb.link(link_id).map(|l| l.assembly_id())?;
                Some(mb.armature()[a])
            })
            .unwrap_or(f32::NAN)
    }

    /// Number of link↔link contact pairs currently touching (both
    /// colliders on chain bodies, at least one contact point) — the
    /// self-collision acceptance readout (`AdjacentExempt` must show
    /// non-adjacent contacts; `Off` must show none).
    pub fn self_contact_pairs(&self) -> usize {
        let chain: Vec<RigidBodyHandle> = self.chain.clone();
        self.narrow_phase
            .contact_pairs()
            .filter(|pair| {
                let p1 = self.colliders.get(pair.collider1).and_then(|c| c.parent());
                let p2 = self.colliders.get(pair.collider2).and_then(|c| c.parent());
                let both_on_chain = p1.is_some_and(|h| chain.contains(&h))
                    && p2.is_some_and(|h| chain.contains(&h));
                both_on_chain
                    && pair
                        .rigid()
                        .is_some_and(|r| r.manifolds.iter().any(|m| !m.points.is_empty()))
            })
            .count()
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
        // Optional per-prop collider shape (M0a: props from the spec);
        // absent ⇒ the legacy 5 cm cube.
        let shapes: BTreeMap<EntKey, PropShape> = world
            .query::<PropShape>()
            .map(|(e, s)| (key(e), *s))
            .collect();
        for (k, p) in &live {
            if !self.map.contains_key(k) {
                let pp = phys.get(k).copied().unwrap_or_default();
                let shape = shapes.get(k).copied().unwrap_or_default();
                let groups = match self.robot.self_collision {
                    SelfCollision::Off => world_groups(),
                    SelfCollision::AdjacentExempt => exempt_world_groups(&self.robot),
                };
                let b = RigidBodyBuilder::dynamic()
                    .linear_damping(0.1)
                    .angular_damping(0.1)
                    .translation(Vec3::new(p.pos[0], p.pos[1], p.pos[2]))
                    .build();
                let h = self.bodies.insert(b);
                let c = prop_collider(shape, pp, groups);
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
                            link.joint.data.set_motor_position(s.axis, want, j.kp, j.kd);
                        }
                    }
                    self.last_cmd[i] = want;
                }
            }
        }

        // 3.5 Drive gripper plates (kinematic) for THIS tick's step.
        self.drive_fingers(gripper, dt);

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
        self.last_substep_dt = params.dt;
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
    ///     `MultibodySystem::new` — the same script through
    ///     `from_robot(RobotSpec::from(&desc))` must match it exactly.
    /// (B) 390-tick IK grasp → lift → release script — the contact-
    ///     friction carry path.
    ///
    /// 平台契约（M1 前台实证 + CI 三平台首跑）：两条脚本**同构建内**
    /// 逐位一致（new ≡ from_robot(preset)）；但与预 M0a 主机快照常量
    /// 的对拍只在锚定机（windows-gnu · rustc 1.99 · rapier 0.36.1）
    /// 成立——跨编译器 f32 libm/求和序差 1 ulp 是设计内差异
    /// （contracts/golden/embodied_fingerprints.pre-m0a.json 口径同此）。
    /// 故拆成：本测试管等价（CI 三平台跑），
    /// [`golden_constants_match_pre_m0a_snapshot`]（--ignored）管主机
    /// 快照对账。
    #[test]
    fn golden_default_path_new_equals_from_robot_preset() {
        // (A) 240-tick swing script: `new` vs `from_robot(preset)`.
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
        assert_eq!(hex(sys2.fingerprint(&world2)), hex(sys.fingerprint(&world)));
    }

    /// 主机锚快照对账（--ignored，锚定机手动跑）：
    /// cargo test -p oriera-subsys --features rigid3d,cam golden -- --ignored
    /// 常量捕获环境 = windows-gnu · rustc 1.99 · rapier 0.36.1（M0a-WPA
    /// 落快照时记录）。跨编译器跑必红（设计内，见上）。
    #[test]
    #[ignore = "host-anchored snapshot: run on the anchor host (windows-gnu, rustc 1.99, rapier 0.36.1) with --ignored"]
    fn golden_constants_match_pre_m0a_snapshot() {
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

        // (B) the 390-tick IK grasp/lift/release script, folded continuously
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
            inertial: None,
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
                inertial: None,
                mesh_uri: None,
            },
            link("carrier", [0.015; 3], [sx, sy, 0.0], [0.0; 3]),
            link("upper_arm", [0.1, r, r], [sx, sy, 0.0], [0.1, 0.0, 0.0]),
            link(
                "forearm",
                [0.09, r, r],
                [sx + 0.2, sy, 0.0],
                [0.09, 0.0, 0.0],
            ),
            link(
                "wrist",
                [0.03, r, r],
                [sx + 0.38, sy, 0.0],
                [0.03, 0.0, 0.0],
            ),
            link("roll", [0.02, r, r], [sx + 0.44, sy, 0.0], [0.02, 0.0, 0.0]),
            link(
                "flange",
                [0.015, r, r],
                [sx + 0.48, sy, 0.0],
                [0.015, 0.0, 0.0],
            ),
        ];
        let j = |name: &str,
                 p: usize,
                 c: usize,
                 anchor: [f32; 3],
                 axis: [f32; 3],
                 limits: (f32, f32),
                 kp: f32,
                 kd: f32| {
            JointSpec {
                name: name.into(),
                kind: JointKind::Revolute,
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
            j(
                "base_yaw",
                0,
                1,
                [0.0, 0.12 - base_h, 0.0],
                [0.0, 1.0, 0.0],
                (-PI, PI),
                60.0,
                4.0,
            ),
            j(
                "shoulder",
                1,
                2,
                [0.0; 3],
                [0.0, 0.0, 1.0],
                (-0.35, 1.45),
                60.0,
                4.0,
            ),
            j(
                "elbow",
                2,
                3,
                [0.2, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                (-2.5, 0.35),
                50.0,
                3.0,
            ),
            j(
                "wrist_pitch",
                3,
                4,
                [0.18, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                (-2.0, 2.0),
                30.0,
                2.0,
            ),
            j(
                "wrist_roll",
                4,
                5,
                [0.06, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                (-PI, PI),
                25.0,
                2.0,
            ),
            j(
                "wrist_yaw",
                5,
                6,
                [0.04, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                (-2.0, 2.0),
                25.0,
                2.0,
            ),
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
            approach_axis: None,
            gap_axis: None,
            contact_mesh: false,
            holding_criterion: HoldingCriterion::default(),
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
            self_collision: SelfCollision::Off,
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

    /// M2 mesh semantics: a `mesh_uri` is provenance metadata (builds
    /// fine next to a primitive), a resolved `ConvexHull` builds a real
    /// hull collider, and `collision_report` speaks human.
    #[test]
    fn mesh_uri_is_metadata_and_hull_shapes_build() {
        // A box link with a leftover mesh reference builds the box.
        let mut spec = RobotSpec::from(&ArmDescription::default());
        spec.links[2].mesh_uri = Some("file:///panda/link2.stl".into());
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
        let mut sys =
            MultibodySystem::from_robot(spec, Scene3D::default()).expect("uri is metadata");
        for i in 0..60u64 {
            sys.run(&ctx(i), &mut world);
        }
        assert!(telemetry(&world, arm).ee_pos.iter().all(|v| v.is_finite()));

        // An 8-vertex cube as ConvexHull: builds, keeps the arm
        // tracking, and the hull volume lands in the report.
        let s = 0.05f32;
        let mut pts = Vec::new();
        for x in [-s, s] {
            for y in [-s, s] {
                for z in [-s, s] {
                    pts.push([0.1 + x, y, z]);
                }
            }
        }
        let mut hull = RobotSpec::from(&ArmDescription::default());
        hull.links[3].shape = LinkShape::ConvexHull { points: pts };
        let report = collision_report(&hull);
        assert!(
            report.contains("convex_hull 8 points"),
            "report must name the hull: {report}"
        );
        assert!(report.contains("upper_arm"), "report names links: {report}");
        let sys2 = MultibodySystem::from_robot(hull, Scene3D::default()).expect("hull builds");
        let vol = collision_report(&RobotSpec::from(&ArmDescription::default()));
        assert!(vol.contains("box"), "primitive path reports too: {vol}");
        let _ = sys2;

        // A degenerate hull (coplanar) is a named build error.
        let mut flat = RobotSpec::from(&ArmDescription::default());
        flat.links[3].shape = LinkShape::ConvexHull {
            points: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
        };
        let err = match MultibodySystem::from_robot(flat, Scene3D::default()) {
            Err(e) => e,
            Ok(_) => panic!("coplanar hull must be refused"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("forearm") && msg.contains("degenerate"),
            "{msg}"
        );
    }

    /// U-1: a spec `inertial` replaces the whole mass model — the body
    /// readback carries exactly the (mass-scaled) spec mass/CoM, and
    /// the armature readback mirrors the per-joint spec values.
    #[test]
    fn spec_inertial_reaches_the_rigid_body() {
        let mut spec = RobotSpec::from(&ArmDescription::default());
        let links = [2usize, 3, 4];
        for (k, i) in links.iter().enumerate() {
            spec.links[*i].inertial = Some(Inertial {
                mass: 1.0 + k as f32 * 0.5,
                com: [0.01 * (k as f32 + 1.0), 0.0, 0.002],
                ixx: 0.01 + k as f32 * 0.01,
                iyy: 0.02 + k as f32 * 0.01,
                izz: 0.03 + k as f32 * 0.01,
                ixy: 0.001,
                ixz: 0.0,
                iyz: 0.0,
            });
        }
        spec.joints[2].armature = 0.077;
        let mut sys =
            MultibodySystem::from_robot(spec.clone(), Scene3D::default()).expect("builds");
        // rapier defers the mass-properties recompute to the first
        // pipeline step (user-changes flush) — settle briefly first.
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
        for i in 0..5u64 {
            sys.run(&ctx(i), &mut world);
        }
        for (k, i) in links.iter().enumerate() {
            let mp = sys.link_mass_properties(*i).expect("readback");
            let want_m = (1.0 + k as f32 * 0.5) * spec.mass_scale;
            assert!(
                (mp.mass - want_m).abs() / want_m < 1e-5,
                "link {} mass {} != spec {}",
                spec.links[*i].name,
                mp.mass,
                want_m
            );
            // The collider contributed zero: total mass == spec mass.
            let com_want = [0.01 * (k as f32 + 1.0), 0.0, 0.002];
            assert!(
                mp.com
                    .iter()
                    .zip(com_want)
                    .all(|(a, b)| (a - b).abs() < 1e-5),
                "link {} com {:?} != spec {com_want:?}",
                spec.links[*i].name,
                mp.com
            );
            // Reconstruct the body-frame tensor from the principal
            // readback and compare against the spec tensor.
            let t = reconstruct_tensor(mp.principal_inertia, mp.principal_frame);
            let want = |ixx: f32, iyy: f32, izz: f32, ixy: f32, ixz: f32, iyz: f32| {
                [[ixx, ixy, ixz], [ixy, iyy, iyz], [ixz, iyz, izz]]
            };
            let w = want(
                0.01 + k as f32 * 0.01,
                0.02 + k as f32 * 0.01,
                0.03 + k as f32 * 0.01,
                0.001,
                0.0,
                0.0,
            );
            for r in 0..3 {
                for c in 0..3 {
                    let scale = w[r][c].abs().max(1e-3);
                    assert!(
                        (t[r][c] - w[r][c]).abs() <= 1e-4 * scale.max(0.1),
                        "link {} tensor[{}][{}] {} != {}",
                        spec.links[*i].name,
                        r,
                        c,
                        t[r][c],
                        w[r][c]
                    );
                }
            }
        }
        // Untouched link keeps the density-derived model (≈0.6 kg + 0.2).
        let mp = sys.link_mass_properties(1).expect("readback");
        assert!((mp.mass - (LINK_DENSITY * 2.7e-5 + LINK_ADDED_MASS)).abs() < 1e-3);
        // Armature: per-joint spec value live in the multibody.
        assert!((sys.joint_armature(2) - 0.077).abs() < 1e-6);
        assert!((sys.joint_armature(1) - spec.joints[1].armature).abs() < 1e-6);
    }

    fn reconstruct_tensor(principal: [f32; 3], frame: [f32; 4]) -> [[f32; 3]; 3] {
        use rapier3d::utils::RotationOps;
        let r = Rotation::from_xyzw(frame[0], frame[1], frame[2], frame[3]);
        let m = r.to_mat();
        // The stored frame maps body axes → principal axes (parry:
        // "rotation from local coordinates to principal inertia
        // axes"), so the body-frame tensor is Rᵀ · diag(λ) · R. Using
        // the same accessor for both factors and writing the transposed
        // cell handles the convention.
        let mut t = [[0.0f32; 3]; 3];
        let rows = [m.row(0), m.row(1), m.row(2)];
        for (a, row_a) in rows.iter().enumerate() {
            for (b, row_b) in rows.iter().enumerate() {
                let mut s = 0.0;
                for k in 0..3 {
                    s += row_a[k] * principal[k] * row_b[k];
                }
                t[a][b] = s;
            }
        }
        t
    }

    /// U-9 self collision: with `AdjacentExempt`, folding the elbow
    /// fully back puts the forearm onto the base column — a
    /// NON-adjacent pair — and the solver reports contacts; the same
    /// posture under `Off` reports none.
    #[test]
    fn adjacent_exempt_enables_non_adjacent_self_contacts() {
        let base = RobotSpec::from(&ArmDescription::default());
        let build = |mode: SelfCollision| {
            let mut spec = base.clone();
            spec.self_collision = mode;
            MultibodySystem::from_robot(spec, Scene3D::default()).expect("builds")
        };
        // Elbow folded hard against the base column (limits −2.5).
        let posture = [0.0f32, 1.0, -2.5, 1.2];
        let run = |sys: &mut MultibodySystem| {
            let mut world = World::new();
            world.insert_resource(Gravity(9.8));
            world.insert_resource(TimeScale(1.0));
            let arm = world.spawn();
            world.insert(
                arm,
                JointTargets {
                    angles: posture.to_vec(),
                    gripper: 0.0,
                },
            );
            world.insert(arm, JointTelemetry::default());
            for i in 0..120u64 {
                sys.run(&ctx(i), &mut world);
            }
            let tel = world
                .query::<JointTelemetry>()
                .next()
                .map(|(_, t)| t.clone());
            (tel, sys.self_contact_pairs())
        };
        let mut off = build(SelfCollision::Off);
        let mut exempt = build(SelfCollision::AdjacentExempt);
        let (tel_off, off_pairs) = run(&mut off);
        let (tel_ex, exempt_pairs) = run(&mut exempt);
        eprintln!(
            "self-collision probe: Off pairs={off_pairs}, AdjacentExempt pairs={exempt_pairs}"
        );
        assert_eq!(off_pairs, 0, "Off must keep arm↔arm contacts disabled");
        assert!(
            exempt_pairs > 0,
            "AdjacentExempt must surface non-adjacent contacts (elbow folded onto base)"
        );
        // The telemetry stays finite under the new contacts.
        assert!(tel_off.is_some() && tel_ex.is_some());
        // Identity: the mode is v2 identity, folded only when on.
        let h_off = robot_identity_hash(&base);
        assert_eq!(
            h_off,
            robot_identity_hash(&{
                let mut s = base.clone();
                s.self_collision = SelfCollision::Off;
                s
            })
        );
        assert_ne!(
            h_off,
            robot_identity_hash(&{
                let mut s = base.clone();
                s.self_collision = SelfCollision::AdjacentExempt;
                s
            })
        );
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

    // --- M0b: plate frame + fingerprint scheme v2 -----------------------

    /// `GripperSpec::{approach_axis, gap_axis}` place the plates: the
    /// pair's centre sits `finger_x` along the approach axis and the
    /// plates separate along the gap axis, both in the attach frame.
    /// Explicitly written defaults land exactly where omitted ones do.
    #[test]
    fn oriented_plates_follow_the_spec_axes() {
        let settle = |spec: RobotSpec| {
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
            sys
        };
        // Rotated frame: approach along local +Y, gap along local +X.
        let mut spec = RobotSpec::from(&ArmDescription::default());
        {
            let g = spec.gripper.as_mut().unwrap();
            g.approach_axis = Some([0.0, 2.0, 0.0]); // non-unit on purpose
            g.gap_axis = Some([1.0, 0.0, 0.0]);
        }
        let (finger_x, gap_open) = {
            let g = spec.gripper.as_ref().unwrap();
            (g.finger_x, g.gap_open)
        };
        let sys = settle(spec);
        let (plates, _) = sys.debug_contact_state();
        let b = sys.bodies.get(sys.ee_body).expect("attach body");
        let (wp, wr) = (b.translation(), *b.rotation());
        let centre = wp + wr.mul_vec3(Vec3::new(0.0, finger_x, 0.0));
        let mid = Vec3::new(
            (plates[0][0] + plates[1][0]) * 0.5,
            (plates[0][1] + plates[1][1]) * 0.5,
            (plates[0][2] + plates[1][2]) * 0.5,
        );
        assert!(
            (mid - centre).length() < 2e-3,
            "plate centre must follow the approach axis: {mid:?} vs {centre:?}"
        );
        let sep = Vec3::new(
            plates[0][0] - plates[1][0],
            plates[0][1] - plates[1][1],
            plates[0][2] - plates[1][2],
        );
        let want = wr.mul_vec3(Vec3::new(gap_open, 0.0, 0.0));
        assert!(
            (sep - want).length() < 2e-3,
            "plates must separate along the gap axis: {sep:?} vs {want:?}"
        );

        // Explicit legacy axes == omitted axes, to the bit.
        let omitted = settle(RobotSpec::from(&ArmDescription::default()));
        let mut explicit_spec = RobotSpec::from(&ArmDescription::default());
        {
            let g = explicit_spec.gripper.as_mut().unwrap();
            g.approach_axis = Some([1.0, 0.0, 0.0]);
            g.gap_axis = Some([0.0, 0.0, 1.0]);
        }
        let explicit = settle(explicit_spec);
        assert_eq!(
            omitted.debug_contact_state().0,
            explicit.debug_contact_state().0,
            "explicit +X/+Z must be the legacy placement bit for bit"
        );
    }

    /// The v1 fold is invertible per round for a known operand: the
    /// pure state hash comes back out of a v1 receipt, so pre-M0b
    /// anchors compare on the state axis.
    #[test]
    fn v1_fingerprint_unfolds_to_the_state_hash() {
        // Round-trip of the primitive on arbitrary operands.
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..64 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let h = seed;
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let x = seed;
            assert_eq!(mix_inverse_h(mix(h, x), x), h);
        }
        // And on a real system after a scripted run.
        let (mut world, mut sys, arm, _obj) = bench([0.30, 0.55, 0.0]);
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.angles = vec![0.2, 0.6, -0.7, 0.1];
            t.gripper = 1.0;
        }
        for i in 0..90u64 {
            sys.run(&ctx(i), &mut world);
        }
        let j0 = &sys.robot.joints[0];
        let recovered =
            state_hash_from_v1(sys.fingerprint(&world), j0.kp, j0.kd, sys.robot.mass_scale);
        assert_eq!(recovered, sim_fingerprint(&world));
        // The v2 fold is a pure function of (state, identity, spec).
        assert_eq!(
            sys.fingerprint_v2(&world, Some(0xabc)),
            fingerprint_v2_from_parts(sim_fingerprint(&world), sys.identity_hash(), Some(0xabc))
        );
        assert_ne!(
            sys.fingerprint_v2(&world, Some(0xabc)),
            sys.fingerprint_v2(&world, None)
        );
        assert_ne!(sys.fingerprint_v2(&world, None), sys.fingerprint(&world));
    }

    /// Scheme v2 sees what v1 is blind to: a per-joint gain on a joint
    /// other than the first, a link's geometry, the DoF count and the
    /// plate frame all move the identity; spelling a default axis out
    /// does not.
    #[test]
    fn identity_hash_covers_joints_links_and_gripper() {
        let base = RobotSpec::from(&ArmDescription::default());
        let h0 = robot_identity_hash(&base);
        assert_eq!(h0, robot_identity_hash(&base.clone()), "stable");

        let mut kp1 = base.clone();
        kp1.joints[1].kp += 1.0;
        assert_ne!(robot_identity_hash(&kp1), h0, "joint[1] kp is identity");
        // …while the v1 triple cannot tell (same joints[0], same mass_scale).
        let w = World::new();
        let s_base = MultibodySystem::from_robot(base.clone(), Scene3D::default()).unwrap();
        let s_kp1 = MultibodySystem::from_robot(kp1, Scene3D::default()).unwrap();
        assert_eq!(
            s_base.fingerprint(&w),
            s_kp1.fingerprint(&w),
            "v1 blind spot (documented)"
        );
        assert_ne!(
            s_base.fingerprint_v2(&w, None),
            s_kp1.fingerprint_v2(&w, None)
        );

        let mut geom = base.clone();
        geom.links[2].collider_offset[0] += 0.001;
        assert_ne!(robot_identity_hash(&geom), h0, "link geometry is identity");

        let mut dof = base.clone();
        dof.joints[3].armature += 0.01;
        assert_ne!(robot_identity_hash(&dof), h0, "armature is identity");

        let mut axes = base.clone();
        axes.gripper.as_mut().unwrap().approach_axis = Some([0.0, -1.0, 0.0]);
        assert_ne!(robot_identity_hash(&axes), h0, "plate frame is identity");

        let mut explicit = base.clone();
        {
            let g = explicit.gripper.as_mut().unwrap();
            g.approach_axis = Some([1.0, 0.0, 0.0]);
            g.gap_axis = Some([0.0, 0.0, 1.0]);
        }
        assert_eq!(
            robot_identity_hash(&explicit),
            h0,
            "explicit default axes are the same identity"
        );

        let mut renamed = base;
        renamed.links[1].name = "carrier-renamed".into();
        renamed.joints[0].name = "yaw".into();
        assert_eq!(robot_identity_hash(&renamed), h0, "names are not physics");
    }

    // --- M3: generic fixtures (any count, per-slab friction) ------------

    /// The legacy `Scene3D` is the two-slab list at 0.9 (one slab
    /// without a table), and building that list through the generic
    /// entry point is trajectory-identical to `from_robot(.., Scene3D)`
    /// — the default world did not move a bit.
    #[test]
    fn legacy_scene_is_the_two_slab_fixture_list() {
        let scene = Scene3D::default();
        let slabs = scene.fixture_slabs();
        assert_eq!(slabs.len(), 2);
        assert_eq!((slabs[0].center, slabs[0].half), scene.ground);
        assert_eq!(Some((slabs[1].center, slabs[1].half)), scene.table);
        assert!(slabs.iter().all(|s| s.friction == LEGACY_FIXTURE_FRICTION));
        assert_eq!(LEGACY_FIXTURE_FRICTION, 0.9);
        let no_table = Scene3D {
            table: None,
            ..Scene3D::default()
        };
        assert_eq!(no_table.fixture_slabs().len(), 1);

        let script = |arm: Entity| {
            move |i: u64, w: &mut World| {
                if i.is_multiple_of(30) {
                    let a = (i as f32) * 0.004;
                    if let Some(tg) = w.get_mut::<JointTargets>(arm) {
                        tg.angles = vec![a, 0.5 + a, -0.8 - a, 0.2];
                        tg.gripper = if (i / 30) % 2 == 1 { 1.0 } else { 0.0 };
                    }
                }
            }
        };
        let (mut w1, mut s1, arm1, _) = bench([0.30, 0.55, 0.0]);
        let (h1, cps1) = fold_episode(&mut s1, &mut w1, 240, &script(arm1));
        let (mut w2, _old, arm2, _) = bench([0.30, 0.55, 0.0]);
        let mut s2 = MultibodySystem::from_robot_fixtures(
            RobotSpec::from(&ArmDescription::default()),
            &Scene3D::default().fixture_slabs(),
        )
        .expect("legacy slabs build");
        let (h2, cps2) = fold_episode(&mut s2, &mut w2, 240, &script(arm2));
        assert_eq!(
            (hex(h1), hexs(&cps1)),
            (hex(h2), hexs(&cps2)),
            "generic ≠ legacy path"
        );
    }

    /// Fixture friction of every built slab, in insertion order (the
    /// fixed bodies' colliders) — the structural readback.
    fn slab_frictions(sys: &MultibodySystem) -> Vec<f32> {
        let mut out = Vec::new();
        for (h, b) in sys.bodies.iter() {
            if !b.is_fixed() || sys.chain.contains(&h) {
                continue;
            }
            for c in b.colliders() {
                if let Some(col) = sys.colliders.get(*c) {
                    out.push(col.friction());
                }
            }
        }
        out
    }

    /// Any number of slabs, each with its own friction: a raised shelf
    /// carries a prop at its own top, and a cube kicked along a
    /// slippery slab slides measurably farther than on a grippy one.
    #[test]
    fn extra_fixtures_build_with_their_own_friction() {
        // A shelf out of the arm's reach (z = 0.8), top at y = 0.3.
        let shelf = |friction: f32| FixtureSlab {
            center: [0.0, 0.25, 0.8],
            half: [0.4, 0.05, 0.15],
            friction,
        };
        let world_with = |friction: f32| {
            let mut slabs = Scene3D::default().fixture_slabs();
            slabs.push(shelf(friction));
            // a 4th and 5th slab just to exercise "any count"
            slabs.push(FixtureSlab {
                center: [-0.8, 0.1, -0.8],
                half: [0.1, 0.1, 0.1],
                friction: 0.3,
            });
            slabs.push(FixtureSlab {
                center: [-0.8, 0.1, 0.8],
                half: [0.1, 0.1, 0.1],
                friction: 1.7,
            });
            slabs
        };
        let run = |friction: f32| {
            let (mut world, _sys, _arm, obj) = bench([0.0, 0.33, 0.8]);
            let mut sys = MultibodySystem::from_robot_fixtures(
                RobotSpec::from(&ArmDescription::default()),
                &world_with(friction),
            )
            .expect("five slabs build");
            assert_eq!(
                slab_frictions(&sys),
                vec![0.9, 0.9, friction, 0.3, 1.7],
                "every slab keeps its own friction, in list order"
            );
            for i in 0..90u64 {
                sys.run(&ctx(i), &mut world);
            }
            let rest = pose(&world, obj);
            // shelf top 0.3 + cube half 0.025
            assert!(
                (rest.pos[1] - 0.325).abs() < 0.01,
                "cube must rest on the shelf slab, y = {}",
                rest.pos[1]
            );
            // Kick it along +X and let friction stop it.
            let h = *sys.map.values().next().expect("prop body");
            if let Some(b) = sys.bodies.get_mut(h) {
                b.set_linvel(Vec3::new(1.0, 0.0, 0.0), true);
            }
            for i in 90..150u64 {
                sys.run(&ctx(i), &mut world);
            }
            pose(&world, obj).pos[0] - rest.pos[0]
        };
        let slippery = run(0.05);
        let grippy = run(1.2);
        eprintln!("kick travel: slippery {slippery:.3} m vs grippy {grippy:.3} m");
        assert!(
            slippery > grippy * 1.5 && grippy > 0.0,
            "per-slab friction must change the slide: slippery {slippery} vs grippy {grippy}"
        );
    }

    /// An unbuildable slab is a named error carrying its list index;
    /// an empty list is a legal furniture-free world.
    #[test]
    fn bad_fixture_is_a_named_error_and_empty_world_builds() {
        let robot = RobotSpec::from(&ArmDescription::default());
        let mut slabs = Scene3D::default().fixture_slabs();
        slabs.push(FixtureSlab {
            center: [0.0; 3],
            half: [0.1, -0.1, 0.1],
            friction: 0.5,
        });
        match MultibodySystem::from_robot_fixtures(robot.clone(), &slabs) {
            Err(RobotSpecError::BadFixture { index, reason }) => {
                assert_eq!(index, 2);
                assert!(reason.contains("half"), "{reason}");
            }
            Err(e) => panic!("expected BadFixture, got {e}"),
            Ok(_) => panic!("negative half extent must be refused"),
        }
        slabs[2].half = [0.1; 3];
        slabs[2].friction = f32::NAN;
        assert!(matches!(
            MultibodySystem::from_robot_fixtures(robot.clone(), &slabs),
            Err(RobotSpecError::BadFixture { index: 2, .. })
        ));
        assert!(MultibodySystem::from_robot_fixtures(robot, &[]).is_ok());
    }
}
