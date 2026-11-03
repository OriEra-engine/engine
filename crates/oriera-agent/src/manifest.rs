//! Run provenance (M0b, PLAN-embodied-general §9.1 P-3): every
//! collect/train run writes a `run_manifest.json` that answers "what
//! EXACTLY produced this data" half a year later — resolved spec,
//! spec hash, engine git SHA + dirty state, Cargo.lock hash, rustc,
//! rapier, features, OS/arch, the command line and the protocol id.
//!
//! Sourcing rules (honest fallbacks, never a silent lie):
//! - **git SHA / dirty**: asked from git at *manifest-write time*
//!   (workspace = what you ran, dirty counted from `status --porcelain`);
//!   no git / not a repo ⇒ `"unknown"` plus the reason.
//! - **rustc**: `rustc -V` on PATH when the manifest is written;
//!   absent ⇒ `"unknown"` (a wheel/runtime box without a toolchain).
//! - **rapier**: read out of the workspace `Cargo.lock` (the version
//!   the build actually resolved), `"unknown"` without the file.
//! - **Cargo.lock hash**: house splitmix fold of the file bytes.
//!
//! ```no_run
//! use oriera_agent::embodied_env::TaskKind;
//! use oriera_agent::manifest::{write_run_manifest, ManifestExtra};
//! use oriera_agent::spec::SceneSpec;
//! let spec = SceneSpec::preset(TaskKind::PickPlace);
//! write_run_manifest(
//!     "runs/abcd1234/scripted/1000/run_manifest.json",
//!     &spec,
//!     &ManifestExtra {
//!         protocol_id: "course@v1".into(),
//!         protocol_hash: "0123456789abcdef".into(),
//!         method: "scripted".into(),
//!         seed: Some(1000),
//!     },
//! ).unwrap();
//! ```

use crate::spec::SceneSpec;
use serde::Serialize;
use std::path::Path;

/// Manifest schema tag (`run_manifest.schema`).
pub const RUN_MANIFEST_SCHEMA: &str = "oriera.run-manifest/1";

/// Compile-time build facts shared with the Python bridge
/// (`oriera_sim.build_info()` returns exactly this).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BuildInfo {
    /// `CARGO_PKG_VERSION` of the engine crate.
    pub version: String,
    /// Enabled feature flags of this build (fixed order).
    pub features: Vec<String>,
    /// `"debug"` or `"release"`.
    pub profile: String,
}

/// The enabled feature flags of THIS build, in fixed order (P-3: the
/// feature set is provenance — a `cam` run and a `--no-cam` run are
/// different worlds).
pub fn feature_list() -> Vec<String> {
    let mut f = Vec::new();
    if cfg!(feature = "rigid3d") {
        f.push("rigid3d".into());
    }
    if cfg!(feature = "cam") {
        f.push("cam".into());
    }
    if cfg!(feature = "spec") {
        f.push("spec".into());
    }
    if cfg!(feature = "urdf") {
        f.push("urdf".into());
    }
    if cfg!(feature = "json") {
        f.push("json".into());
    }
    if cfg!(feature = "python") {
        f.push("python".into());
    }
    f
}

/// [`BuildInfo`] of the running binary.
pub fn build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        features: feature_list(),
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
        .into(),
    }
}

/// `feature_list()` joined the way the receipts print it
/// (`rigid3d+cam+spec`); empty ⇒ `"default"`.
pub fn features_string() -> String {
    let f = feature_list();
    if f.is_empty() {
        "default".into()
    } else {
        f.join("+")
    }
}

