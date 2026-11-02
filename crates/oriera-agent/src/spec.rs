//! SceneSpec (M0a schema v2, PLAN-embodied-general §3.1) — the
//! experiment-as-file contract: a JSON scene description that fully
//! determines a [`TableTopEnv`] (robot, furniture, props, task
//! thresholds, rates, solver knobs, randomization ranges).
//!
//! The file IS the experiment identity: two labs exchanging a spec
//! JSON exchange bit-identical episode streams (same seeds ⇒ same
//! fingerprints), so the spec's own fingerprint is pinned as a golden
//! contract in the tests. v2 hardens the pre-M0a draft along the
//! review P-4 line: a versioned `schema` field, `deny_unknown_fields`
//! everywhere (typos fail loudly — plus a recursive key audit for the
//! subsys-side structs), a `validate()` that names the offending
//! field and its legal values (G-7), a `migrate_v1` path (the old
//! `desc`/`scene`/`task`-enum drafts migrate losslessly), and
//! `spec_hash()` — a canonical (key-sorted, `meta`-stripped)
//! serialization folded through splitmix64, written into every
//! receipt and eval report.
//!
//! **M0b spec library (P-9)**: a top-level `"extends": "<name>"` makes
//! the file an *overlay* on `specs/official/<name>.json` (see
//! [`resolve_extends`] / [`merge_overlay`]); resolution happens at load,
//! the resolved spec is what gets validated and hashed, and the key
//! never reaches the typed struct. `specs/lab/` holds overlays;
//! `scripts/gen_spec_index.py` writes `specs/INDEX.json` (name → hash,
//! same-name-different-hash is an error).
//!
//! ```json
//! { "schema": "oriera.scene/2", "meta": {"name": "..."},
//!   "robot": {RobotSpec}, "scene": {SceneGraph},
//!   "task": {"id": "pick_place@v1", "params": {}},
//!   "control": {"mode": "joint_pos"}, "dt": ..., "max_steps": ...,
//!   "randomization": {"enabled": true}, "physics": {} }
//! ```

use crate::embodied_env::{SceneGraph, TableTopEnv, TaskKind, TaskParams};
use oriera_subsys::{ArmDescription, PhysicsParams, RobotSpec, Scene3D};
use serde::{Deserialize, Serialize};

/// The v2 schema tag (`spec.schema` must equal this).
pub const SCHEMA_V2: &str = "oriera.scene/2";

/// Provenance block — deliberately **excluded from
/// [`SceneSpec::spec_hash`]**: renaming a lab or rewording a note must
/// never change an experiment's identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpecMeta {
    pub name: String,
    pub author: Option<String>,
    pub notes: Option<String>,
}

/// Which task runs (`task_registry` id, e.g. `"pick_place@v1"`) and
/// its thresholds. `params` omitted ⇒ the v1 defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRef {
    pub id: String,
    #[serde(default)]
    pub params: TaskParams,
}

/// Joint-position control with the action scale taken from the robot
/// (review U-10: was the hardcoded ×2.0 in the Python wrapper).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Control {
    pub mode: ControlMode,
    /// Per-joint scale (rad per unit action); `None` ⇒
    /// `robot.action_scale` verbatim.
    pub action_scale: Option<Vec<f32>>,
}

/// Control mode. M0a ships exactly one (target-position servos).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlMode {
    #[default]
    JointPos,
}

/// Reset-mode domain randomization (IsaacLab EventTerm ranges).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Randomization {
    /// `randomize` of the v1 drafts.
    pub enabled: bool,
    /// Motor-gain jitter half-range; 0.0 off.
    pub kp_jitter: f32,
    /// Link-mass scale jitter half-range; 0.0 off.
    pub mass_jitter: f32,
    /// Additive observation noise; 0.0 off.
    pub obs_noise: f32,
    /// Absolute prop density draw range (kg/m³), props[0] only.
    pub prop_density: [f32; 2],
    /// Absolute prop friction draw range, props[0] only.
    pub prop_friction: [f32; 2],
}

impl Default for Randomization {
    fn default() -> Self {
        Self {
            enabled: true,
            kp_jitter: 0.0,
            mass_jitter: 0.0,
            obs_noise: 0.0,
            prop_density: [240.0, 640.0],
            prop_friction: [0.5, 1.1],
        }
    }
}

/// A complete, shareable experiment description (schema v2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSpec {
    /// Must be [`SCHEMA_V2`] — the version gate (P-4).
    pub schema: String,
    #[serde(default)]
    pub meta: SpecMeta,
    /// The robot (M0a: N-DoF [`RobotSpec`]; the 4-DoF preset is a
    /// `From` projection of the legacy `ArmDescription`).
    pub robot: RobotSpec,
    /// Fixtures + props with shapes and placements (G-3: the cube
    /// finally lives in the spec).
    pub scene: SceneGraph,
    pub task: TaskRef,
    #[serde(default)]
    pub control: Control,
    pub dt: f32,
    pub max_steps: u64,
    #[serde(default)]
    pub randomization: Randomization,
    #[serde(default)]
    pub physics: PhysicsParams,
}

/// A parsed spec plus how it got here.
#[derive(Debug, Clone)]
pub struct LoadedSpec {
    pub spec: SceneSpec,
    /// `true` when the input was a v1 draft migrated on the fly.
    pub migrated: bool,
}

