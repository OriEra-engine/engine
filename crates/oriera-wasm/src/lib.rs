//! `oriera-wasm` — the browser/node front door of the OriEra embodied
//! simulator (PLAN-embodied-general v2.2 §3.2 / M1).
//!
//! ```text
//! oriera-subsys(rigid3d) ─▶ oriera-agent(TableTopEnv, SceneSpec)
//!        │                              │
//!   Rust CLI / PyO3                 this crate (wasm-bindgen)
//!                                       │
//!                           examples/15-embodied-sim/engine/kernel.js
//!                           (three.js renders what `poses()` says)
//! ```
//!
//! One physics, four entrances: the same `TableTopEnv` that the CLI
//! collector and the Python wheel drive is compiled to wasm32 here, so
//! the browser demo stops being a JS *mirror* of the engine and becomes
//! a *renderer* of it. Same spec + same seed ⇒ the same `fingerprint()`
//! receipt as the native build — the §五 experiment `tools/
//! wasm_golden_check.mjs` measures exactly that.
//!
//! JS surface (see `vendor/oriera/oriera_wasm.js` after bindgen):
//!
//! ```js
//! const sim = OrieraSim.from_spec_json(specText);   // throws readable Error
//! let obs = sim.reset(1000);                        // Float32Array
//! const r = sim.step([0, 0, 0, 0, 0]);              // StepResult
//! r.obs, r.reward, r.terminated, r.truncated, r.success; r.free();
//! JSON.parse(sim.poses());                          // every pose the renderer places
//! sim.fingerprint();                                // decimal u64 string
//! ```

mod core;

pub use crate::core::{preset_spec_json, Pose, SimCore};

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(s: &str);
}

/// Route Rust panics to `console.error` with the message (otherwise the
/// browser only sees `RuntimeError: unreachable`). No extra crate — a
/// plain wasm-bindgen import.
#[wasm_bindgen(start)]
pub fn __oriera_wasm_init() {
    std::panic::set_hook(Box::new(|info| {
        console_error(&format!("oriera-wasm panic: {info}"));
    }));
}

/// One `step()` outcome — the Gymnasium tuple plus `success`.
#[wasm_bindgen]
pub struct StepResult {
    obs: Vec<f32>,
    reward: f32,
    terminated: bool,
    truncated: bool,
    success: bool,
}

#[wasm_bindgen]
impl StepResult {
    #[wasm_bindgen(getter)]
    pub fn obs(&self) -> Vec<f32> {
        self.obs.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn reward(&self) -> f32 {
        self.reward
    }
    #[wasm_bindgen(getter)]
    pub fn terminated(&self) -> bool {
        self.terminated
    }
    #[wasm_bindgen(getter)]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
    #[wasm_bindgen(getter)]
    pub fn success(&self) -> bool {
        self.success
    }
}

/// The simulator handle. Build with [`OrieraSim::from_spec_json`].
#[wasm_bindgen]
pub struct OrieraSim {
    core: SimCore,
}

#[wasm_bindgen]
impl OrieraSim {
    /// The engine's default experiment (`tabletop-4dof-pick`, pick_place)
    /// as spec JSON — the demo's preset; `from_spec_json(preset_spec_json())`
    /// is the same env as `TableTopEnv::new()`.
    pub fn preset_spec_json() -> String {
        crate::core::preset_spec_json()
    }

    /// Build from `oriera.scene/2` JSON text (a v1 draft migrates on the
    /// fly). Validation failures surface as a JS `Error` carrying the
    /// engine's field-level message.
    #[wasm_bindgen]
    pub fn from_spec_json(json: String) -> Result<OrieraSim, JsValue> {
        SimCore::from_spec_json(&json)
            .map(|core| OrieraSim { core })
            .map_err(|e| JsValue::from(js_sys_error(&e)))
    }

    /// Reset to a fresh episode. `seed` is a JS number (exact to 2^53;
    /// the collector's seeds are small integers).
    pub fn reset(&mut self, seed: f64) -> Vec<f32> {
        self.core.reset(seed.max(0.0) as u64)
    }

