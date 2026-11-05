//! The scene as data (M0a G-3 + M3-A): the [`SceneGraph`] — fixtures,
//! props with shapes and placements, and the spec-declared calibrated
//! cameras — plus [`Lcg`], the episode RNG whose draw order is part of
//! the episode receipt. Serde derives ride the `spec` feature (which
//! turns on subsys' serde for RobotSpec/PropShape/PropPhys/Scene3D).

use oriera_subsys::rigid3d::{PropShape, SlideSpec};
use oriera_subsys::{PropPhys, Scene3D};

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
#[cfg_attr(
    feature = "spec",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
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
    pub(crate) fn sample(&self, rng: &mut Lcg) -> [f32; 3] {
        match *self {
            Self::Range { x, y, z } => [rng.next_f32(x[0], x[1]), y, rng.next_f32(z[0], z[1])],
            Self::Fixed(p) => p,
        }
    }
}

/// One dynamic prop: shape + nominal physics + where it spawns.
/// `props[0]` is the *task object* (the one the obs/reward channels
/// track); any further props spawn as scene clutter. An optional
/// `slide` (P2-W6B) turns the prop into a *slidable fixture panel* —
/// a dynamic body on a prismatic rail to a static anchor at its spawn
/// pose (the drawer front of `drawer-4dof-v2`).
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
    /// Slidable-fixture rail (`subsys::SlideSpec`): `None` (the
    /// default, omitted from serialization) keeps the plain free body
    /// and every pre-P2 spec hash bit-identical; `Some` = prismatic
    /// joint to a static anchor frozen at the spawn pose.
    #[cfg_attr(
        feature = "spec",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub slide: Option<SlideSpec>,
}

// ---------------------------------------------------------------------------
// Cameras (M3-A, PLAN §四 M3 / §9.1 U-5): calibrated pinhole cameras live
// in the spec — intrinsics + Brown–Conrady distortion + extrinsics, with
// an optional end-effector mount. Empty (the default) keeps the legacy
// `--cam` orthographic top view byte-for-byte; anything here renders
// through `oriera_subsys::camera::render_pinhole` and exports as extra
// LeRobot video/depth tracks. The serde derives ride the `spec` feature
// like the neighbours; the *rendering* additionally needs the `cam`
// feature — a spec with cameras loads and validates either way.
// ---------------------------------------------------------------------------

/// Brown–Conrady lens distortion (radial `k1`/`k2` + tangential
/// `p1`/`p2`). All-zero (the default) = ideal pinhole.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(default, deny_unknown_fields))]
pub struct DistortionSpec {
    pub k1: f32,
    pub k2: f32,
    pub p1: f32,
    pub p2: f32,
}

impl Default for DistortionSpec {
    fn default() -> Self {
        Self {
            k1: 0.0,
            k2: 0.0,
            p1: 0.0,
            p2: 0.0,
        }
    }
}

/// Output channels of a camera (at least one must be on — validate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(default, deny_unknown_fields))]
pub struct CamChannels {
    /// RGB8 PNG track `observation.images.{name}`.
    pub rgb: bool,
    /// Millimetre u16 PNG track `depth.{name}` (0 = no return).
    pub depth: bool,
}

impl Default for CamChannels {
    fn default() -> Self {
        Self {
            rgb: true,
            depth: false,
        }
    }
}

/// Where `pos`/`look_at`/`up` are expressed. `world` = metres in the
/// world frame; `ee` = offsets in the **end-effector (TCP) frame** —
/// origin at `telemetry.ee_pos`, rotation `telemetry.ee_rot`, so a
/// wrist camera rides the arm and the extrinsics are recomputed every
/// tick. The telemetry frame's +X is the gripper approach axis of the
/// 4-DoF preset (world-down at the pick posture).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(rename_all = "snake_case"))]
pub enum CamAttach {
    /// Fixed world-frame mount (overhead shots).
    #[default]
    World,
    /// Wrist mount: `pos`/`look_at`/`up` are EE-frame offsets.
    Ee,
}

