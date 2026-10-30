//! PyO3 gymnasium bridge (feature `python`) — the online training
//! loop's front door (PLAN-embodied-sim M1 #1). Builds the lib as the
//! `oriera_sim` extension module; SB3/lerobot wrap it directly:
//!
//! ```python
//! import oriera_sim
//! env = oriera_sim.PickPlaceEnv("pick_place", seed=1000)
//! obs, _done = env.reset(1000)  # 21 dims
//! obs, r, term, trunc, success = env.step([0.0] * 4 + [0.0])
//! ```
//!
//! Determinism survives the bridge: same seed ⇒ the same episode and
//! the same `fingerprint()` receipt as the Rust-side collection runs.
//! Test note: under the `python` feature the test binary must run with
//! `--test-threads=1` on Windows (cdylib + parallel test init is a
//! known pyo3 interaction) — the CI leg pins exactly that.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::embodied_env::{EmbodiedEnv, TableTopEnv, TaskKind};

/// A table-top manipulation environment (Gymnasium-style contract).
#[pyclass(unsendable)]
struct PickPlaceEnv {
    env: TableTopEnv,
}

#[pymethods]
impl PickPlaceEnv {
    #[new]
    #[pyo3(signature = (task = "pick_place", seed = Some(1000), kp_jitter = 0.0, mass_jitter = 0.0, obs_noise = 0.0))]
    fn new(
        task: &str,
        seed: Option<u64>,
        kp_jitter: f32,
        mass_jitter: f32,
        obs_noise: f32,
    ) -> PyResult<Self> {
        let mut env = TableTopEnv::new();
        env.task = TaskKind::parse(task).ok_or_else(|| {
            PyErr::new::<PyValueError, _>(format!(
                "unknown task {task:?} (reach | pick_place | push)"
            ))
        })?;
        env.kp_jitter = kp_jitter;
        env.mass_jitter = mass_jitter;
        env.obs_noise = obs_noise;
        env.reset(seed.unwrap_or(1000));
        Ok(Self { env })
    }

    /// Reset to a fresh episode; `seed` fully determines it.
    fn reset(&mut self, seed: u64) -> Vec<f32> {
        self.env.reset(seed)
    }

    /// Gymnasium step: returns (obs, reward, terminated, truncated,
    /// success).
    fn step(&mut self, action: Vec<f32>) -> (Vec<f32>, f64, bool, bool, bool) {
        let o = self.env.step(&action);
        (o.obs, o.reward as f64, o.terminated, o.truncated, o.success)
    }

    /// Per-episode state fingerprint (#67 semantics) — the
    /// reproducibility receipt.
    fn fingerprint(&self) -> u64 {
        self.env.fingerprint()
    }

    /// Current cube position [x, y, z] (task introspection).
    fn cube_pos(&self) -> [f32; 3] {
        self.env.cube_pos()
    }

    /// The goal position [x, y, z].
    fn goal(&self) -> [f32; 3] {
        self.env.goal()
    }

    /// Whether the gripper holds the cube.
    fn holding(&self) -> bool {
        self.env.holding()
    }

    #[getter]
    fn obs_names(&self) -> Vec<String> {
        EmbodiedEnv::obs_names(&self.env)
    }

    #[getter]
    fn action_names(&self) -> Vec<String> {
        EmbodiedEnv::action_names(&self.env)
    }

    #[getter]
    fn task_phrase(&self) -> &'static str {
        self.env.task.phrase()
    }

    #[getter]
    fn randomization(&self) -> String {
        self.env.randomization_receipt().to_string()
    }
}

#[pymodule]
fn oriera_sim(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PickPlaceEnv>()?;
    Ok(())
}
