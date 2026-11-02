//! Robot specification (PLAN-embodied-general §3.1, M0a): a generic
//! N-stage serial arm plus an optional gripper, described as data.
//!
//! [`RobotSpec`] is what [`crate::rigid3d::MultibodySystem`] builds
//! from. [`crate::rigid3d::ArmDescription`] — the 4-DoF tabletop arm
//! every existing spec/CLI/Python path speaks — becomes a *preset*: a
//! [`From`] projection onto `RobotSpec` that reproduces the legacy
//! construction field by field (same bodies, same insertion order, same
//! anchors/axes/limits, same gripper constants), so the default
//! trajectory is bit-identical to the pre-M0a engine (pinned by the
//! golden tests in `rigid3d`).
//!
//! **M0a scope (honest)**: fixed-base revolute chains, box/capsule
//! primitive colliders, one parallel-plate gripper. `mesh_uri` is
//! *registered* here (so URDF import can carry it) but the builder
//! refuses it — mesh colliders are M2. `home` / `action_scale` are
//! carried for the wrapper layers (Python / spec) and not consumed by
//! the physics build. `effort` on the gripper is registered for the
//! contact-force gripper; the M0a kinematic plates ignore it.

use crate::rigid3d::{
    ArmDescription, ARMATURE, FINGER_FRICTION, FINGER_GAP_CLOSED, FINGER_GAP_OPEN, FINGER_GAP_RATE,
    FINGER_HALF, FINGER_X, GRIPPER_EFFORT, JOINT_DAMPING, JOINT_NAMES, LINK_ADDED_MASS,
    LINK_DENSITY,
};
use std::collections::BTreeSet;
use std::fmt;

/// Collider primitive of one link, in the link body's local frame.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LinkShape {
    /// Capsule along local +X: `half_len` is half the segment length.
    Capsule { half_len: f32, radius: f32 },
    /// Axis-aligned box with the given half extents.
    Box { half: [f32; 3] },
}

/// One rigid link of the chain.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LinkSpec {
    pub name: String,
    pub shape: LinkShape,
    /// World position of the link body frame at the home configuration
    /// (all joint angles zero, identity rotations). Stored explicitly
    /// rather than re-derived by forward kinematics so a preset can pin
    /// the exact float the legacy builder used (`(bx + l0) + l1` and
    /// `bx + (l0 + l1)` may differ by an ulp — and the receipt is
    /// bit-exact). For a consistent chain `origin[child] ==
    /// origin[parent] + anchor` at home.
    pub origin: [f32; 3],
    /// Collider translation inside the body frame (e.g. mid-link).
    pub collider_offset: [f32; 3],
    /// Collider density, kg/m³. Scaled by [`RobotSpec::mass_scale`] on
    /// dynamic links; ignored by the solver on the fixed root.
    pub density: f32,
    /// Contact friction coefficient of the link collider.
    pub friction: f32,
    /// Extra body mass (kg) on top of the collider's — keeps light
    /// stages inside the solver's healthy mass range. Scaled by
    /// `mass_scale`; not applied to the fixed root.
    pub added_mass: f32,
    /// Mesh asset for a mesh collider. **M0a registers it only** — the
    /// builder returns [`RobotSpecError::MeshNotSupported`] (M2 consumes
    /// it). Carried so URDF import can round-trip the reference.
    pub mesh_uri: Option<String>,
}

/// One actuated revolute joint: parent → child, anchor and axis in the
/// parent link's frame (the child frame coincides with the parent's at
/// home, so the axis is the same vector in both).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct JointSpec {
    pub name: String,
    pub parent_link: usize,
    pub child_link: usize,
    /// Joint position in the parent body's local frame.
    pub anchor: [f32; 3],
    /// Revolute axis in the parent body's local frame (unit vector).
    pub axis: [f32; 3],
    /// Revolute limits (rad), `lo <= hi`.
    pub limits: (f32, f32),
    /// Motor torque clamp (N·m). `None` ⇒ the legacy
    /// [`crate::rigid3d::MOTOR_MAX_FORCE`] (80 N·m).
    pub effort: Option<f32>,
    /// Target-angle rate limit (rad/s): each tick the commanded target
    /// may move at most `v · dt` from the previous command. `None` ⇒
    /// unlimited (the legacy behaviour).
    pub velocity_limit: Option<f32>,
    /// ForceBased PD gains (N·m/rad, N·m·s/rad).
    pub kp: f32,
    pub kd: f32,
    /// Reflected rotor inertia on this DoF (kg·m²).
    pub armature: f32,
    /// Passive viscous joint friction on this DoF (N·m·s/rad).
    pub damping: f32,
}