impl SceneSpec {
    /// The default tabletop experiment (= [`TableTopEnv::new`] and the
    /// legacy `--task pick_place` path), as a spec.
    pub fn preset(task: TaskKind) -> Self {
        Self {
            schema: SCHEMA_V2.into(),
            meta: SpecMeta {
                name: "tabletop-4dof-pick".into(),
                author: None,
                notes: Some("引擎默认桌面实验的规格化等价物（M0a golden）".into()),
            },
            robot: RobotSpec::from(&ArmDescription::default()),
            scene: SceneGraph::default(),
            task: TaskRef {
                id: task.id().into(),
                params: TaskParams::default(),
            },
            control: Control::default(),
            dt: 1.0 / 60.0,
            max_steps: 700,
            randomization: Randomization::default(),
            physics: PhysicsParams::default(),
        }
    }

    /// Build the environment this spec describes.
    pub fn to_env(&self) -> TableTopEnv {
        let mut env = TableTopEnv::from_parts(
            self.robot.clone(),
            self.scene.clone(),
            self.dt,
            self.max_steps,
        );
        let base = self.task.id.split('@').next().unwrap_or(&self.task.id);
        env.task = TaskKind::parse(base)
            .unwrap_or_else(|| panic!("spec.task.id：无法解析 {base:?}（应已被 validate 拦下）"));
        env.task_params = self.task.params;
        env.randomize = self.randomization.enabled;
        env.kp_jitter = self.randomization.kp_jitter;
        env.mass_jitter = self.randomization.mass_jitter;
        env.obs_noise = self.randomization.obs_noise;
        env.prop_density_range = self.randomization.prop_density;
        env.prop_friction_range = self.randomization.prop_friction;
        env.physics = self.physics;
        if let Some(scale) = &self.control.action_scale {
            env.robot.action_scale = scale.clone();
        }
        env
    }