    /// Advance one control step with `[tgt_j0..tgt_jN-1, gripper]`.
    pub fn step(&mut self, action: Vec<f32>) -> StepResult {
        let o = self.core.step(&action);
        StepResult {
            obs: o.obs,
            reward: o.reward,
            terminated: o.terminated,
            truncated: o.truncated,
            success: o.success,
        }
    }

    /// The last observation (after `reset` or `step`).
    pub fn obs(&self) -> Vec<f32> {
        self.core.obs().to_vec()
    }

    /// Every pose the renderer places, as JSON (links, TCP, plates,
    /// prop, goal, flags) — see `SimCore::poses_json`.
    pub fn poses(&self) -> String {
        self.core.poses_json()
    }

    /// Per-episode state fingerprint (#67), as a decimal `u64` string
    /// (JS numbers cannot hold it exactly).
    pub fn fingerprint(&self) -> String {
        self.core.fingerprint().to_string()
    }

    /// Same receipt, 16 hex digits.
    pub fn fingerprint_hex(&self) -> String {
        format!("{:016x}", self.core.fingerprint())
    }

    /// Canonical spec hash (16 hex digits) — the experiment's identity.
    pub fn spec_hash(&self) -> String {
        self.core.spec_hash().to_string()
    }

    /// Whether the input was a v1 draft migrated on load.
    pub fn migrated(&self) -> bool {
        self.core.migrated()
    }

    pub fn task_phrase(&self) -> String {
        self.core.task_phrase().to_string()
    }

    pub fn task_id(&self) -> String {
        self.core.task_id().to_string()
    }

    /// Observation/action layout JSON (names, slices, dims, dof, dt,
    /// max_steps).
    pub fn layout(&self) -> String {
        self.core.layout_json()
    }

    /// The resolved spec as JSON — the renderer builds its scene from it.
    pub fn spec_json(&self) -> String {
        self.core.spec_json()
    }

    pub fn dof(&self) -> u32 {
        self.core.dof() as u32
    }

    pub fn dt(&self) -> f32 {
        self.core.dt()
    }

    pub fn max_steps(&self) -> u32 {
        self.core.max_steps() as u32
    }

    /// Control steps since the last reset.
    pub fn steps(&self) -> u32 {
        self.core.steps() as u32
    }

    /// Engine ticks since the last reset (includes the settle ticks).
    pub fn tick(&self) -> u32 {
        self.core.tick() as u32
    }

    pub fn goal(&self) -> Vec<f32> {
        self.core.goal().to_vec()
    }

    pub fn cube_pos(&self) -> Vec<f32> {
        self.core.cube_pos().to_vec()
    }

    pub fn ee_pos(&self) -> Vec<f32> {
        self.core.ee_pos().to_vec()
    }

    pub fn holding(&self) -> bool {
        self.core.holding()
    }

    pub fn gripper(&self) -> f32 {
        self.core.gripper()
    }

    /// Current gripper plate gap (m) — the ramped value, bit-exact with
    /// the engine's `MultibodySystem::gap_now` (M1 leftover #2 closed;
    /// read-only).
    pub fn gap(&self) -> f32 {
        self.core.gap()
    }

    /// Measured joint angles (rad).
    pub fn angles(&self) -> Vec<f32> {
        self.core.angles()
    }

    pub fn randomization(&self) -> String {
        self.core.randomization_receipt().to_string()
    }

    /// The collector's pick_place scripted baseline (f32-exact mirror
    /// of `examples/embodied_sim_collect.rs`) for control step `step`,
    /// read off the last observation. Empty array when not applicable
    /// (non-4-DoF arm / other task) — the caller keeps its own policy.
    pub fn baseline_action(&self, step: u32) -> Vec<f32> {
        self.core.baseline_action(step as u64).unwrap_or_default()
    }
}

/// A JS `Error` with the engine's message. Built through wasm-bindgen's
/// own `JsError` (no js-sys dep).
fn js_sys_error(msg: &str) -> JsError {
    JsError::new(msg)
}