/// The payload of [`CameraKind::Pinhole`]: intrinsics + extrinsics +
/// channels of one calibrated camera. Pixel convention is OpenCV's
/// (`u = fx·X/Z + cx` is the pixel index, `(cx, cy)` ≈ centre), depth
/// is camera-frame forward distance in millimetres as u16 PNG.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(default, deny_unknown_fields))]
pub struct PinholeSpec {
    /// Image width in pixels (≤ 640 — a soft memory cap, validate).
    pub width: u32,
    /// Image height in pixels (≤ 640).
    pub height: u32,
    /// Focal length in horizontal pixels (> 0).
    pub fx: f32,
    /// Focal length in vertical pixels (> 0).
    pub fy: f32,
    /// Principal point column (pixels).
    pub cx: f32,
    /// Principal point row (pixels).
    pub cy: f32,
    /// Lens distortion (default ideal).
    pub dist: DistortionSpec,
    /// Camera origin — world frame, or EE-frame offset when
    /// `attach: "ee"` (metres).
    pub pos: [f32; 3],
    /// The point the optical axis aims at (same frame as `pos`).
    pub look_at: [f32; 3],
    /// Head-up hint (same frame as `pos`; must not be parallel to the
    /// view direction — the image-right direction is `up × look`).
    pub up: [f32; 3],
    /// Which tracks this camera exports (at least one).
    pub channels: CamChannels,
    /// Frame of `pos`/`look_at`/`up` (default world).
    pub attach: CamAttach,
}

impl Default for PinholeSpec {
    /// A sane default: a 160×120 overhead photo of the bench centre.
    fn default() -> Self {
        Self {
            width: 160,
            height: 120,
            fx: 140.0,
            fy: 140.0,
            cx: 80.0,
            cy: 60.0,
            dist: DistortionSpec::default(),
            pos: [0.32, 1.10, 0.0],
            look_at: [0.32, 0.50, 0.0],
            up: [0.0, 0.0, 1.0],
            channels: CamChannels::default(),
            attach: CamAttach::World,
        }
    }
}

/// The camera model. One variant today (`pinhole`); externally tagged
/// like `Placement`/`LinkShape` so `extends` overlays can swap models
/// wholesale (`merge_overlay`'s single-key replacement rule).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(rename_all = "snake_case"))]
pub enum CameraKind {
    Pinhole(PinholeSpec),
}

/// One spec-declared camera (`scene.cameras[i]`): a name (the LeRobot
/// feature suffix) + its model.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(deny_unknown_fields))]
pub struct CameraSpec {
    /// LeRobot feature suffix: `observation.images.{name}` /
    /// `depth.{name}`. Must be non-empty and unique in the scene.
    pub name: String,
    /// The camera model (pinhole intrinsics/extrinsics).
    pub kind: CameraKind,
}

impl CameraSpec {
    /// The pinhole payload (`kind` is `pinhole` today; the accessor
    /// keeps call sites honest if a second model ever lands).
    pub fn pinhole(&self) -> &PinholeSpec {
        match &self.kind {
            CameraKind::Pinhole(p) => p,
        }
    }
}

