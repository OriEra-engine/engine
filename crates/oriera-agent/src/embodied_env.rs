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
//!
//! # Module layout (P1-C1 split, PLAN-embodied-v3 #19)
//!
//! The former single 3,950-line file now lives in four focused
//! submodules — `scene` (SceneGraph / fixtures / props / camera
//! specs + the `Lcg` episode RNG), `tasks` (the `Task` trait, the
//! nine registry implementations, `TaskParams`, `TaskKind`), `env`
//! (`TableTopEnv`, `ObsLayout`, the Gymnasium contract, the capture
//! wiring) and `cameras` (frame types + render helpers) — with this
//! file as the umbrella. Every public item is re-exported here, so
//! the `oriera_agent::embodied_env::…` paths (lib.rs, python.rs,
//! lerobot.rs, spec.rs, manifest.rs, tests, examples, the wasm
//! shell) stay byte-for-byte unchanged.

/// Legacy observation/action column names (pre-M0a LeRobot feature
/// meta) — kept public as the positional contract the derived
/// [`ObsLayout`] is tested against.
pub use self::embodied_env_defaults::{ACTION_NAMES, OBS_NAMES};

mod cameras;
mod env;
mod scene;
mod tasks;

pub use cameras::CameraFrame;
#[cfg(feature = "cam")]
pub use cameras::CameraShot;
pub use env::{
    action_layout, EmbodiedEnv, EnvSpec, ObsGroup, ObsLayout, ObsSlice, StepOutcome, TableTopEnv,
    SETTLE_TICKS,
};
pub use scene::{
    CamAttach, CamChannels, CameraKind, CameraSpec, DistortionSpec, Fixture, Lcg, PinholeSpec,
    Placement, PropSpec, SceneGraph, OBJ_RANGE,
};
pub use tasks::{
    make_task, task_entry, task_id_from_phrase, task_registry, Placements, StepState, Task,
    TaskEntry, TaskKind, TaskParams, GOAL_XZ_TOL, GOAL_Y_MAX, GOAL_Y_MIN, PULL_AXIS_DEFAULT,
    PULL_DISTANCE_DEFAULT, REACH_RADIUS, RELEASE_BELOW_DEFAULT, REST_SPEED_DEFAULT,
    REST_STEPS_DEFAULT, STACK_XZ_TOL_DEFAULT, STACK_Y_TOL_DEFAULT, TASK_PARAMS_M2_KEYS,
    TERMINATE_ON_SUCCESS_DEFAULT,
};

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