/// The engine workspace root: `crates/oriera-agent/../..` at compile
/// time, overridable at runtime with `ORIERA_ENGINE_ROOT` (e.g. when a
/// moved checkout writes manifests).
fn engine_root() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("ORIERA_ENGINE_ROOT") {
        if !p.trim().is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn git_output(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// `(sha, dirty_count | None)` from the engine checkout. `sha`/count
/// are `unknown`/`None` with a reason when git is missing or the tree
/// is not a repo (binary-only/wheel environments).
pub fn git_provenance() -> (String, Option<usize>, String) {
    let root = engine_root();
    let in_repo = git_output(&[
        "-C",
        root.to_string_lossy().as_ref(),
        "rev-parse",
        "--is-inside-work-tree",
    ]);
    if in_repo.as_deref() != Some("true") {
        // Fall through: try the process cwd (a data dir inside the repo
        // still resolves through git's own discovery).
        if git_output(&["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
            return (
                "unknown".into(),
                None,
                "git 不可用或运行目录不在引擎仓库内（wheel/独立二进制环境）——复现请以 Cargo.lock hash + rustc 版本为准".into(),
            );
        }
        let sha = git_output(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
        let dirty = git_output(&["status", "--porcelain"]).map(|s| s.lines().count());
        return (
            sha,
            dirty,
            "来自进程工作目录（引擎根定位失败，用 ORIERA_ENGINE_ROOT 指到 checkout 可消掉这行）"
                .into(),
        );
    }
    let sha = git_output(&["-C", root.to_string_lossy().as_ref(), "rev-parse", "HEAD"])
        .unwrap_or_else(|| "unknown".into());
    let dirty = git_output(&[
        "-C",
        root.to_string_lossy().as_ref(),
        "status",
        "--porcelain",
    ])
    .map(|s| s.lines().count());
    (sha, dirty, String::new())
}

/// `rustc -V` at manifest time; `"unknown"` without a toolchain.
pub fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("-V")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// The house splitmix fold of arbitrary bytes (hex-16) — same family
/// as the spec hash; shared with the index / lockfile hashing.
pub fn hash_bytes_hex(bytes: &[u8]) -> String {
    crate::spec::fold_json_hex(bytes)
}

/// `(version, lock_hash_hex)` for `rapier3d` + the whole `Cargo.lock`
/// file, read from the workspace lock at manifest time.
pub fn cargo_lock_provenance() -> (String, Option<String>, String) {
    let path = engine_root().join("Cargo.lock");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return (
                "unknown".into(),
                None,
                format!("Cargo.lock 不可读（{}）：{e}", path.display()),
            )
        }
    };
    let lock_hash = hash_bytes_hex(&bytes);
    // The resolved rapier3d version: the `[[package]]` block this build
    // linked against (grep the lock text — no new deps for this).
    let text = String::from_utf8_lossy(&bytes);
    let mut rapier = "unknown".to_string();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() == "name = \"rapier3d\"" {
            if let Some(v) = lines
                .next()
                .filter(|l| l.trim().starts_with("version = "))
                .map(|l| {
                    l.trim()
                        .trim_start_matches("version = \"")
                        .trim_end_matches('"')
                        .to_string()
                })
            {
                rapier = v;
            }
            break;
        }
    }
    (rapier, Some(lock_hash), path.display().to_string())
}

/// Everything the caller knows that the module can't derive.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ManifestExtra {
    /// Protocol id (`course@v1`) — empty when no protocol was used.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub protocol_id: String,
    /// Protocol parameter hash (hex) — empty when no protocol was used.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub protocol_hash: String,
    /// Data-collection method label (`scripted`, …) — the
    /// `runs/<hash8>/<method>/<seed>/` segment.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub method: String,
    /// The run's seed0 (Some for collect runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

/// The manifest document (serialized pretty to `run_manifest.json`).
#[derive(Debug, Serialize)]
pub struct RunManifest {
    pub schema: String,
    pub created_unix: u64,
    pub engine: EngineBlock,
    pub build: BuildBlock,
    pub python: PythonBlock,
    pub command_line: Vec<String>,
    pub protocol: ProtocolBlock,
    pub method: String,
    pub seed: Option<u64>,
    /// The FULL resolved spec — the file is the experiment; the
    /// manifest carries a verbatim copy so `runs/<hash8>/` is
    /// self-describing even if the spec file moves.
    pub spec: serde_json::Value,
    pub spec_hash: String,
}

#[derive(Debug, Serialize)]
pub struct EngineBlock {
    pub name: String,
    pub version: String,
    pub git_sha: String,
    /// Number of modified/untracked entries when the manifest was
    /// written; `null` = not determinable.
    pub git_dirty: Option<usize>,
    /// Non-empty = why git is `unknown` / which tree it came from.
    pub git_note: String,
    pub rapier_version: String,
    pub cargo_lock_hash: Option<String>,
    pub cargo_lock_path: String,
}

#[derive(Debug, Serialize)]
pub struct BuildBlock {
    pub rustc: String,
    pub features: Vec<String>,
    pub profile: String,
    pub os: String,
    pub arch: String,
    pub family: String,
}

#[derive(Debug, Serialize)]
pub struct PythonBlock {
    /// The engine version this cdylib was built as; the Python side
    /// (`scripts/oriera_gym.py::check_build`) compares the loaded pyd's
    /// `build_info()` against a run's manifest and warns on mismatch —
    /// an old .pyd silently loaded is exactly the P-3 failure mode.
    pub engine_version_expected: String,
    pub package_version: Option<String>,
    pub note: String,
}

#[derive(Debug, Serialize)]
pub struct ProtocolBlock {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub parameter_hash: String,
}

impl RunManifest {
    pub fn new(spec: &SceneSpec, extra: &ManifestExtra) -> Self {
        let (sha, dirty, note) = git_provenance();
        let (rapier, lock_hash, lock_path) = cargo_lock_provenance();
        let bi = build_info();
        Self {
            schema: RUN_MANIFEST_SCHEMA.into(),
            created_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            engine: EngineBlock {
                name: "oriera-engine".into(),
                version: bi.version.clone(),
                git_sha: sha,
                git_dirty: dirty,
                git_note: note,
                rapier_version: rapier,
                cargo_lock_hash: lock_hash,
                cargo_lock_path: lock_path,
            },
            build: BuildBlock {
                rustc: rustc_version(),
                features: bi.features,
                profile: bi.profile,
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                family: std::env::consts::FAMILY.into(),
            },
            python: PythonBlock {
                engine_version_expected: bi.version,
                package_version: None,
                note: "python 侧由 scripts/oriera_gym.py 的 check_build() 以本块 engine_version_expected 校验 pyd build_info 并告警（Rust 侧不运行 python）".into(),
            },
            command_line: std::env::args().collect(),
            protocol: ProtocolBlock {
                id: extra.protocol_id.clone(),
                parameter_hash: extra.protocol_hash.clone(),
            },
            method: extra.method.clone(),
            seed: extra.seed,
            spec: serde_json::to_value(spec).expect("SceneSpec serializes"),
            spec_hash: spec.spec_hash_hex(),
        }
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("RunManifest serializes")
    }
}

/// Write `<path>/run_manifest.json`-style manifest for one run.
/// `path` is the full manifest file path (parent dirs are created).
pub fn write_run_manifest(
    path: impl AsRef<Path>,
    spec: &SceneSpec,
    extra: &ManifestExtra,
) -> std::io::Result<()> {
    write_run_manifest_opts(path, spec, extra, false)
}

/// 脱敏变体（P-B3 挂起项落地，M3）：`sanitize=true` 时 manifest 不带
/// resolved spec 全文（商业机型参数保密场景），只留 `spec_hash`——
/// 仍可对账身份，但**不再声称逐位可复现**（复现需要完整 spec）。
pub fn write_run_manifest_opts(
    path: impl AsRef<Path>,
    spec: &SceneSpec,
    extra: &ManifestExtra,
    sanitize: bool,
) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut m = RunManifest::new(spec, extra);
    if sanitize {
        m.spec = serde_json::json!({
            "redacted": true,
            "reason": "sanitized export: proprietary parameters withheld; identity is verifiable via spec_hash, but byte-exact reproduction is NOT claimed without the full spec",
        });
    }
    std::fs::write(path, m.to_json_pretty())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::SceneSpec;

    fn preset() -> SceneSpec {
        SceneSpec::preset(crate::embodied_env::TaskKind::PickPlace)
    }

    #[test]
    fn build_info_reflects_the_compile_config() {
        let bi = build_info();
        assert_eq!(bi.version, env!("CARGO_PKG_VERSION"));
        assert!(["debug", "release"].contains(&bi.profile.as_str()));
        // this test leg always runs under the spec combo
        assert!(bi.features.contains(&"spec".to_string()));
        assert_eq!(features_string(), bi.features.join("+"));
    }

    #[test]
    fn manifest_is_complete_and_self_describing() {
        let spec = preset();
        let extra = ManifestExtra {
            protocol_id: "course@v1".into(),
            protocol_hash: "feed".into(),
            method: "scripted".into(),
            seed: Some(1000),
        };
        let m = RunManifest::new(&spec, &extra);
        assert_eq!(m.schema, RUN_MANIFEST_SCHEMA);
        assert_eq!(m.spec_hash, spec.spec_hash_hex());
        assert_eq!(m.method, "scripted");
        assert_eq!(m.seed, Some(1000));
        // resolved spec rides verbatim
        let on_file = serde_json::to_value(&spec).unwrap();
        assert_eq!(m.spec, on_file);
        // provenance blocks are populated (or honestly "unknown" + note)
        assert!(!m.engine.version.is_empty());
        assert!(["unknown"].contains(&m.engine.git_sha.as_str()) || m.engine.git_sha.len() >= 7);
        assert!(!m.engine.git_sha.is_empty());
        if m.engine.git_sha == "unknown" {
            assert!(!m.engine.git_note.is_empty(), "unknown needs a reason");
        }
        assert!(m.engine.rapier_version != "unknown" || !m.engine.cargo_lock_path.is_empty());
        assert!(!m.build.rustc.is_empty());
        assert_eq!(m.build.os, std::env::consts::OS);
        // protocol + command line land
        assert_eq!(m.protocol.id, "course@v1");
        assert!(!m.command_line.is_empty());
        // python block tells the pyd-check story
        assert_eq!(m.python.engine_version_expected, env!("CARGO_PKG_VERSION"));
        assert!(!m.python.note.is_empty());
    }

    #[test]
    fn manifest_writes_and_serializes_cleanly() {
        let dir = std::env::temp_dir().join(format!("oriera-manifest-{}", std::process::id()));
        let path = dir.join("runs").join("abcd").join("run_manifest.json");
        let spec = preset();
        write_run_manifest(&path, &spec, &ManifestExtra::default()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(v["schema"], RUN_MANIFEST_SCHEMA);
        assert_eq!(v["spec_hash"], spec.spec_hash_hex());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lockfile_hash_is_stable() {
        let (_, h1, _) = cargo_lock_provenance();
        let (_, h2, _) = cargo_lock_provenance();
        assert_eq!(h1, h2);
        if let Some(h) = h1 {
            assert_eq!(h.len(), 16, "{h}");
        }
    }
}