    /// Structural + semantic validation: names the offending field and
    /// its legal values (G-7), never a silent default.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA_V2 {
            return Err(format!(
                "spec.schema：须为 {:?}，当前 {:?}（旧版用 `--migrate` 迁移）",
                SCHEMA_V2, self.schema
            ));
        }
        self.robot
            .validate()
            .map_err(|e| format!("spec.robot：{e}"))?;
        self.scene
            .validate()
            .map_err(|e| format!("spec.{e}"))?;
        let params = &self.task.params;
        params.validate().map_err(|e| format!("spec.task.{e}"))?;
        crate::embodied_env::make_task(&self.task.id, *params)
            .map_err(|e| format!("spec.{e}"))?;
        let dof = self.robot.joints.len();
        if let Some(scale) = &self.control.action_scale {
            if scale.len() != dof {
                return Err(format!(
                    "spec.control.action_scale：{} 个分量，须与关节数一致（当前 {} 个关节；省略该字段即用 robot 自带的 {}）",
                    scale.len(),
                    dof,
                    self.robot.action_scale.len()
                ));
            }
            if scale.iter().any(|v| !v.is_finite() || *v <= 0.0) {
                return Err("spec.control.action_scale：每个分量须为有限正数（rad/单位动作）".into());
            }
        }
        if !(self.dt.is_finite() && self.dt > 0.0) {
            return Err(format!(
                "spec.dt = {}: 须为有限正数（秒；默认 1/60 = {}）",
                self.dt,
                1.0 / 60.0
            ));
        }
        if self.max_steps == 0 {
            return Err("spec.max_steps = 0：至少要走 1 步".into());
        }
        let r = &self.randomization;
        for (name, rng) in [("prop_density", r.prop_density), ("prop_friction", r.prop_friction)] {
            if !(rng[0].is_finite() && rng[1].is_finite() && rng[0] <= rng[1]) {
                return Err(format!(
                    "spec.randomization.{name} = {:?}：须为 [lo, hi] 且 lo ≤ hi",
                    rng
                ));
            }
        }
        if !(r.kp_jitter.is_finite() && r.kp_jitter >= 0.0) {
            return Err(format!(
                "spec.randomization.kp_jitter = {}: 须有限且 ≥ 0（0 = 关闭）",
                r.kp_jitter
            ));
        }
        if !(r.mass_jitter.is_finite() && r.mass_jitter >= 0.0) {
            return Err(format!(
                "spec.randomization.mass_jitter = {}: 须有限且 ≥ 0（0 = 关闭）",
                r.mass_jitter
            ));
        }
        if !(r.obs_noise.is_finite() && r.obs_noise >= 0.0) {
            return Err(format!(
                "spec.randomization.obs_noise = {}: 须有限且 ≥ 0（0 = 关闭）",
                r.obs_noise
            ));
        }
        Ok(())
    }

    /// The experiment identity: serialize (keys sorted — serde_json's
    /// default map), drop `meta`, fold the canonical JSON bytes
    /// through splitmix64. Goes into `eval_report.json` and the
    /// LeRobot receipt: same spec_hash + same seeds ⇒ same episodes.
    ///
    /// Canonicalization (M0b): an explicitly written legacy plate axis
    /// (`approach_axis: [1,0,0]` / `gap_axis: [0,0,1]`) hashes the same
    /// as an omitted one — same gripper, same identity.
    pub fn spec_hash(&self) -> u64 {
        let mut canon = self.clone();
        if let Some(g) = canon.robot.gripper.as_mut() {
            g.canonicalize_axes();
        }
        let mut v = serde_json::to_value(&canon).expect("SceneSpec serializes");
        v.as_object_mut()
            .expect("top level is a map")
            .remove("meta");
        fold_json(v.to_string().as_bytes())
    }

    /// `spec_hash` as 16 hex digits (the manifest/report form).
    pub fn spec_hash_hex(&self) -> String {
        format!("{:016x}", self.spec_hash())
    }

    /// First 8 hex digits of the spec hash — the `runs/<hash8>/` directory key.
    pub fn spec_hash8(&self) -> String {
        self.spec_hash_hex()[..8].to_string()
    }

    /// Parse a v2 spec from JSON text: `extends` resolution (against
    /// the official library), schema gate, unknown-key audit (also over
    /// the subsys-side structs that can't carry `deny_unknown_fields`),
    /// then [`Self::validate`].
    #[cfg(feature = "json")]
    pub fn from_json_v2(text: &str) -> Result<Self, String> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("spec JSON 非法：{e}"))?;
        let v = resolve_extends(v, 0, &mut Vec::new())?;
        Self::from_value(v)
    }

    /// Typed parse from a JSON value (the `--set` override path lands
    /// here after mutation).
    #[cfg(feature = "json")]
    pub fn from_value(v: serde_json::Value) -> Result<Self, String> {
        let spec: Self = serde_json::from_value(v.clone())
            .map_err(|e| format!("spec 字段非法：{e}"))?;
        if let Some(report) = unknown_key_report(&v, &serde_json::to_value(&spec).expect("reserialize")) {
            return Err(report);
        }
        spec.validate()?;
        Ok(spec)
    }

    /// Load v2 or auto-migrate v1 (the old `desc`/`scene`/`task`-enum
    /// drafts) — migration is lossless for the defaults by construction.
    /// A top-level `extends` (M0b spec library) is resolved first: the
    /// named official spec is the base, this file is an overlay.
    #[cfg(feature = "json")]
    pub fn load(text: &str) -> Result<LoadedSpec, String> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("spec JSON 非法：{e}"))?;
        let v = resolve_extends(v, 0, &mut Vec::new())?;
        let is_v1 = v.get("schema").is_none();
        if !is_v1 {
            return Ok(LoadedSpec {
                spec: Self::from_value(v)?,
                migrated: false,
            });
        }
        let spec = Self::migrate_value(v)?;
        Ok(LoadedSpec { spec, migrated: true })
    }

    /// Read a spec file from disk ([`Self::load`]).
    #[cfg(feature = "json")]
    pub fn load_file(path: &std::path::Path) -> Result<LoadedSpec, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读 spec 失败（{}）：{e}", path.display()))?;
        Self::load(&text)
    }

    /// v1 → v2 migration (from parsed JSON): `desc` →
    /// `RobotSpec::from` (the WP-A preset projection reproduces the
    /// legacy build field by field), `Scene3D` → [`SceneGraph`] with
    /// the default cube prop, the `task` enum → its `@v1` registry id,
    /// the flat randomization knobs → the `randomization` block.
    #[cfg(feature = "json")]
    pub fn migrate_value(v: serde_json::Value) -> Result<Self, String> {
        let old: SpecV1 = serde_json::from_value(v)
            .map_err(|e| format!("v1 spec 迁移失败：{e}"))?;
        Ok(Self::migrate_v1_struct(&old))
    }

    /// v1 → v2 migration from JSON text.
    #[cfg(feature = "json")]
    pub fn migrate_v1(text: &str) -> Result<Self, String> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("spec JSON 非法：{e}"))?;
        Self::migrate_value(v)
    }

    #[cfg(feature = "json")]
    fn migrate_v1_struct(old: &SpecV1) -> Self {
        Self {
            schema: SCHEMA_V2.into(),
            meta: SpecMeta {
                name: "migrated-from-v1".into(),
                author: None,
                notes: Some("由 v1（desc/scene/task 枚举）无损迁移".into()),
            },
            robot: RobotSpec::from(&old.desc),
            scene: SceneGraph::from_scene3d(&old.scene),
            task: TaskRef {
                id: TaskKind::from(old.task).id().into(),
                params: TaskParams::default(),
            },
            control: Control::default(),
            dt: old.dt,
            max_steps: old.max_steps,
            randomization: Randomization {
                enabled: old.randomize,
                kp_jitter: old.kp_jitter,
                mass_jitter: old.mass_jitter,
                obs_noise: old.obs_noise,
                ..Randomization::default()
            },
            physics: old.physics,
        }
    }

    /// Apply `--set a.b.c=v` overrides on this spec (the JSON value
    /// path): array indices as plain numbers (`scene.props.0.shape`),
    /// values parsed as JSON when possible (`=0.5`, `=true`,
    /// `={"kind":"cylinder",...}`), bare words stay strings. Unknown
    /// paths are an error — no silent creation.
    #[cfg(feature = "json")]
    pub fn apply_sets(&mut self, sets: &[String]) -> Result<(), String> {
        if sets.is_empty() {
            return Ok(());
        }
        let mut v = serde_json::to_value(&*self).expect("SceneSpec serializes");
        for s in sets {
            let (path, val) = s
                .split_once('=')
                .ok_or_else(|| format!("--set {s:?}：须为 a.b.c=v 形式"))?;
            let parsed: serde_json::Value = serde_json::from_str(val).unwrap_or_else(|_| {
                serde_json::Value::String(val.to_string())
            });
            set_json_path(&mut v, path, parsed)?;
        }
        *self = Self::from_value(v)?;
        Ok(())
    }
}