/// The scene as data: static fixtures + dynamic props + cameras. The
/// default is exactly the pre-M0a hardcoded bench (ground slab + table +
/// 5 cm cube at `OBJ_RANGE`); `cameras` is omitted from the serialized
/// form when empty so every pre-M3 spec keeps its spec_hash.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "spec", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "spec", serde(deny_unknown_fields))]
pub struct SceneGraph {
    pub fixtures: Vec<Fixture>,
    pub props: Vec<PropSpec>,
    /// Calibrated cameras (M3-A). Empty/absent ⇒ the legacy `--cam`
    /// orthographic top view and an unchanged spec hash.
    #[cfg_attr(
        feature = "spec",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub cameras: Vec<CameraSpec>,
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
                slide: None,
            }],
            cameras: Vec::new(),
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

    /// Full fixture list as L6 slabs（M3：from_robot_fixtures 用）。
    pub fn to_fixture_slabs(&self) -> Vec<oriera_subsys::rigid3d::FixtureSlab> {
        self.fixtures
            .iter()
            .map(|f| oriera_subsys::rigid3d::FixtureSlab {
                center: f.center,
                half: f.half,
                friction: f.friction,
            })
            .collect()
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
        if self.fixtures.is_empty() {
            return Err("scene.fixtures: 至少要有一个 fixture（fixtures[0] 是地面）".into());
        }
        // M3：from_robot_fixtures 吃任意个数、各自摩擦的 fixture（M0a
        // 的 1–2 限制随引擎面解锁一并解除）。
        for (i, f) in self.fixtures.iter().enumerate() {
            if !(f.center.iter().all(|v| v.is_finite()) && f.half.iter().all(|v| ok(*v))) {
                return Err(format!(
                    "scene.fixtures[{i}]（{}）: center 须有限、half 须为正数（half = {:?}）",
                    f.name, f.half
                ));
            }
            if !(f.friction.is_finite() && f.friction >= 0.0) {
                return Err(format!(
                    "scene.fixtures[{i}]（{}）.friction = {}: 须为有限非负数",
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
            // P2-W6B slide rail: finite non-zero axis, [lo, hi] travel
            // with lo ≤ hi, non-negative damping (G-7: name the field).
            if let Some(s) = &p.slide {
                let norm2 = s.axis[0] * s.axis[0] + s.axis[1] * s.axis[1] + s.axis[2] * s.axis[2];
                if !(s.axis.iter().all(|v| v.is_finite()) && norm2 > 1e-12) {
                    return Err(format!(
                        "scene.props[{i}]（{}）.slide.axis = {:?}：须为有限非零向量（世界系滑轴，构建时归一化）",
                        p.name, s.axis
                    ));
                }
                if !(s.limits[0].is_finite()
                    && s.limits[1].is_finite()
                    && s.limits[0] <= s.limits[1])
                {
                    return Err(format!(
                        "scene.props[{i}]（{}）.slide.limits = {:?}：须为 [lo, hi] 且 lo ≤ hi（米，行程零点 = 出生位；[0, 0.1] = 出生位闭合、可拉出 10 cm）",
                        p.name, s.limits
                    ));
                }
                if !(s.damping.is_finite() && s.damping >= 0.0) {
                    return Err(format!(
                        "scene.props[{i}]（{}）.slide.damping = {}：须为有限非负数（N·s/m，滑轨阻尼 = 滑动摩擦模型；0 = 无摩擦滑轨）",
                        p.name, s.damping
                    ));
                }
            }
        }
        // M3-A cameras: intrinsics sane, ≤640 px (soft memory cap),
        // at least one channel, names unique (they are LeRobot feature
        // keys), extrinsics non-degenerate (frame-invariant — the
        // checks hold for `attach: "ee"` offsets too).
        let mut cam_names: Vec<&str> = Vec::new();
        for (i, c) in self.cameras.iter().enumerate() {
            if c.name.trim().is_empty() {
                return Err(format!(
                    "scene.cameras[{i}].name：不能为空（相机名是 LeRobot 特性键后缀 observation.images.<name> / depth.<name>）"
                ));
            }
            if cam_names.contains(&c.name.as_str()) {
                return Err(format!(
                    "scene.cameras[{i}].name = {:?}：重复——相机名必须唯一（已有 {cam_names:?}）",
                    c.name
                ));
            }
            cam_names.push(&c.name);
            let p = c.pinhole();
            let px = |v: f32| v.is_finite() && v > 0.0;
            if !px(p.fx) || !px(p.fy) {
                return Err(format!(
                    "scene.cameras[{i}]（{}）.fx/fy = {}/{}：须为有限正数（像素焦距；缺省 140）",
                    c.name, p.fx, p.fy
                ));
            }
            if p.width == 0 || p.height == 0 || p.width > 640 || p.height > 640 {
                return Err(format!(
                    "scene.cameras[{i}]（{}）.width/height = {}×{}：须在 1..=640（软上限防内存；更大分辨率属研究模式自渲染，不入 spec）",
                    c.name, p.width, p.height
                ));
            }
            if !p.channels.rgb && !p.channels.depth {
                return Err(format!(
                    "scene.cameras[{i}]（{}）.channels：rgb/depth 至少开一个（当前两个都 false，渲染不出任何轨）",
                    c.name
                ));
            }
            // P0-8（20 角色审查 QA-3）：cx/cy 此前完全不设防（-1e9 也过）；
            // name 直接拼进 LeRobot 视频目录/特性键，非法字符会在写盘时才炸。
            if !(p.cx.is_finite() && p.cy.is_finite()) {
                return Err(format!(
                    "scene.cameras[{i}]（{}）.cx/cy = ({}, {})：主点须为有限数（像素系，通常 ≈ 宽高的一半）",
                    c.name, p.cx, p.cy
                ));
            }
            if c.name.is_empty()
                || !c
                    .name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
            {
                return Err(format!(
                    "scene.cameras[{i}].name = {:?}：须非空且只含 [A-Za-z0-9_.-]（name 会拼进 LeRobot 特性键与视频目录路径）",
                    c.name
                ));
            }
            let finite3 = |v: [f32; 3]| v.iter().all(|x| x.is_finite());
            if !finite3(p.pos) || !finite3(p.look_at) || !finite3(p.up) {
                return Err(format!(
                    "scene.cameras[{i}]（{}）.pos/look_at/up：须为有限坐标（世界系，或 attach: \"ee\" 时的末端系偏移）",
                    c.name
                ));
            }
            let view = [
                p.look_at[0] - p.pos[0],
                p.look_at[1] - p.pos[1],
                p.look_at[2] - p.pos[2],
            ];
            if view.iter().all(|v| v.abs() <= 1e-9) {
                return Err(format!(
                    "scene.cameras[{i}]（{}）.look_at == pos：视线方向为零向量（look_at 须与 pos 不同点）",
                    c.name
                ));
            }
            let up_cross = [
                p.up[1] * view[2] - p.up[2] * view[1],
                p.up[2] * view[0] - p.up[0] * view[2],
                p.up[0] * view[1] - p.up[1] * view[0],
            ];
            if up_cross.iter().all(|v| v.abs() <= 1e-6) {
                return Err(format!(
                    "scene.cameras[{i}]（{}）.up 与视线平行：图像右方向不定（换一个 up，俯拍常用 [0,0,1]）",
                    c.name
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // M3：>2 个 fixture 与任意非负摩擦已解锁（from_robot_fixtures）
        // ——旧「M0a 只支持 1–2 个」报错退场；负摩擦仍拒绝。
        let mut g = SceneGraph::default();
        g.fixtures.push(Fixture {
            name: "shelf".into(),
            center: [0.4, 0.9, 0.0],
            half: [0.2, 0.02, 0.2],
            friction: 0.9,
        });
        g.fixtures[1].friction = 0.5;
        assert!(g.validate().is_ok(), "M3: 3 fixtures + friction 0.5 legal");
        let mut g = SceneGraph::default();
        g.fixtures[1].friction = -0.1;
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

    /// P2-W6B slide rail: the default scene has none; a well-formed
    /// rail validates; every bad field is a named error (G-7).
    #[test]
    fn slide_spec_validates_and_defaults_stay_rail_free() {
        let g = SceneGraph::default();
        assert!(g.props.iter().all(|p| p.slide.is_none()));
        let mut g = SceneGraph::default();
        g.props[0].slide = Some(SlideSpec {
            axis: [-1.0, 0.0, 0.0],
            limits: [0.0, 0.10],
            damping: 8.0,
        });
        assert!(g.validate().is_ok(), "{:?}", g.validate());
        let ok = SlideSpec {
            axis: [-1.0, 0.0, 0.0],
            limits: [0.0, 0.10],
            damping: 8.0,
        };
        let bad = |s: SlideSpec, word: &str| {
            let mut g = SceneGraph::default();
            g.props[0].slide = Some(s);
            let err = g.validate().unwrap_err();
            assert!(err.contains("slide") && err.contains(word), "{word}: {err}");
        };
        bad(
            SlideSpec {
                axis: [f32::NAN, 0.0, 0.0],
                ..ok
            },
            "axis",
        );
        bad(
            SlideSpec {
                axis: [0.0, 0.0, 0.0],
                ..ok
            },
            "axis",
        );
        bad(
            SlideSpec {
                limits: [0.2, -0.1],
                ..ok
            },
            "limits",
        );
        bad(
            SlideSpec {
                damping: -0.5,
                ..ok
            },
            "damping",
        );
    }
}
