//! L9 embodied simulation runtime — the spec-driven table-top
//! manipulation stack, split out of `oriera-agent` (ADR 0004,
//! PLAN-embodied-v3 P2).
//!
//! One crate per world generation: this is the **embodied face**
//! (仿真运行时), `oriera-agent` keeps the **cognition face** (认知层)
//! and re-exports everything here for path compatibility —
//! `oriera_agent::embodied_env::…` keeps working after the split.
//!
//! Contents (all moved verbatim from `oriera-agent`, zero logic
//! change):
//!
//! * [`embodied_env`] — the Gymnasium-signature environment
//!   ([`embodied_env::TableTopEnv`]), the spec-driven task family and
//!   its registry, scene graph, domain randomization and the camera
//!   capture wiring (P1-C1 four-submodule layout).
//! * [`spec`] — `oriera.scene/2` experiment-as-a-file: serde scene
//!   descriptions, `extends` overlays, `spec_hash`.
//! * [`urdf`] — robot description import (URDF → [`oriera_subsys::ArmDescription`]).
//! * [`meshres`] — URDF mesh resolution + GLB JSON chunk probing.
//! * [`lerobot`] — LeRobot v2.1/v3 dataset export (the world-as-benchmark).
//! * [`eval`] — versioned evaluation protocols (`course@v1` / `paper@v1`).
//! * [`ik`] — DLS numerical IK for arbitrary N-DoF `RobotSpec`s.
//! * [`manifest`] — run provenance manifests + compile-time `build_info()`.
//!
//! Consumers: the wasm shell ([`oriera-wasm`]) depends on **this**
//! crate (not on the cognition layer), the PyO3 gymnasium bridge
//! (`oriera-agent` feature `python`) and the collection/baseline
//! harness examples reach the same runtime through the compat
//! re-exports.
//!
//! [`oriera-wasm`]: https://docs.rs/oriera-wasm

#[cfg(feature = "rigid3d")]
pub mod embodied_env;
/// Versioned evaluation protocols (M0b P-1): `course@v1` / `paper@v1`
/// behind one `evaluate()` entry. Needs the experiment layer
/// (`rigid3d` + `json`, i.e. the `spec` feature).
#[cfg(feature = "spec")]
pub mod eval;
/// DLS 数值 IK（M2）：任意 N-DoF RobotSpec 的末端位置 + 工具朝向——
/// Panda 7 轴等非 4 轴构型的脚本/数值基线共用。纯浮点，无物理依赖。
#[cfg(feature = "spec")]
pub mod ik;
#[cfg(feature = "rigid3d")]
pub mod lerobot;
/// Run provenance manifests (M0b P-3): `run_manifest.json` writers and
/// the compile-time `build_info()` the Python bridge mirrors.
#[cfg(feature = "spec")]
pub mod manifest;
#[cfg(feature = "urdf")]
pub mod meshres;
#[cfg(feature = "spec")]
pub mod spec;
#[cfg(feature = "urdf")]
pub mod urdf;
