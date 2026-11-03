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
use oriera_subsys::camera::{
    capture_png, encode_depth_png, encode_png, render_pinhole, render_top_down, CameraView,
    Distortion, PinholeCam, Prim, SceneView,
};

/// One captured camera frame (per tick, per camera): PNG bytes of the
/// channels the camera exports. The legacy top-down `--cam` view emits
/// `cam = "top"` with RGB only; spec cameras emit RGB and/or a
/// millimetre u16 depth PNG. Drained by [`TableTopEnv::take_frames`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraFrame {
    /// LeRobot feature suffix (`observation.images.{cam}` / `depth.{cam}`).
    pub cam: String,
    /// RGB8 PNG bytes, when the camera exports RGB.
    pub rgb_png: Option<Vec<u8>>,
    /// 16-bit grayscale PNG (millimetres, 0 = no return), when the
    /// camera exports depth.
    pub depth_png: Option<Vec<u8>>,
}

/// One raw (unencoded) spec-camera render — the calibration probe's
/// input (feature `cam`): pixel buffers plus the camera that produced
/// them, so a caller can `project()` world points onto the same image.
#[cfg(feature = "cam")]
#[derive(Debug, Clone, PartialEq)]
pub struct CameraShot {
    pub cam: String,
    pub w: u32,
    pub h: u32,
    /// Row-major RGB8.
    pub rgb: Option<Vec<u8>>,
    /// Row-major millimetre depth.
    pub depth: Option<Vec<u16>>,
    /// The calibrated camera (resolved extrinsics) this frame came from.
    pub pinhole: PinholeCam,
}

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
        Self { k1: 0.0, k2: 0.0, p1: 0.0, p2: 0.0 }
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
        Self { rgb: true, depth: false }
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
        let Some(obj) = params.as_object_mut() else { return };
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
            return Err("task.params: goal_xz_tol / reach_radius / min_separation 须为有限正数".into());
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
        if !(self.release_below.is_finite() && self.release_below > 0.0 && self.release_below <= 1.0)
        {
            return Err(format!(
                "task.params.release_below = {}：须在 (0, 1]（夹爪指令低于它视为松爪，默认 0.5）",
                self.release_below
            ));
        }
        if !ok(self.stack_xz_tol) || !ok(self.stack_y_tol) {
            return Err("task.params: stack_xz_tol / stack_y_tol 须为有限正数".into());
        }
        let horiz = (self.pull_axis[0] * self.pull_axis[0] + self.pull_axis[2] * self.pull_axis[2]).sqrt();
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
        let d = ((obj[0] - placements.goal[0]).powi(2) + (obj[2] - placements.goal[2]).powi(2)).sqrt();
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
        let props: Vec<[f32; 3]> = scene.props.iter().map(|p| p.placement.sample(rng)).collect();
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
        let props: Vec<[f32; 3]> = scene.props.iter().map(|p| p.placement.sample(rng)).collect();
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
        let props: Vec<[f32; 3]> = scene.props.iter().map(|p| p.placement.sample(rng)).collect();
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
        let props: Vec<[f32; 3]> = scene.props.iter().map(|p| p.placement.sample(rng)).collect();
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
        let b = placements.props.get(1).copied().unwrap_or([0.30, 0.55, 0.0]);
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
            reward += -1.2 * s.d_obj_goal
                + 2.0 * (s.obj_pos[1] - self.params.goal_y_min).clamp(0.0, 0.2);
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
        let props: Vec<[f32; 3]> = scene.props.iter().map(|p| p.placement.sample(rng)).collect();
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
        let props: Vec<[f32; 3]> = scene.props.iter().map(|p| p.placement.sample(rng)).collect();
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
        let pulled = (s.obj_pos[0] - s.obj_start[0]) * u[0]
            + (s.obj_pos[2] - s.obj_start[2]) * u[2];
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
    /// Gym env name (the [`EnvSpec::name`] source).
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
        find(&|e: &TaskEntry| {
            e.id.split('@').next() == Some(id) && e.id.ends_with("@v1")
        })
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
    /// Registry override (M2): when set (spec-driven path), the reset
    /// builds THIS id instead of the [`TaskKind`] alias's `@v1` — how
    /// `@v2` revisions and the new families (`stack@v1`, …) run without
    /// touching the frozen three-variant enum.
    pub task_entry: Option<&'static TaskEntry>,
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
    /// Second prop entity (`scene.props[1]` — the stack base).
    obj1: Option<Entity>,
    /// All prop entities in `scene.props` order（M2 渲染全量 poses 用）。
    prop_ents: Vec<Entity>,
    rng: Lcg,
    steps: u64,
    was_holding: bool,
    /// `holding` seen on any step of this episode (push@v2).
    ever_holding: bool,
    /// Task-object centre last tick — the still-counter's speed source.
    prev_obj_pos: [f32; 3],
    /// Consecutive steps below `rest_speed` (@v2 rest criterion).
    obj_still_steps: u32,
    /// Where the task object spawned this episode (drawer origin).
    obj_start: [f32; 3],
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
    /// Spec cameras (M3-A): rendered through the pinhole pipeline at
    /// the control rate when the `cam` feature is on. Empty (the
    /// default) keeps the legacy `--cam` path byte-for-byte.
    pub spec_cameras: Vec<CameraSpec>,
    /// (tick, camera frame) of the open episode; drained by
    /// [`TableTopEnv::take_frames`] on export.
    frames: Vec<(u64, CameraFrame)>,
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
            task_entry: None,
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
            obj1: None,
            prop_ents: Vec::new(),
            rng: Lcg::new(1),
            steps: 0,
            was_holding: false,
            ever_holding: false,
            prev_obj_pos: [0.0; 3],
            obj_still_steps: 0,
            obj_start: [0.0; 3],
            tick: 0,
            rand_note: String::new(),
            robot_working: RobotSpec::from(&ArmDescription::default()),
            task_impl: Box::new(PickPlaceTask::new(TaskParams::default())),
            layout,
            actions,
            #[cfg(feature = "cam")]
            cam: None,
            spec_cameras: Vec::new(),
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

    /// The id the reset actually builds: the registry override when set
    /// (spec path), else the [`TaskKind`] alias's `@v1` (frozen path).
    fn active_task_id(&self) -> &'static str {
        match self.task_entry {
            Some(e) => e.id,
            None => self.task.id(),
        }
    }

    /// Override the task by registry id (M2 spec path): sets the entry
    /// (and the closest [`TaskKind`] alias for the frozen callers) and
    /// re-derives the obs/action layout metadata from a probe of the
    /// task. Errors name the legal ids (G-7 style).
    pub fn set_task_id(&mut self, id: &str) -> Result<(), String> {
        let entry = task_entry(id)?;
        let probe = (entry.make)(self.task_params);
        self.task_entry = Some(entry);
        self.task = entry.kind;
        self.layout = ObsLayout::derive(&self.robot, probe.as_ref());
        Ok(())
    }

    /// The active task's registry id (`pick_place@v1`).
    pub fn task_id(&self) -> &'static str {
        self.active_task_id()
    }

    /// The active task's natural-language instruction.
    pub fn task_phrase(&self) -> &'static str {
        match self.task_entry {
            Some(e) => e.phrase,
            None => self.task.phrase(),
        }
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

    /// Mount the spec's calibrated cameras (M3-A). They render through
    /// the pinhole pipeline at the control rate — alongside, never
    /// instead of, the legacy `--cam` top view (that one is mounted
    /// separately via [`Self::set_camera`]).
    pub fn set_cameras(&mut self, cams: Vec<CameraSpec>) {
        self.spec_cameras = cams;
    }

    /// Drain the open episode's captured frames: `(tick, CameraFrame)`.
    /// The legacy top view arrives as `cam = "top"` (RGB only); spec
    /// cameras carry RGB and/or depth PNG bytes by name.
    pub fn take_frames(&mut self) -> Vec<(u64, CameraFrame)> {
        std::mem::take(&mut self.frames)
    }

    /// The resolved world-frame pose of one spec camera
    /// (`(pos, look_at, up)`): verbatim for `attach: "world"`, or the
    /// EE-frame offsets rotated by the telemetry quaternion for
    /// `attach: "ee"` — the per-tick extrinsics of a wrist camera.
    #[cfg(feature = "cam")]
    pub fn cam_axes(&self, cam: &CameraSpec) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let p = cam.pinhole();
        match p.attach {
            CamAttach::World => (p.pos, p.look_at, p.up),
            CamAttach::Ee => {
                let tel = self
                    .world
                    .get::<JointTelemetry>(self.arm)
                    .cloned()
                    .unwrap_or_default();
                let rot = |v: [f32; 3]| quat_rotate(tel.ee_rot, v);
                let rel = |v: [f32; 3]| {
                    let r = rot(v);
                    [
                        tel.ee_pos[0] + r[0],
                        tel.ee_pos[1] + r[1],
                        tel.ee_pos[2] + r[2],
                    ]
                };
                (rel(p.pos), rel(p.look_at), rot(p.up))
            }
        }
    }

    /// The calibrated [`PinholeCam`] for one spec camera at the current
    /// tick (extrinsics resolved; validation already made failures
    /// impossible, the panic names the spec field otherwise).
    #[cfg(feature = "cam")]
    pub fn pinhole_for(&self, cam: &CameraSpec) -> PinholeCam {
        let p = cam.pinhole();
        let (pos, look, up) = self.cam_axes(cam);
        PinholeCam::from_look(
            pos,
            look,
            up,
            p.width,
            p.height,
            p.fx,
            p.fy,
            p.cx,
            p.cy,
            Distortion {
                k1: p.dist.k1,
                k2: p.dist.k2,
                p1: p.dist.p1,
                p2: p.dist.p2,
            },
            p.channels.rgb,
            p.channels.depth,
        )
        .unwrap_or_else(|e| panic!("scene.cameras（{}）：{e}（应已被 validate 拦下）", cam.name))
    }

    /// The world as the pinhole renderer sees it (M3-A): fixtures as
    /// boxes, props as oriented primitives, the arm skeleton as capsule
    /// segments between joint origins, the gripper plates as two small
    /// balls, the goal as a ground disc. Flat colours double as the
    /// calibration targets (prop / arm / goal each have one exact RGB).
    #[cfg(feature = "cam")]
    pub fn scene_view(&self) -> SceneView {
        let mut prims = Vec::with_capacity(16 + self.prop_ents.len());
        let robot = &self.robot_working;
        for (i, f) in self.scene.fixtures.iter().enumerate() {
            let color = if i == 0 { [38, 42, 52] } else { [56, 68, 88] };
            prims.push(Prim::Cuboid {
                center: f.center,
                half: f.half,
                color,
            });
        }
        // the fixed base link (its box never moves)
        if let Some(l) = robot.links.first() {
            let half = match l.shape {
                oriera_subsys::LinkShape::Box { half } => half,
                _ => [0.03; 3],
            };
            prims.push(Prim::Cuboid {
                center: [
                    l.origin[0] + l.collider_offset[0],
                    l.origin[1] + l.collider_offset[1],
                    l.origin[2] + l.collider_offset[2],
                ],
                half,
                color: [70, 76, 96],
            });
        }
        // arm skeleton: capsules between consecutive chain points,
        // radius read off the link geometry the segment belongs to
        let pts = self.sys.chain_points();
        for (k, w) in pts.windows(2).enumerate() {
            let radius = robot
                .joints
                .get(k + 1)
                .and_then(|j| robot.links.get(j.child_link))
                .map(|l| link_radius(&l.shape))
                .unwrap_or(0.012);
            prims.push(Prim::Capsule {
                a: w[0],
                b: w[1],
                radius,
                color: [91, 141, 239],
            });
        }
        // gripper plates as two small balls (plate centres from the sys)
        if let Some([a, b]) = self.sys.plate_positions() {
            for p in [a, b] {
                prims.push(Prim::Sphere {
                    center: p,
                    r: 0.006,
                    color: [210, 214, 224],
                });
            }
        }
        // props (pos + quaternion from the world; shapes from the spec)
        for (i, prop) in self.scene.props.iter().enumerate() {
            let Some(pose) = self.prop_pose(i) else { continue };
            match prop.shape {
                PropShape::Cuboid { half } => prims.push(Prim::BoxQ {
                    center: pose.pos,
                    rot: pose.rot,
                    half,
                    color: [255, 206, 110],
                }),
                PropShape::Cylinder { half_h, radius } => prims.push(Prim::Cylinder {
                    center: pose.pos,
                    rot: pose.rot,
                    half_h,
                    radius,
                    color: [255, 206, 110],
                }),
                PropShape::Sphere { r } => prims.push(Prim::Sphere {
                    center: pose.pos,
                    r,
                    color: [255, 206, 110],
                }),
            }
        }
        // the goal marker (a hair above the surface so the z-buffer
        // keeps it over the table slab)
        prims.push(Prim::Disc {
            center: [self.goal[0], self.goal[1] + 0.002, self.goal[2]],
            radius: 0.045,
            color: [64, 200, 132],
        });
        // the calibration beacon (U-5): an unlit LED-like marker on the
        // gripper tip — the reprojection gate's exact-colour target
        // (everything else in the frame is flat-shaded, angle-dependent)
        if let Some(tip) = pts.last() {
            prims.push(Prim::Beacon {
                center: *tip,
                r: 0.008,
                color: [91, 141, 239],
            });
        }
        SceneView { prims }
    }

    /// Render every spec camera at the current tick (raw buffers — the
    /// PNG encoding happens in the capture path; the calibration probe
    /// consumes the raw pixels directly).
    #[cfg(feature = "cam")]
    pub fn render_cameras(&self) -> Vec<CameraShot> {
        let view = self.scene_view();
        self.spec_cameras
            .iter()
            .map(|c| {
                let pin = self.pinhole_for(c);
                let frame = render_pinhole(&pin, &view);
                CameraShot {
                    cam: c.name.clone(),
                    w: pin.width,
                    h: pin.height,
                    rgb: frame.rgb,
                    depth: frame.depth,
                    pinhole: pin,
                }
            })
            .collect()
    }

    /// Encode one spec-camera shot into the export frame (`cam`, RGB
    /// PNG, depth PNG) — the per-tick capture path.
    #[cfg(feature = "cam")]
    fn spec_camera_frame(&self, shot: &CameraShot) -> Result<CameraFrame, String> {
        let rgb_png = match &shot.rgb {
            Some(buf) => Some(encode_png(buf, shot.w, shot.h)?),
            None => None,
        };
        let depth_png = match &shot.depth {
            Some(buf) => Some(encode_depth_png(buf, shot.w, shot.h)?),
            None => None,
        };
        Ok(CameraFrame {
            cam: shot.cam.clone(),
            rgb_png,
            depth_png,
        })
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
            ..StepState::default()
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

/// Quaternion `[x, y, z, w]` rotate a vector — the EE-frame mount math
/// (mirrors `oriera_subsys::camera`'s private helper; kept here so the
/// env never reaches into the renderer's internals).
#[cfg(feature = "cam")]
fn quat_rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let qv = [q[0], q[1], q[2]];
    let t = [
        2.0 * (qv[1] * v[2] - qv[2] * v[1]),
        2.0 * (qv[2] * v[0] - qv[0] * v[2]),
        2.0 * (qv[0] * v[1] - qv[1] * v[0]),
    ];
    [
        v[0] + q[3] * t[0] + (qv[1] * t[2] - qv[2] * t[1]),
        v[1] + q[3] * t[1] + (qv[2] * t[0] - qv[0] * t[2]),
        v[2] + q[3] * t[2] + (qv[0] * t[1] - qv[1] * t[0]),
    ]
}