/// v1 mirror of the pre-M0a [`SceneSpec`] draft (`desc`/`scene`/`task`
/// enum) — kept only as the migration source.
#[cfg(feature = "json")]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecV1 {
    desc: ArmDescription,
    scene: Scene3D,
    task: TaskKindV1,
    dt: f32,
    max_steps: u64,
    #[serde(default = "default_true")]
    randomize: bool,
    #[serde(default)]
    kp_jitter: f32,
    #[serde(default)]
    mass_jitter: f32,
    #[serde(default)]
    obs_noise: f32,
    #[serde(default)]
    physics: PhysicsParams,
}

#[cfg(feature = "json")]
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskKindV1 {
    Reach,
    PickPlace,
    Push,
}

impl From<TaskKindV1> for TaskKind {
    fn from(t: TaskKindV1) -> Self {
        match t {
            TaskKindV1::Reach => Self::Reach,
            TaskKindV1::PickPlace => Self::PickPlace,
            TaskKindV1::Push => Self::Push,
        }
    }
}

#[cfg(feature = "json")]
fn default_true() -> bool {
    true
}

/// Set `value` at a dotted path (`props.0.shape`). Numeric segments
/// index arrays; everything else keys objects. Missing intermediates
/// are honest errors (P-4: no silent creation).
#[cfg(feature = "json")]
fn set_json_path(v: &mut serde_json::Value, path: &str, new: serde_json::Value) -> Result<(), String> {
    let mut segs: Vec<&str> = path.split('.').collect();
    let last = segs.pop().ok_or("--set：路径为空")?;
    let mut cur = v;
    for (i, seg) in segs.iter().enumerate() {
        let err = || format!("--set {path:?}：路径第 {} 段 {seg:?} 不存在", i + 1);
        match cur {
            serde_json::Value::Object(m) => {
                cur = m.get_mut(*seg).ok_or_else(err)?;
            }
            serde_json::Value::Array(a) => {
                let idx: usize = seg.parse().map_err(|_| {
                    format!("--set {path:?}：第 {} 段 {seg:?} 不是数组下标", i + 1)
                })?;
                cur = a.get_mut(idx).ok_or_else(err)?;
            }
            _other => {
                return Err(format!(
                    "--set {path:?}：第 {} 段撞上标量，无法继续下钻",
                    i + 1
                ))
            }
        }
    }
    match cur {
        serde_json::Value::Object(m) => {
            m.insert(last.to_string(), new);
            Ok(())
        }
        serde_json::Value::Array(a) => {
            let idx: usize = last
                .parse()
                .map_err(|_| format!("--set {path:?}：末段 {last:?} 不是数组下标"))?;
            if idx >= a.len() {
                return Err(format!(
                    "--set {path:?}：数组下标 {idx} 越界（长度 {}）",
                    a.len()
                ));
            }
            a[idx] = new;
            Ok(())
        }
        _ => Err(format!("--set {path:?}：末段落在标量上")),
    }
}

/// Recursive unknown-key audit: every object key present in the input
/// must survive the typed round-trip. This catches typos inside the
/// subsys-side structs (RobotSpec/LinkSpec/… live behind subsys' serde
/// and can't carry `deny_unknown_fields`) — P-4 "拼错字段静默默认" is
/// then impossible anywhere in the tree. Returns the first offender
/// with its path and the legal keys at that level.
#[cfg(feature = "json")]
fn unknown_key_report(input: &serde_json::Value, roundtrip: &serde_json::Value) -> Option<String> {
    match (input, roundtrip) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for (k, av) in a {
                match b.get(k) {
                    Some(bv) => {
                        if let Some(r) = unknown_key_report(av, bv) {
                            return Some(r);
                        }
                    }
                    None => {
                        let mut legal: Vec<&String> = b.keys().collect();
                        legal.sort();
                        let names: Vec<String> = legal.iter().map(|s| format!("\"{s}\"")).collect();
                        return Some(format!(
                            "spec：字段 {k:?} 不存在（这一层的合法字段：{}；拼错字段会静默用默认值——先改对再跑）",
                            names.join(", ")
                        ));
                    }
                }
            }
            None
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
            for (av, bv) in a.iter().zip(b.iter()) {
                if let Some(r) = unknown_key_report(av, bv) {
                    return Some(r);
                }
            }
            None
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Spec library (M0b, PLAN §9.1 P-9): `extends` inheritance against the
// governed `specs/official/` directory; lab specs are overlays.
// ---------------------------------------------------------------------------

/// Top-level key naming the official base spec (file stem under
/// [`official_specs_dir`]); the key never survives resolution.
pub const EXTENDS_KEY: &str = "extends";

/// Maximum inheritance depth (a chain longer than this is a mistake,
/// not a design).
const EXTENDS_MAX_DEPTH: usize = 8;

/// The governed spec library: `$ORIERA_SPECS_OFFICIAL` when set, else
/// the engine's `crates/oriera-agent/specs/official/` (compile-time
/// path — the library ships with the engine that resolves it).
pub fn official_specs_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ORIERA_SPECS_OFFICIAL") {
        if !dir.trim().is_empty() {
            return std::path::PathBuf::from(dir);
        }
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("specs")
        .join("official")
}

