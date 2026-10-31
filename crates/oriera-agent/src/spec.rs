//! SceneSpec (M1 #5) — the experiment-as-file contract: a JSON scene
//! description that fully determines a [`TableTopEnv`] (arm geometry,
//! furniture, task, rates, solver knobs, randomization ranges).
//!
//! The file IS the experiment identity: two labs exchanging a spec
//! JSON exchange bit-identical episode streams (same seeds ⇒ same
//! fingerprints), so the spec's own fingerprint is pinned as a golden
//! contract in the tests. serde derives live behind subsys' `serde`
//! feature; no new dependencies.

use crate::embodied_env::{TableTopEnv, TaskKind};
use oriera_subsys::{ArmDescription, PhysicsParams, Scene3D};
use serde::{Deserialize, Serialize};

/// A complete, shareable experiment description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneSpec {
    pub desc: ArmDescription,
    pub scene: Scene3D,
    pub task: Task,
    pub dt: f32,
    pub max_steps: u64,
    #[serde(default = "default_true")]
    pub randomize: bool,
    #[serde(default)]
    pub kp_jitter: f32,
    #[serde(default)]
    pub mass_jitter: f32,
    #[serde(default)]
    pub obs_noise: f32,
    #[serde(default)]
    pub physics: PhysicsParams,
}

fn default_true() -> bool {
    true
}

/// Mirror of [`TaskKind`] for serde (snake_case names in JSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    Reach,
    PickPlace,
    Push,
}

impl From<Task> for TaskKind {
    fn from(t: Task) -> Self {
        match t {
            Task::Reach => Self::Reach,
            Task::PickPlace => Self::PickPlace,
            Task::Push => Self::Push,
        }
    }
}

impl SceneSpec {
    /// Build the environment this spec describes.
    pub fn to_env(&self) -> TableTopEnv {
        let mut env = TableTopEnv::with(
            self.desc.clone(),
            self.scene.clone(),
            self.dt,
            self.max_steps,
        );
        env.task = self.task.into();
        env.randomize = self.randomize;
        env.kp_jitter = self.kp_jitter;
        env.mass_jitter = self.mass_jitter;
        env.obs_noise = self.obs_noise;
        env.physics = self.physics;
        env
    }

    /// Parse a spec from JSON text (feature `json` provides
    /// serde_json; see also the checked-in golden spec file).
    #[cfg(feature = "json")]
    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| format!("spec JSON 非法：{e}"))
    }
}

/// The checked-in golden spec (`specs/pick_place.json`), as a literal —
/// the file and this constant must stay identical (contract test).
pub const GOLDEN_PICK_PLACE_SPEC: &str = r#"{
  "desc": {
    "base": [0.02, 0.5, 0.0],
    "shoulder_height": 0.12,
    "links": [0.20, 0.18, 0.09],
    "link_radius": 0.022,
    "home": [0.0, 0.0, 0.0, 0.0],
    "limits": [[-3.14159265, 3.14159265], [-0.35, 1.45], [-2.5, 0.35], [-2.0, 2.0]],
    "kp": 60.0,
    "kd": 4.0,
    "mass_scale": 1.0
  },
  "scene": {
    "ground": [[0.0, -0.05, 0.0], [2.0, 0.05, 1.5]],
    "table": [[0.32, 0.25, 0.0], [0.28, 0.25, 0.32]]
  },
  "task": "pick_place",
  "dt": 0.016666668,
  "max_steps": 700,
  "randomize": true,
  "kp_jitter": 0.0,
  "mass_jitter": 0.0,
  "obs_noise": 0.0,
  "physics": {}
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embodied_env::EmbodiedEnv;

    #[cfg(feature = "json")]
    #[test]
    fn golden_spec_parses_and_matches_the_default_env() {
        let spec = SceneSpec::from_json(GOLDEN_PICK_PLACE_SPEC).expect("golden spec must parse");
        assert_eq!(spec.task, Task::PickPlace);
        let mut env = spec.to_env();
        env.randomize = false; // 与默认 env 的逐位对照需同档位
        let mut default = TableTopEnv::new();
        default.randomize = false;
        let o1 = env.reset(42);
        let o2 = default.reset(42);
        assert_eq!(o1, o2, "golden spec must reproduce the default env");
    }

    #[cfg(feature = "json")]
    #[test]
    fn spec_drives_the_randomization_knobs() {
        let mut spec = SceneSpec::from_json(GOLDEN_PICK_PLACE_SPEC).unwrap();
        spec.kp_jitter = 0.2;
        spec.mass_jitter = 0.15;
        let env = spec.to_env();
        assert!((env.kp_jitter - 0.2).abs() < 1e-6);
        assert!((env.mass_jitter - 0.15).abs() < 1e-6);
    }

    #[cfg(feature = "json")]
    #[test]
    fn malformed_spec_is_an_error_not_a_panic() {
        assert!(SceneSpec::from_json("{ nope").is_err());
    }
}