/// Skeleton capsule radius of one link: the shape's thin dimension
/// (capsule radius / box min-half), floored so hairline links stay
/// visible in the perspective render.
#[cfg(feature = "cam")]
fn link_radius(shape: &oriera_subsys::LinkShape) -> f32 {
    let r = match shape {
        oriera_subsys::LinkShape::Capsule { radius, .. } => *radius,
        oriera_subsys::LinkShape::Box { half } => half.iter().cloned().fold(f32::MAX, f32::min),
        _ => 0.012,
    };
    r.max(0.008)
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
            name: match self.task_entry {
                Some(e) => e.env_name,
                None => self.task.env_name(),
            },
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
        self.ever_holding = false;
        self.obj_still_steps = 0;
        self.frames.clear();
        self.task_impl = make_task(self.active_task_id(), self.task_params)
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
        self.prop_ents.clear();
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
                self.obj_start = placements.props[i];
            } else if i == 1 {
                self.obj1 = Some(e);
            }
            self.prop_ents.push(e);
        }
        self.world.insert_resource(self.physics);
        self.sys = MultibodySystem::from_robot_fixtures(
            self.robot_working.clone(),
            &self.scene.to_fixture_slabs(),
        )
        .unwrap_or_else(|e| panic!("reset：robot 构建失败：{e}"));

        // Settle: let the cube land and the chain stabilize, so every
        // episode starts from the same resting posture.
        for _ in 0..SETTLE_TICKS {
            self.sys.run(&self.ctx(), &mut self.world);
            self.tick += 1;
        }
        self.prev_obj_pos = self.cube_pos();
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
        // the previous placement is gone). Spec cameras (M3-A) capture
        // the same state through the pinhole pipeline, after the
        // legacy view so its bytes never move.
        #[cfg(feature = "cam")]
        {
            if let Some(view) = &self.cam {
                let obj_now = self
                    .world
                    .get::<Pose3>(self.obj)
                    .copied()
                    .unwrap_or_default();
                if let Ok(png) = self.frame_png(view, obj_now.pos) {
                    self.frames.push((
                        self.tick,
                        CameraFrame {
                            cam: "top".into(),
                            rgb_png: Some(png),
                            depth_png: None,
                        },
                    ));
                }
            }
            if !self.spec_cameras.is_empty() {
                for shot in self.render_cameras() {
                    if let Ok(frame) = self.spec_camera_frame(&shot) {
                        self.frames.push((self.tick, frame));
                    }
                }
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

        // @v2 still-counter: consecutive ticks the task object stayed
        // quasi-static (centre speed < rest_speed). One moved tick
        // resets it — "dropped it and it kept rolling" never counts.
        let speed = ((obj.pos[0] - self.prev_obj_pos[0]).powi(2)
            + (obj.pos[1] - self.prev_obj_pos[1]).powi(2)
            + (obj.pos[2] - self.prev_obj_pos[2]).powi(2))
        .sqrt()
            / self.dt.max(1e-6);
        if speed < self.task_params.rest_speed {
            self.obj_still_steps += 1;
        } else {
            self.obj_still_steps = 0;
        }
        self.prev_obj_pos = obj.pos;
        if tel.holding {
            self.ever_holding = true;
        }
        let prop1 = self
            .obj1
            .and_then(|e| self.world.get::<Pose3>(e))
            .map(|p| p.pos)
            .unwrap_or([0.0; 3]);
        let gripper_cmd = self
            .world
            .get::<JointTargets>(self.arm)
            .map(|t| t.gripper)
            .unwrap_or(0.0);

        let s = StepState {
            ee,
            obj_pos: obj.pos,
            goal: self.goal,
            holding: tel.holding,
            was_holding: self.was_holding,
            d_ee_obj,
            d_obj_goal,
            gripper: gripper_cmd,
            still_steps: self.obj_still_steps,
            ever_holding: self.ever_holding,
            prop1_pos: prop1,
            obj_start: self.obj_start,
        };
        // Dense shaping (IsaacLab/ManiSkill tradition), per task; the
        // success bonus rides inside the reward (task trait).
        let reward = self.task_impl.reward(&s);
        let success = self.task_impl.success(&s);
        self.was_holding = tel.holding;
        // `terminate_on_success` (TaskParams, @v2 protocol slot): true =
        // the legacy terminate-on-first-success; false = run to the
        // time limit, the caller reports success_once / success_at_end.
        let terminated = success && self.task_params.terminate_on_success;
        let truncated = !terminated && self.steps >= self.max_steps;

        StepOutcome {
            obs: self.observe(),
            reward,
            terminated,
            truncated,
            success,
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

    /// Position of `scene.props[1]` (the stack base) — introspection for
    /// baselines and tests; zero without a second prop.
    pub fn prop1_pos(&self) -> [f32; 3] {
        self.obj1
            .and_then(|e| self.world.get::<Pose3>(e))
            .map(|p| p.pos)
            .unwrap_or([0.0; 3])
    }

    /// 姿态快照（M2 渲染全量 props 用；rot 自 WPA 起公开）。
    pub fn prop_pose(&self, i: usize) -> Option<Pose3> {
        self.prop_ents.get(i).and_then(|e| self.world.get::<Pose3>(*e)).copied()
    }

    /// 场景 prop 总数（含任务对象）。
    pub fn prop_count(&self) -> usize {
        self.prop_ents.len()
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
        assert!(err.contains("合法值") && err.contains("pick_place@v1"), "{err}");
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
            ..StepState::default()
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
        let gripped = StepState {
            gripper: 1.0,
            ..s
        };
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
        let gripped = StepState {
            gripper: 1.0,
            ..s
        };
        assert!(!t.success(&gripped));
        assert_eq!(t.success_criteria(), "v2");
    }

    /// Env-level: the v2 predicate + `terminate_on_success` slot end to
    /// end — teleport the cube into the zone with the gripper open, the
    /// episode terminates only after the rest window has elapsed.
    #[test]
    fn pick_place_v2_env_terminates_after_the_rest_window() {
        let mut env = TableTopEnv::new();
        env.task_params.terminate_on_success = true;
        env.set_task_id("pick_place@v2").expect("v2 builds");
        env.randomize = false;
        env.reset(11);
        if let Some(p) = env.world.get_mut::<Pose3>(env.obj) {
            p.pos = env.goal;
            p.pos[1] = 0.525; // resting height at the goal XZ
        }
        // Open-gripper zero-torque policy; the cube stays put.
        let mut fired = None;
        for i in 0..30u64 {
            let out = env.step(&[0.0; 5]);
            if out.terminated {
                fired = Some((i, out.success));
                break;
            }
        }
        let (i, success) = fired.expect("v2 success must fire after ~rest_steps");
        assert!(success);
        assert!(
            i >= u64::from(env.task_params.rest_steps.saturating_sub(2)),
            "step {i}"
        );
        // The step it fired on IS a success frame (terminated ⇔ success).
    }

    /// `terminate_on_success = false`: the same teleported episode runs
    /// to the time limit instead of terminating (the caller then reports
    /// success_once / success_at_end).
    #[test]
    fn terminate_on_success_false_runs_to_the_time_limit() {
        let mut env = TableTopEnv::new();
        env.task_params.terminate_on_success = false;
        env.task_params.rest_steps = 2;
        env.set_task_id("pick_place@v2").expect("v2 builds");
        env.randomize = false;
        env.max_steps = 80;
        env.reset(11);
        if let Some(p) = env.world.get_mut::<Pose3>(env.obj) {
            p.pos = env.goal;
            p.pos[1] = 0.525;
        }
        let mut steps = 0;
        loop {
            let out = env.step(&[0.0; 5]);
            steps += 1;
            if out.truncated {
                assert!(out.success, "still in zone at the time limit");
                break;
            }
            assert!(!out.terminated, "must not terminate with the slot off");
            assert!(steps < 2000, "no truncation — broken");
        }
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
        let png = f1[0].1.rgb_png.as_ref().expect("top view is RGB");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(f1[0].1.cam, "top", "the legacy track keeps its name");
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