/// Resolve a top-level `extends` chain: load the named official spec
/// (recursively resolved), overlay this value on it (objects merge per
/// key, arrays merge index-wise with extra overlay items appended,
/// scalars replace), drop the key. Cycles and over-deep chains are
/// errors naming the chain; a missing base names the directory and the
/// candidates in it.
#[cfg(feature = "json")]
pub fn resolve_extends(
    v: serde_json::Value,
    depth: usize,
    chain: &mut Vec<String>,
) -> Result<serde_json::Value, String> {
    resolve_extends_in(&official_specs_dir(), v, depth, chain)
}

/// [`resolve_extends`] against an explicit library directory.
#[cfg(feature = "json")]
pub fn resolve_extends_in(
    dir: &std::path::Path,
    mut v: serde_json::Value,
    depth: usize,
    chain: &mut Vec<String>,
) -> Result<serde_json::Value, String> {
    let Some(obj) = v.as_object_mut() else {
        return Ok(v);
    };
    let Some(ext) = obj.remove(EXTENDS_KEY) else {
        return Ok(v);
    };
    let name = ext
        .as_str()
        .ok_or_else(|| format!("spec.extends：须为字符串（官方 spec 名，如 \"tabletop-4dof-pick\"），当前 {ext}"))?
        .trim()
        .trim_start_matches("official/")
        .trim_end_matches(".json")
        .to_string();
    if name.is_empty() {
        return Err("spec.extends：名字为空".into());
    }
    if depth >= EXTENDS_MAX_DEPTH {
        return Err(format!(
            "spec.extends：继承链过深（> {EXTENDS_MAX_DEPTH}）：{} → {name}",
            chain.join(" → ")
        ));
    }
    if chain.iter().any(|c| c == &name) {
        return Err(format!(
            "spec.extends：循环继承：{} → {name}",
            chain.join(" → ")
        ));
    }
    let path = dir.join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).map_err(|e| {
        let mut have: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let p = e.path();
                        (p.extension().and_then(|s| s.to_str()) == Some("json"))
                            .then(|| {
                                p.file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("")
                                    .to_string()
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();
        have.sort();
        format!(
            "spec.extends = {name:?}：官方库里没有（{}；可用：{}）：{e}",
            dir.display(),
            if have.is_empty() { "<空>".to_string() } else { have.join(", ") }
        )
    })?;
    let base: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("spec.extends = {name:?}：基 spec JSON 非法：{e}"))?;
    chain.push(name);
    let mut base = resolve_extends_in(dir, base, depth + 1, chain)?;
    merge_overlay(&mut base, v);
    Ok(base)
}