/// Gripper mechanism kind. M0a: one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GripperKind {
    /// Two kinematic plates straddling the TCP, driven along the
    /// attach link's gap axis (legacy: local Z); the prop stays dynamic
    /// and plate/prop friction does the holding.
    ParallelPlates,
}

/// Legacy plate approach direction (attach-link frame): the plate
/// centre sits `finger_x` along this axis. `GripperSpec::approach_axis
/// = None` means this.
pub const GRIPPER_APPROACH_DEFAULT: [f32; 3] = [1.0, 0.0, 0.0];
/// Legacy plate separation direction (attach-link frame): the two
/// plates straddle the centre at `±gap/2` along this axis.
/// `GripperSpec::gap_axis = None` means this.
pub const GRIPPER_GAP_DEFAULT: [f32; 3] = [0.0, 0.0, 1.0];

/// Gripper attached to one link of the chain.
///
/// **Plate frame (M0b)**: the two plates live in the attach link's
/// frame. `approach_axis` is where the plates (and the TCP) point —
/// the centre of the plate pair sits `finger_x` along it; `gap_axis`
/// is the direction the plates separate along (`±gap/2`). Both are
/// optional: `None` keeps the legacy `+X` / `+Z` placement, so every
/// pre-M0b spec, the 4-DoF preset and its spec_hash are unchanged
/// (the fields are omitted from the serialized form when `None`). An
/// external gripper whose plates hang *down* from an unrotated flange
/// writes e.g. `approach_axis: [0, 0, -1]` with the flange rolled so
/// local −Z is world down — see the `gripper_down` tests.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GripperSpec {
    /// Link index the plates (and the TCP) ride on.
    pub attach_link: usize,
    /// Plate-centre offset along the approach axis (m) — legacy name
    /// kept (`+X` was the only approach axis before M0b).
    pub finger_x: f32,
    /// Tool-centre-point offset in the attach link's local frame — the
    /// end-effector pose reported in telemetry.
    pub tcp_offset: [f32; 3],
    /// Plate half extents (m), in the plate's (= attach link's) frame.
    pub finger_half: [f32; 3],
    /// Plate gap (m) at demand 0 / demand 1.
    pub gap_open: f32,
    pub gap_closed: f32,
    /// Plate contact friction.
    pub friction: f32,
    /// Grip force budget (N). Registered for the contact-force gripper
    /// (M2); the kinematic ParallelPlates drive does not consume it.
    pub effort: f32,
    /// Max gap change per engine tick (m/tick) — the plate ramp.
    pub gap_rate: f32,
    pub kind: GripperKind,
    /// Plate approach direction in the attach link's frame (need not be
    /// unit length; normalized at build). `None` ⇒
    /// [`GRIPPER_APPROACH_DEFAULT`] (`+X`, the legacy placement).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub approach_axis: Option<[f32; 3]>,
    /// Plate separation direction in the attach link's frame. `None` ⇒
    /// [`GRIPPER_GAP_DEFAULT`] (`+Z`, the legacy placement).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub gap_axis: Option<[f32; 3]>,
}

impl GripperSpec {
    /// The approach axis actually used (legacy `+X` when unset).
    pub fn approach_axis_or_default(&self) -> [f32; 3] {
        self.approach_axis.unwrap_or(GRIPPER_APPROACH_DEFAULT)
    }

    /// The gap axis actually used (legacy `+Z` when unset).
    pub fn gap_axis_or_default(&self) -> [f32; 3] {
        self.gap_axis.unwrap_or(GRIPPER_GAP_DEFAULT)
    }

