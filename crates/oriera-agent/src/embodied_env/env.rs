//! The env body: [`TableTopEnv`] — the spec-driven table-top
//! manipulation environment — the Gymnasium contract
//! ([`EmbodiedEnv`]), the observation/action layout metadata
//! ([`ObsLayout`], review P-8), and the camera capture wiring on the
//! env (the legacy `--cam` top view plus the M3-A spec cameras).

use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, World};
#[cfg(feature = "cam")]
use oriera_subsys::rigid3d::PropShape;
use oriera_subsys::{
    ArmDescription, Gravity, JointTargets, JointTelemetry, MultibodySystem, PhysicsParams, Pose3,
    PropPhys, RobotSpec, Scene3D, SimObject, TimeScale,
};

use super::cameras::CameraFrame;
#[cfg(feature = "cam")]
use super::cameras::{link_radius, quat_rotate, stamp_prop, CameraShot};
#[cfg(feature = "cam")]
use super::scene::CamAttach;
use super::scene::{CameraSpec, Lcg, SceneGraph};
use super::tasks::{
    make_task, task_entry, PickPlaceTask, StepState, Task, TaskEntry, TaskKind, TaskParams,
};
#[cfg(feature = "cam")]
use oriera_subsys::camera::{
    capture_png, encode_depth_png, encode_png, render_pinhole, render_top_down, CameraView,
    Distortion, PinholeCam, Prim, SceneView,
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
            let Some(pose) = self.prop_pose(i) else {
                continue;
            };
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
        let shape = self
            .scene
            .props
            .first()
            .map(|p| p.shape)
            .unwrap_or_default();
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
            (
                self.scene.props[0].phys.density,
                self.scene.props[0].phys.friction,
            )
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
        self.prop_ents
            .get(i)
            .and_then(|e| self.world.get::<Pose3>(*e))
            .copied()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embodied_env::OBS_NAMES;

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
            vec![("arm".to_string(), 0, 4), ("gripper".to_string(), 4, 5),]
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
    fn cylinder_prop_runs_and_fingerprints_differently() {
        // G-3 core: one field in the spec swaps the cube for a cylinder;
        // the env must run end-to-end (same 21-dim contract).
        let mut g = SceneGraph::default();
        use oriera_subsys::rigid3d::PropShape;
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
        use oriera_subsys::rigid3d::PropShape;
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
        stamp_prop(
            &view,
            &mut rgb,
            pos,
            PropShape::Cylinder {
                half_h: 0.025,
                radius: 0.025,
            },
        );
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