/// Overlay semantics for `extends`: objects merge per key, arrays merge
/// index-wise (extra overlay items appended), everything else replaces.
/// Two tagged-enum rules keep shapes swappable: an object whose `kind`
/// tag differs from the base's (`{"kind":"cylinder",…}` over
/// `{"kind":"cuboid",…}`) replaces it outright, and so does a
/// single-key externally tagged variant with a different key
/// (`{"Capsule":{…}}` over `{"Box":{…}}`) — a per-key merge would leave
/// the old variant's fields behind and trip `deny_unknown_fields`.
#[cfg(feature = "json")]
pub fn merge_overlay(base: &mut serde_json::Value, overlay: serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(b), serde_json::Value::Object(o)) => {
            let kind_changes = match (b.get("kind"), o.get("kind")) {
                (Some(bk), Some(ok)) => bk != ok,
                _ => false,
            };
            let variant_changes = b.len() == 1
                && o.len() == 1
                && b.keys().next() != o.keys().next()
                && b.values().next().is_some_and(|v| v.is_object())
                && o.values().next().is_some_and(|v| v.is_object());
            if kind_changes || variant_changes {
                *b = o;
                return;
            }
            for (k, ov) in o {
                match b.get_mut(&k) {
                    Some(bv) => merge_overlay(bv, ov),
                    None => {
                        b.insert(k, ov);
                    }
                }
            }
        }
        (serde_json::Value::Array(b), serde_json::Value::Array(o)) => {
            for (i, ov) in o.into_iter().enumerate() {
                if i < b.len() {
                    merge_overlay(&mut b[i], ov);
                } else {
                    b.push(ov);
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// splitmix64 byte fold — the house hash (same rounds as
/// `lerobot.rs::content_hash`); corruption detection, not crypto.
/// Shared with the eval protocol / manifest hashes (M0b).
pub(crate) fn fold_json(bytes: &[u8]) -> u64 {
    fn mix(mut h: u64, x: u64) -> u64 {
        h ^= x;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 29;
        h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        h ^= h >> 32;
        h
    }
    let mut h = 0x0000_6f72_6965_7261;
    for chunk in bytes.chunks(8) {
        let mut buf = [0u8; 8];
        buf[..chunk.len()].copy_from_slice(chunk);
        h = mix(h, u64::from_le_bytes(buf));
    }
    mix(h, bytes.len() as u64)
}

/// [`fold_json`] as 16 hex digits — the receipt/manifest form.
pub(crate) fn fold_json_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fold_json(bytes))
}

/// The checked-in golden v2 spec (`specs/tabletop-4dof-pick.json`), as
/// a literal — the file and this constant must stay identical
/// (contract test), and the parsed spec must reproduce the default env
/// bit-for-bit. Its `spec_hash` is pinned below (any field change
/// moves the experiment identity and must be deliberate).
pub const GOLDEN_TABLETOP_4DOF_PICK_SPEC: &str = include_str!("../specs/tabletop-4dof-pick.json");

/// The spec_hash (hex) of the golden v2 spec — the experiment-identity
/// anchor of M0a. `meta` is excluded, so renaming/editing notes never
/// moves it.
pub const GOLDEN_SPEC_HASH_HEX: &str = "c90869dcf6a04875";

/// The pre-M0a golden draft (`specs/pick_place.json`) — kept untouched
/// as the migration regression case: loading it must migrate to a spec
/// whose hash equals the v2 golden's (same experiment).
pub const GOLDEN_PICK_PLACE_SPEC_V1: &str = r#"{
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

    fn golden() -> SceneSpec {
        SceneSpec::from_json_v2(GOLDEN_TABLETOP_4DOF_PICK_SPEC).expect("golden v2 spec must parse")
    }

    #[cfg(feature = "json")]
    #[test]
    fn golden_v2_spec_reproduces_the_default_env_bit_for_bit() {
        let spec = golden();
        let mut env = spec.to_env();
        env.randomize = false; // 与默认 env 的逐位对照需同档位
        let mut default = TableTopEnv::new();
        default.randomize = false;
        let o1 = env.reset(42);
        let o2 = default.reset(42);
        assert_eq!(o1, o2, "golden v2 spec must reproduce the default env");
        for step in 0..30 {
            assert_eq!(env.step(&[0.0; 5]), default.step(&[0.0; 5]), "step {step}");
        }
        assert_eq!(env.fingerprint(), default.fingerprint());
    }

    #[cfg(feature = "json")]
    #[test]
    fn checked_in_v2_file_matches_the_literal_and_the_hash_anchor() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/specs/tabletop-4dof-pick.json"
        );
        let on_disk = std::fs::read_to_string(path).expect("read golden v2 spec");
        assert_eq!(
            on_disk.trim_end(),
            GOLDEN_TABLETOP_4DOF_PICK_SPEC.trim_end(),
            "specs/tabletop-4dof-pick.json and the literal must stay identical"
        );
        assert_eq!(golden().spec_hash_hex(), GOLDEN_SPEC_HASH_HEX);
    }

    #[cfg(feature = "json")]
    #[test]
    fn v1_migrates_to_the_same_experiment() {
        let old = SceneSpec::load(GOLDEN_PICK_PLACE_SPEC_V1).expect("v1 loads");
        assert!(old.migrated);
        let fresh = golden();
        // Same robot/scene/task ⇒ same canonical spec ⇒ same hash
        // (meta is excluded; only the name differs).
        assert_eq!(old.spec.spec_hash(), fresh.spec_hash());
        // …and the on-disk legacy file migrates identically.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/specs/pick_place.json");
        let disk = SceneSpec::load_file(std::path::Path::new(path)).expect("v1 file loads");
        assert!(disk.migrated);
        assert_eq!(disk.spec.spec_hash(), fresh.spec_hash());
        let mut env = disk.spec.to_env();
        env.randomize = false;
        let mut default = TableTopEnv::new();
        default.randomize = false;
        assert_eq!(env.reset(7), default.reset(7));
    }

    #[cfg(feature = "json")]
    #[test]
    fn unknown_field_is_reported_with_path_and_legal_values() {
        // nested subsys-side struct (no deny_unknown_fields there)
        let mut v = serde_json::to_value(golden()).unwrap();
        v["robot"]["joints"][0]["kpp"] = serde_json::json!(1.0);
        let err = SceneSpec::from_value(v).unwrap_err();
        assert!(err.contains("kpp") && err.contains("合法字段"), "{err}");
        // a required-field typo reads as a normal serde error…
        let mut v = serde_json::to_value(golden()).unwrap();
        v["robot"].as_object_mut().unwrap().remove("base");
        assert!(SceneSpec::from_value(v).unwrap_err().contains("base"));
        // …while an unknown key anywhere is reported with its path's
        // legal fields (top-level agent-side deny_unknown_fields)
        let mut v = serde_json::to_value(golden()).unwrap();
        v["dts"] = serde_json::json!(0.1);
        assert!(SceneSpec::from_value(v).unwrap_err().contains("dts"));
        let mut v = serde_json::to_value(golden()).unwrap();
        v["scene"]["props"][0]["halfx"] = serde_json::json!(1.0);
        let err = SceneSpec::from_value(v).unwrap_err();
        assert!(err.contains("halfx"), "{err}");
    }

    #[cfg(feature = "json")]
    #[test]
    fn validate_names_the_offending_field_and_legal_values() {
        let mut spec = golden();
        spec.task.id = "fly@v9".into();
        let err = spec.validate().unwrap_err();
        assert!(err.contains("task") && err.contains("pick_place@v1"), "{err}");
        let mut spec = golden();
        spec.control.action_scale = Some(vec![2.0; 3]);
        let err = spec.validate().unwrap_err();
        assert!(err.contains("action_scale") && err.contains("4"), "{err}");
        let mut spec = golden();
        spec.schema = "oriera.scene/1".into();
        assert!(spec.validate().unwrap_err().contains("oriera.scene/2"));
        let mut spec = golden();
        spec.max_steps = 0;
        assert!(spec.validate().unwrap_err().contains("max_steps"));
    }

    #[cfg(feature = "json")]
    #[test]
    fn spec_hash_is_stable_and_meta_does_not_count() {
        let a = golden();
        let mut b = a.clone();
        b.meta.name = "renamed".into();
        b.meta.author = Some("someone".into());
        b.meta.notes = None;
        assert_eq!(a.spec_hash(), b.spec_hash(), "meta must not count");
        // any real field moves the hash
        let mut c = a.clone();
        c.max_steps = 699;
        assert_ne!(a.spec_hash(), c.spec_hash());
        // and it is stable across calls
        assert_eq!(a.spec_hash(), a.spec_hash());
    }

    #[cfg(feature = "json")]
    #[test]
    fn set_overrides_change_the_spec_and_land_in_the_hash() {
        let mut spec = golden();
        let before = spec.spec_hash();
        spec.apply_sets(&[format!(
            "scene.props.0.shape={}",
            r#"{"kind":"cylinder","half_h":0.03,"radius":0.025}"#
        )])
        .expect("sets apply");
        assert_ne!(spec.spec_hash(), before, "overrides are experiment identity");
        assert!(format!("{:?}", spec.scene.props[0].shape).contains("Cylinder"));
        // plain-word / numeric values parse too
        let mut spec = golden();
        spec.apply_sets(&["scene.props.0.name=ball".into(), "max_steps=500".into()])
            .expect("scalar sets apply");
        assert_eq!(spec.scene.props[0].name, "ball");
        assert_eq!(spec.max_steps, 500);
        // unknown path is an error, not a silent create
        let mut spec = golden();
        let err = spec.apply_sets(&["scene.nope.1=3".into()]).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        // a set that fails validation is caught
        let mut spec = golden();
        assert!(spec.apply_sets(&["dt=-1".into()]).is_err());
    }

    #[cfg(feature = "json")]
    #[test]
    fn preset_matches_the_default_env() {
        let mut env = SceneSpec::preset(TaskKind::PickPlace).to_env();
        env.randomize = false;
        let mut default = TableTopEnv::new();
        default.randomize = false;
        assert_eq!(env.reset(3), default.reset(3));
    }

    #[cfg(feature = "json")]
    #[test]
    fn malformed_spec_is_an_error_not_a_panic() {
        assert!(SceneSpec::from_json_v2("{ nope").is_err());
        assert!(SceneSpec::load(GOLDEN_PICK_PLACE_SPEC_V1).is_ok());
    }

    // --- M0b: spec library (`extends`) + plate-axis canonical hash -----

    /// The official copy of the golden IS the golden (byte-identical,
    /// same hash) — two paths, one experiment identity.
    #[cfg(feature = "json")]
    #[test]
    fn official_golden_is_the_checked_in_golden() {
        let official = official_specs_dir().join("tabletop-4dof-pick.json");
        let on_disk = std::fs::read_to_string(&official).expect("official golden exists");
        assert_eq!(on_disk.trim_end(), GOLDEN_TABLETOP_4DOF_PICK_SPEC.trim_end());
        let spec = SceneSpec::load_file(&official).expect("official loads").spec;
        assert_eq!(spec.spec_hash_hex(), GOLDEN_SPEC_HASH_HEX);
    }

    /// A lab overlay resolves against the official base: everything not
    /// written comes from the base, the overridden shape lands, the
    /// resolved spec (not the overlay text) is what gets hashed, and
    /// `extends` never survives into the typed spec.
    #[cfg(feature = "json")]
    #[test]
    fn extends_resolves_against_official_and_hashes_the_result() {
        let lab = concat!(env!("CARGO_MANIFEST_DIR"), "/specs/lab/example-cylinder.json");
        let loaded = SceneSpec::load_file(std::path::Path::new(lab)).expect("lab spec resolves");
        assert!(!loaded.migrated);
        let s = &loaded.spec;
        assert_eq!(s.meta.name, "lab-example-cylinder");
        assert_eq!(s.scene.props[0].name, "cylinder");
        assert!(
            format!("{:?}", s.scene.props[0].shape).contains("Cylinder"),
            "{:?}",
            s.scene.props[0].shape
        );
        // inherited, not written in the overlay
        assert_eq!(s.robot.dof(), 4);
        assert_eq!(s.max_steps, golden().max_steps);
        assert_eq!(s.scene.fixtures.len(), golden().scene.fixtures.len());
        // identity moved (shape is identity), and equals the same change made by hand
        assert_ne!(s.spec_hash_hex(), GOLDEN_SPEC_HASH_HEX);
        let mut by_hand = golden();
        by_hand
            .apply_sets(&[
                "scene.props.0.name=cylinder".into(),
                format!(
                    "scene.props.0.shape={}",
                    r#"{"kind":"cylinder","half_h":0.03,"radius":0.025}"#
                ),
            ])
            .unwrap();
        assert_eq!(s.spec_hash(), by_hand.spec_hash(), "resolved overlay == hand-edited golden");
        // the serialized resolved spec carries no `extends`
        let v = serde_json::to_value(s).unwrap();
        assert!(v.get(EXTENDS_KEY).is_none());
    }

    /// Chains resolve transitively; cycles, missing bases and over-deep
    /// chains are named errors.
    #[cfg(feature = "json")]
    #[test]
    fn extends_errors_name_the_chain_and_the_candidates() {
        let err = SceneSpec::from_json_v2(r#"{"extends":"no-such-spec"}"#).unwrap_err();
        assert!(err.contains("no-such-spec") && err.contains("tabletop-4dof-pick"), "{err}");
        let err = SceneSpec::from_json_v2(r#"{"extends": 7}"#).unwrap_err();
        assert!(err.contains("extends"), "{err}");
        // a self-referencing chain through a temp library dir (explicit
        // dir — no process-global env var, so parallel tests stay clean)
        let dir = std::env::temp_dir().join(format!("oriera-spec-cycle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), r#"{"extends":"b"}"#).unwrap();
        std::fs::write(dir.join("b.json"), r#"{"extends":"a"}"#).unwrap();
        let mut chain = Vec::new();
        let a: serde_json::Value = serde_json::from_str(r#"{"extends":"a"}"#).unwrap();
        let err = resolve_extends_in(&dir, a, 0, &mut chain).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.contains("循环"), "{err}");
    }

    /// Overlay rules: per-key object merge, index-wise arrays, tagged
    /// shapes replace when the tag changes (no stale variant fields).
    #[cfg(feature = "json")]
    #[test]
    fn merge_overlay_rules() {
        let mut base = serde_json::json!({
            "a": {"x": 1, "y": 2},
            "arr": [{"n": 1, "shape": {"kind": "cuboid", "half": [1,1,1]}}, {"n": 2}],
            "link": {"shape": {"Box": {"half": [1,2,3]}}},
            "s": "old"
        });
        merge_overlay(
            &mut base,
            serde_json::json!({
                "a": {"y": 20, "z": 30},
                "arr": [
                    {"shape": {"kind": "cylinder", "half_h": 0.1, "radius": 0.2}},
                    {},
                    {"n": 3}
                ],
                "link": {"shape": {"Capsule": {"half_len": 1.0, "radius": 0.1}}},
                "s": "new"
            }),
        );
        assert_eq!(base["a"], serde_json::json!({"x": 1, "y": 20, "z": 30}));
        assert_eq!(base["arr"][0]["n"], 1, "untouched sibling key survives");
        assert_eq!(
            base["arr"][0]["shape"],
            serde_json::json!({"kind": "cylinder", "half_h": 0.1, "radius": 0.2}),
            "kind change replaces, no stale `half`"
        );
        assert_eq!(base["arr"][1], serde_json::json!({"n": 2}));
        assert_eq!(base["arr"][2], serde_json::json!({"n": 3}), "extra items append");
        assert_eq!(
            base["link"]["shape"],
            serde_json::json!({"Capsule": {"half_len": 1.0, "radius": 0.1}}),
            "externally tagged variant replaces"
        );
        assert_eq!(base["s"], "new");
    }

    /// Plate-frame axes: omitted == explicit default for the hash (and
    /// the explicit form passes the unknown-key audit), while a real
    /// orientation is identity.
    #[cfg(feature = "json")]
    #[test]
    fn gripper_axes_canonicalize_in_the_spec_hash() {
        let mut v = serde_json::to_value(golden()).unwrap();
        v["robot"]["gripper"]["approach_axis"] = serde_json::json!([1.0, 0.0, 0.0]);
        v["robot"]["gripper"]["gap_axis"] = serde_json::json!([0.0, 0.0, 1.0]);
        let explicit = SceneSpec::from_value(v).expect("explicit default axes parse");
        assert_eq!(explicit.spec_hash_hex(), GOLDEN_SPEC_HASH_HEX, "explicit default == omitted");
        let mut v = serde_json::to_value(golden()).unwrap();
        v["robot"]["gripper"]["approach_axis"] = serde_json::json!([0.0, 0.0, -1.0]);
        v["robot"]["gripper"]["gap_axis"] = serde_json::json!([0.0, 1.0, 0.0]);
        let down = SceneSpec::from_value(v).expect("oriented axes parse");
        assert_ne!(down.spec_hash_hex(), GOLDEN_SPEC_HASH_HEX, "orientation is identity");
        // and a bad pair is a named validation error, not a panic
        let mut v = serde_json::to_value(golden()).unwrap();
        v["robot"]["gripper"]["gap_axis"] = serde_json::json!([1.0, 0.0, 0.0]);
        let err = SceneSpec::from_value(v).unwrap_err();
        assert!(err.contains("parallel"), "{err}");
    }
}