    /// Canonical form for identity hashing: an explicitly written
    /// default axis is the same gripper as an omitted one.
    pub fn canonicalize_axes(&mut self) {
        if self.approach_axis == Some(GRIPPER_APPROACH_DEFAULT) {
            self.approach_axis = None;
        }
        if self.gap_axis == Some(GRIPPER_GAP_DEFAULT) {
            self.gap_axis = None;
        }
    }
}

/// The robot: links, joints (in topological order — every joint's
/// parent is the root or an earlier joint's child), optional gripper,
/// and the wrapper-layer knobs (`home`, `action_scale`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RobotSpec {
    pub name: String,
    /// World position of the robot mount (informational; link origins
    /// are absolute).
    pub base: [f32; 3],
    pub links: Vec<LinkSpec>,
    pub joints: Vec<JointSpec>,
    pub gripper: Option<GripperSpec>,
    /// Home joint angles (rad), one per joint. Carried for the env
    /// wrappers (initial `JointTargets`); the physics builds at the
    /// zero configuration like the legacy engine.
    pub home: Vec<f32>,
    /// Uniform link-mass scale (DR knob), 1.0 = nominal.
    pub mass_scale: f32,
    /// Per-joint action scale (rad per unit action) for the wrapper
    /// layers; not consumed by the physics build.
    pub action_scale: Vec<f32>,
}

/// Why a [`RobotSpec`] cannot be built.
#[derive(Debug, Clone, PartialEq)]
pub enum RobotSpecError {
    /// A link carries `mesh_uri` — mesh colliders arrive in M2.
    MeshNotSupported { link: String, uri: String },
    NoLinks,
    NoJoints,
    /// Joint references a link index out of range or parent == child.
    BadLinkRef { joint: String },
    /// Not exactly one root (a link that is no joint's child).
    RootCount(usize),
    /// A link is the child of more than one joint.
    MultiParent { link: String },
    /// Joint order is not topological: its parent is not yet placed.
    NotTopological { joint: String },
    /// Zero-length revolute axis.
    ZeroAxis { joint: String },
    /// `limits.0 > limits.1`.
    BadLimits { joint: String },
    /// `home` / `action_scale` length ≠ joint count (when non-empty).
    VectorLength { field: &'static str, got: usize, want: usize },
    /// Non-finite or negative gain / inertia / damping / effort.
    BadGain { joint: String },
    /// Non-positive `mass_scale` or link density / non-finite mass.
    BadMass { what: String },
    /// Gripper attach link out of range / gap or rate inconsistent.
    BadGripper(String),
}

impl fmt::Display for RobotSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MeshNotSupported { link, uri } => write!(
                f,
                "link `{link}` references mesh `{uri}`: mesh colliders are not supported in M0a \
                 (registered only; mesh collision lands in M2 — use a Box/Capsule primitive for now)"
            ),
            Self::NoLinks => write!(f, "robot has no links"),
            Self::NoJoints => write!(f, "robot has no joints"),
            Self::BadLinkRef { joint } => {
                write!(f, "joint `{joint}` references an invalid link (out of range or parent == child)")
            }
            Self::RootCount(n) => write!(
                f,
                "robot must have exactly one root link (no joint's child), found {n}"
            ),
            Self::MultiParent { link } => {
                write!(f, "link `{link}` is the child of more than one joint")
            }
            Self::NotTopological { joint } => write!(
                f,
                "joint `{joint}` is out of topological order (its parent link is not yet placed)"
            ),
            Self::ZeroAxis { joint } => write!(f, "joint `{joint}` has a zero-length axis"),
            Self::BadLimits { joint } => write!(f, "joint `{joint}` has limits.0 > limits.1"),
            Self::VectorLength { field, got, want } => {
                write!(f, "`{field}` has {got} entries, expected {want} (one per joint)")
            }
            Self::BadGain { joint } => write!(
                f,
                "joint `{joint}` has a non-finite or negative kp/kd/armature/damping/effort/velocity_limit"
            ),
            Self::BadMass { what } => write!(f, "{what} must be finite and positive"),
            Self::BadGripper(msg) => write!(f, "gripper: {msg}"),
        }
    }
}

impl std::error::Error for RobotSpecError {}

impl RobotSpec {
    /// Index of the root link: the one link that is no joint's child.
    /// `None` when there is not exactly one such link.
    pub fn root_link(&self) -> Option<usize> {
        let children: BTreeSet<usize> = self.joints.iter().map(|j| j.child_link).collect();
        let mut roots = (0..self.links.len()).filter(|i| !children.contains(i));
        let r = roots.next()?;
        if roots.next().is_some() {
            return None;
        }
        Some(r)
    }

    /// Number of actuated joints.
    pub fn dof(&self) -> usize {
        self.joints.len()
    }

    /// Joint names in `JointTargets`/`JointTelemetry` order.
    pub fn joint_names(&self) -> Vec<&str> {
        self.joints.iter().map(|j| j.name.as_str()).collect()
    }

    /// Structural validation — everything the builder relies on.
    pub fn validate(&self) -> Result<(), RobotSpecError> {
        if self.links.is_empty() {
            return Err(RobotSpecError::NoLinks);
        }
        if self.joints.is_empty() {
            return Err(RobotSpecError::NoJoints);
        }
        for l in &self.links {
            if let Some(uri) = &l.mesh_uri {
                return Err(RobotSpecError::MeshNotSupported {
                    link: l.name.clone(),
                    uri: uri.clone(),
                });
            }
            if !(l.density.is_finite() && l.density > 0.0) {
                return Err(RobotSpecError::BadMass {
                    what: format!("link `{}` density", l.name),
                });
            }
            if !(l.added_mass.is_finite() && l.added_mass >= 0.0) {
                return Err(RobotSpecError::BadMass {
                    what: format!("link `{}` added_mass", l.name),
                });
            }
        }
        if !(self.mass_scale.is_finite() && self.mass_scale > 0.0) {
            return Err(RobotSpecError::BadMass {
                what: "mass_scale".into(),
            });
        }
        let n = self.links.len();
        let mut seen_child = BTreeSet::new();
        for j in &self.joints {
            if j.parent_link >= n || j.child_link >= n || j.parent_link == j.child_link {
                return Err(RobotSpecError::BadLinkRef {
                    joint: j.name.clone(),
                });
            }
            if !seen_child.insert(j.child_link) {
                return Err(RobotSpecError::MultiParent {
                    link: self.links[j.child_link].name.clone(),
                });
            }
            let len2 = j.axis.iter().map(|v| v * v).sum::<f32>();
            if !(len2.is_finite() && len2 > 1e-12) {
                return Err(RobotSpecError::ZeroAxis {
                    joint: j.name.clone(),
                });
            }
            if j.limits.0 > j.limits.1 {
                return Err(RobotSpecError::BadLimits {
                    joint: j.name.clone(),
                });
            }
            let ok = |v: f32| v.is_finite() && v >= 0.0;
            if !(ok(j.kp) && ok(j.kd) && ok(j.armature) && ok(j.damping))
                || j.effort.is_some_and(|e| !ok(e))
                || j.velocity_limit.is_some_and(|v| !ok(v))
            {
                return Err(RobotSpecError::BadGain {
                    joint: j.name.clone(),
                });
            }
        }
        let root = match self.root_link() {
            Some(r) => r,
            None => {
                let children: BTreeSet<usize> =
                    self.joints.iter().map(|j| j.child_link).collect();
                let count = (0..n).filter(|i| !children.contains(i)).count();
                return Err(RobotSpecError::RootCount(count));
            }
        };
        let mut placed = BTreeSet::from([root]);
        for j in &self.joints {
            if !placed.contains(&j.parent_link) {
                return Err(RobotSpecError::NotTopological {
                    joint: j.name.clone(),
                });
            }
            placed.insert(j.child_link);
        }
        let dof = self.joints.len();
        if !self.home.is_empty() && self.home.len() != dof {
            return Err(RobotSpecError::VectorLength {
                field: "home",
                got: self.home.len(),
                want: dof,
            });
        }
        if !self.action_scale.is_empty() && self.action_scale.len() != dof {
            return Err(RobotSpecError::VectorLength {
                field: "action_scale",
                got: self.action_scale.len(),
                want: dof,
            });
        }
        if let Some(g) = &self.gripper {
            if g.attach_link >= n {
                return Err(RobotSpecError::BadGripper(format!(
                    "attach_link {} out of range ({} links)",
                    g.attach_link, n
                )));
            }
            if !(g.gap_open.is_finite() && g.gap_closed.is_finite() && g.gap_closed <= g.gap_open)
            {
                return Err(RobotSpecError::BadGripper(
                    "gap_closed must be <= gap_open".into(),
                ));
            }
            if !(g.gap_rate.is_finite() && g.gap_rate > 0.0) {
                return Err(RobotSpecError::BadGripper("gap_rate must be > 0".into()));
            }
            if !(g.friction.is_finite() && g.friction >= 0.0) {
                return Err(RobotSpecError::BadGripper("friction must be >= 0".into()));
            }
            // Plate frame axes: finite, non-degenerate and not parallel
            // (the plates must be able to straddle the approach line).
            let unit_ok = |a: [f32; 3]| {
                let len2 = a.iter().map(|v| v * v).sum::<f32>();
                len2.is_finite() && len2 > 1e-12
            };
            let ap = g.approach_axis_or_default();
            let gp = g.gap_axis_or_default();
            if !unit_ok(ap) {
                return Err(RobotSpecError::BadGripper(
                    "approach_axis must be a finite non-zero vector (omit it for +X)".into(),
                ));
            }
            if !unit_ok(gp) {
                return Err(RobotSpecError::BadGripper(
                    "gap_axis must be a finite non-zero vector (omit it for +Z)".into(),
                ));
            }
            let cross = [
                ap[1] * gp[2] - ap[2] * gp[1],
                ap[2] * gp[0] - ap[0] * gp[2],
                ap[0] * gp[1] - ap[1] * gp[0],
            ];
            let cross2 = cross.iter().map(|v| v * v).sum::<f32>();
            let n2 = ap.iter().map(|v| v * v).sum::<f32>() * gp.iter().map(|v| v * v).sum::<f32>();
            if cross2 <= 1e-6 * n2 {
                return Err(RobotSpecError::BadGripper(
                    "approach_axis and gap_axis must not be parallel".into(),
                ));
            }
        }
        Ok(())
    }
}

/// The 4-DoF tabletop preset: a field-by-field projection of the legacy
/// [`ArmDescription`] builder. Every number below is the *same
/// expression* the pre-M0a `build_arm` evaluated, so the resulting
/// bodies/colliders/joints are bit-identical (the golden tests in
/// `rigid3d` pin this).
///
/// Links: `[base(fixed column), yaw_carrier, upper_arm, forearm, wrist]`;
/// joints: `JOINT_NAMES[0..4]` = base_yaw(Y) / shoulder(Z) / elbow(Z) /
/// wrist_pitch(Z); gripper: the `FINGER_*` plates on the wrist.
impl From<&ArmDescription> for RobotSpec {
    fn from(d: &ArmDescription) -> Self {
        let shoulder_y = d.base[1] + d.shoulder_height;
        let r = d.link_radius;
        let base_h = d.shoulder_height * 0.5;
        let at_shoulder = [d.base[0], shoulder_y, d.base[2]];
        let dynamic = |name: &str, half: [f32; 3], origin: [f32; 3], off: [f32; 3]| LinkSpec {
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
            // Fixed base column: the legacy collider set neither density
            // nor friction — these are rapier's defaults (Density(1.0),
            // 0.5), written out so the generic builder reproduces them.
            LinkSpec {
                name: "base".into(),
                shape: LinkShape::Box {
                    half: [r * 1.8, base_h, r * 1.8],
                },
                origin: [d.base[0], d.base[1] + base_h, d.base[2]],
                collider_offset: [0.0; 3],
                density: 1.0,
                friction: 0.5,
                added_mass: 0.0,
                mesh_uri: None,
            },
            dynamic("yaw_carrier", [0.015, 0.015, 0.015], at_shoulder, [0.0; 3]),
            dynamic(
                "upper_arm",
                [d.links[0] * 0.5, r, r],
                at_shoulder,
                [d.links[0] * 0.5, 0.0, 0.0],
            ),
            dynamic(
                "forearm",
                [d.links[1] * 0.5, r, r],
                [d.base[0] + d.links[0], shoulder_y, d.base[2]],
                [d.links[1] * 0.5, 0.0, 0.0],
            ),
            dynamic(
                "wrist",
                [d.links[2] * 0.5, r, r],
                [d.base[0] + d.links[0] + d.links[1], shoulder_y, d.base[2]],
                [d.links[2] * 0.5, 0.0, 0.0],
            ),
        ];
        let damping = d.passive_damping.unwrap_or(JOINT_DAMPING);
        let joint = |i: usize, parent: usize, child: usize, anchor: [f32; 3], axis: [f32; 3]| {
            JointSpec {
                name: JOINT_NAMES[i].into(),
                parent_link: parent,
                child_link: child,
                anchor,
                axis,
                limits: d.limits[i],
                effort: None,
                velocity_limit: None,
                kp: d.kp,
                kd: d.kd,
                armature: ARMATURE,
                damping,
            }
        };
        let joints = vec![
            // base body centre sits shoulder_height/2 below the shoulder
            // anchor — the anchor is LOCAL to the base body.
            joint(0, 0, 1, [0.0, d.shoulder_height - base_h, 0.0], [0.0, 1.0, 0.0]),
            joint(1, 1, 2, [0.0; 3], [0.0, 0.0, 1.0]),
            joint(2, 2, 3, [d.links[0], 0.0, 0.0], [0.0, 0.0, 1.0]),
            joint(3, 3, 4, [d.links[1], 0.0, 0.0], [0.0, 0.0, 1.0]),
        ];
        let gripper = Some(GripperSpec {
            attach_link: 4,
            finger_x: FINGER_X,
            tcp_offset: [d.links[2], 0.0, 0.0],
            finger_half: FINGER_HALF,
            gap_open: FINGER_GAP_OPEN,
            gap_closed: FINGER_GAP_CLOSED,
            friction: FINGER_FRICTION,
            effort: GRIPPER_EFFORT,
            gap_rate: FINGER_GAP_RATE,
            kind: GripperKind::ParallelPlates,
            approach_axis: None,
            gap_axis: None,
        });
        RobotSpec {
            name: "tabletop-4dof".into(),
            base: d.base,
            links,
            joints,
            gripper,
            home: d.home.to_vec(),
            mass_scale: d.mass_scale,
            action_scale: vec![2.0; 4],
        }
    }
}

impl From<ArmDescription> for RobotSpec {
    fn from(d: ArmDescription) -> Self {
        Self::from(&d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_projects_the_legacy_arm() {
        let d = ArmDescription::default();
        let r = RobotSpec::from(&d);
        assert!(r.validate().is_ok(), "{:?}", r.validate());
        assert_eq!(r.links.len(), 5);
        assert_eq!(r.dof(), 4);
        assert_eq!(r.root_link(), Some(0));
        assert_eq!(r.joint_names(), &JOINT_NAMES[0..4]);
        assert_eq!(r.joints[0].axis, [0.0, 1.0, 0.0]);
        for j in &r.joints[1..] {
            assert_eq!(j.axis, [0.0, 0.0, 1.0]);
        }
        for (j, lim) in r.joints.iter().zip(d.limits) {
            assert_eq!(j.limits, lim);
            assert_eq!((j.kp, j.kd, j.armature, j.damping), (d.kp, d.kd, ARMATURE, JOINT_DAMPING));
            assert_eq!((j.effort, j.velocity_limit), (None, None));
        }
        let g = r.gripper.as_ref().unwrap();
        assert_eq!(g.attach_link, 4);
        assert_eq!(g.finger_x, FINGER_X);
        assert_eq!(g.tcp_offset, [d.links[2], 0.0, 0.0]);
        assert_eq!((g.gap_open, g.gap_closed), (FINGER_GAP_OPEN, FINGER_GAP_CLOSED));
        // M0b: the preset leaves the plate frame at the legacy +X/+Z.
        assert_eq!((g.approach_axis, g.gap_axis), (None, None));
        assert_eq!(g.approach_axis_or_default(), GRIPPER_APPROACH_DEFAULT);
        assert_eq!(g.gap_axis_or_default(), GRIPPER_GAP_DEFAULT);
        assert_eq!(r.action_scale, vec![2.0; 4]);
        assert_eq!(r.home, d.home.to_vec());
        // passive_damping override flows into every joint.
        let d2 = ArmDescription {
            passive_damping: Some(0.3),
            ..ArmDescription::default()
        };
        assert!(RobotSpec::from(&d2).joints.iter().all(|j| j.damping == 0.3));
    }

    #[test]
    fn validation_catches_structural_errors() {
        let ok = RobotSpec::from(&ArmDescription::default());
        let mut two_roots = ok.clone();
        two_roots.joints.remove(0);
        assert_eq!(two_roots.validate(), Err(RobotSpecError::RootCount(2)));

        let mut bad_order = ok.clone();
        bad_order.joints.swap(1, 2);
        assert!(matches!(
            bad_order.validate(),
            Err(RobotSpecError::NotTopological { .. })
        ));

        let mut dup = ok.clone();
        dup.joints[3].child_link = 2;
        assert!(matches!(dup.validate(), Err(RobotSpecError::MultiParent { .. })));

        let mut zero_axis = ok.clone();
        zero_axis.joints[1].axis = [0.0; 3];
        assert!(matches!(zero_axis.validate(), Err(RobotSpecError::ZeroAxis { .. })));

        let mut home_len = ok.clone();
        home_len.home = vec![0.0; 3];
        assert!(matches!(
            home_len.validate(),
            Err(RobotSpecError::VectorLength { field: "home", got: 3, want: 4 })
        ));

        let mut grip = ok.clone();
        grip.gripper.as_mut().unwrap().attach_link = 9;
        assert!(matches!(grip.validate(), Err(RobotSpecError::BadGripper(_))));

        let mut mesh = ok;
        mesh.links[2].mesh_uri = Some("file:///panda/link2.stl".into());
        let err = mesh.validate().unwrap_err();
        assert!(matches!(err, RobotSpecError::MeshNotSupported { .. }));
        assert!(err.to_string().contains("M2"), "{err}");
    }

    /// M0b plate frame: zero / non-finite / parallel axes are refused
    /// with a message naming the field; any non-parallel pair passes,
    /// and an explicitly written default canonicalizes to `None` (so
    /// the identity hash cannot fork on spelling).
    #[test]
    fn gripper_axes_are_validated_and_canonicalized() {
        let ok = RobotSpec::from(&ArmDescription::default());
        let with = |ap: Option<[f32; 3]>, gp: Option<[f32; 3]>| {
            let mut r = ok.clone();
            let g = r.gripper.as_mut().unwrap();
            g.approach_axis = ap;
            g.gap_axis = gp;
            r
        };
        assert!(with(Some([0.0, 0.0, -1.0]), Some([0.0, 1.0, 0.0])).validate().is_ok());
        assert!(with(Some([0.0, -2.0, 0.0]), None).validate().is_ok(), "non-unit is fine");
        let err = with(Some([0.0; 3]), None).validate().unwrap_err().to_string();
        assert!(err.contains("approach_axis"), "{err}");
        let err = with(None, Some([f32::NAN, 0.0, 0.0])).validate().unwrap_err().to_string();
        assert!(err.contains("gap_axis"), "{err}");
        let err = with(Some([0.0, 0.0, 1.0]), None).validate().unwrap_err().to_string();
        assert!(err.contains("parallel"), "{err}");
        let err = with(Some([1.0, 0.0, 0.0]), Some([-3.0, 0.0, 0.0]))
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("parallel"), "{err}");

        let mut explicit = with(Some(GRIPPER_APPROACH_DEFAULT), Some(GRIPPER_GAP_DEFAULT));
        explicit.gripper.as_mut().unwrap().canonicalize_axes();
        assert_eq!(explicit, ok, "explicit default axes canonicalize to None");
        let mut custom = with(Some([0.0, 0.0, -1.0]), None);
        custom.gripper.as_mut().unwrap().canonicalize_axes();
        assert_eq!(custom.gripper.as_ref().unwrap().approach_axis, Some([0.0, 0.0, -1.0]));
    }
}
