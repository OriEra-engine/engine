//! Embodied 3D collection (PLAN-embodied-sim) — the lab batch runner.
//!
//! Drives [`TableTopEnv`] (L9 environment contract × L6 `rigid3d`
//! multibody backend) with analytic-IK scripted baselines across the
//! task family, records every step into a **LeRobotDataset v2.1** tree
//! (JSONL rows + camera PNG frames + full meta; `--lerobot-v3` adds the
//! v3.0 tree) and pins the reproducibility claim: with
//! `--verify-determinism=full` (the default) the whole collection runs
//! TWICE and both runs must be bit-identical — dataset, rewards and the
//! per-episode `sim_fingerprint` receipts (#66 + #67 at sim
//! granularity).
//!
//! **M0a (PLAN-embodied-general)**: the experiment is a spec. `--spec`
//! loads a `oriera.scene/2` JSON (v1 drafts auto-migrate, `extends`
//! overlays resolve against `specs/official/`), `--set a.b=v` overrides
//! fields (they enter the hash), and the legacy `--task` path builds
//! the exact same spec through `SceneSpec::preset` — both paths run the
//! same code and the receipt carries `spec_hash`. The scripted IK
//! baseline reads the arm geometry from the RobotSpec (4-DoF preset
//! only — honest error otherwise).
//!
//! **M0b (PLAN §四 M0b / §9.1 P-1/P-3/P-5/P-10)**:
//! - versioned protocols: `--protocol course@v1` (default = the
//!   classroom contract, 12 episodes + 90% hard gate) or
//!   `paper@v1` (≥100 episodes, record-only) — one `evaluate()` entry,
//!   protocol id + parameter hash in every report;
//! - provenance: a `run_manifest.json` (resolved spec, spec_hash, git
//!   SHA+dirty, Cargo.lock hash, rustc/rapier, features, OS/arch,
//!   command line) lands next to every dataset;
//! - batch flags: `--seed N` (overrides seed0), `--gate=hard|report`,
//!   `--verify-determinism=full|sample|off`, `--runs-root runs/` →
//!   `runs/<hash8>/<method-p<proto8>>/<seed>/` layout with
//!   skip-if-done resume, `--fingerprints FILE` → the scheme-v2 golden
//!   fingerprint file (`contracts/golden/embodied_fingerprints.v2.json`
//!   is generated with exactly this flag);
//! - 脱敏导出（P-B3）：`--sanitize` 让 manifest 不带 resolved spec 全文，
//!   只留 spec_hash；oriera_spec.json 不落盘。对外交换数据用。
//!
//! - `eval_report.json` carries the three fingerprint views
//!   (`episode_state_hashes` pure-state, `_v1` legacy, `_v2`
//!   identity-complete) plus the Wilson interval and gate verdict.
//!
//! An `eval_report.json` lands next to the dataset: per-episode
//! outcome plus the success rate with a Wilson 95% interval — the
//! reporting format lab experiments are held to.
//!
//! **P1-A1 (PLAN-embodied-v3 §二 #11) — episode 级断点续跑**：run 级
//! skip 名不副实（paper@v1 100 集崩在第 99 集 = 40 分钟归零）。每完成
//! 一集即把该集行写到**最终 v2.1 布局**（`lerobot-v21/data/chunk-000/
//! episode_XXXXXX.jsonl`——converter 本来就吃这个文件——加相机 PNG 帧
//! 目录与 `meta/episodes.jsonl` 追加行），并把抽签日志（seed / 成功 /
//! steps / 指纹 / 是否退化重抽）原子提交到 `lerobot-v21/progress.json`。
//! 同一 `--out`/`--runs-root` 目录中途崩溃后重跑同一条命令：日志里已有
//! 的抽签按记录重放——保留集直接从盘上读回（不重采），退化重抽按日志
//! 还原 65536 位移种子带——只补缺的集。结束时 `write_dir` 照旧全量重写
//! meta（info/stats/tasks/episodes/MANIFEST）+ receipt，converter 不用
//! 改；resume 跑与连续跑的最终数据树逐位一致。`--force` 清空 checkpoint
//! 全量重采；`--runs-root` 的「完成即跳过」（eval_report.json 在 = 跳）
//! 语义不变，两种布局都支持续跑。
//!
//! **M2 (PLAN §9.1 U-8)**: `--export npz` additionally writes the same
//! rows as bare numpy archives (`<out>/npz/episode_{i:06}.npz`:
//! `obs_{i}.npy` / `act_{i}.npy` float32 + `meta.json`; hand-written
//! stored ZIP, zero new crates) next to — not instead of — the LeRobot
//! trees. Without `--export` nothing changes. There is no
//! `--export parquet`: parquet stays on the pyarrow converter
//! (`scripts/jsonl_to_lerobot_v21.py`), Rust adds no parquet crate.
//!
//! Run:
//! `cargo run -p oriera-agent --example embodied_sim_collect --features rigid3d,cam,spec,urdf -- --task pick_place --episodes 12 --cam`
//! `… -- --spec crates/oriera-agent/specs/tabletop-4dof-pick.json --episodes 12`
//! Args: `--task reach|pick_place|push` · `--spec FILE` · `--set a.b=v`
//!       `--episodes N` · `--seed0 S` / `--seed N` · `--cam` / `--no-cam`
//!       `--cameras-from-spec` (M3-A: render spec.scene.cameras —
//!       multi RGB/depth video tracks — instead of the top view)
//!       `--out DIR` · `--runs-root DIR` · `--lerobot-v3` · `--export npz`
//!       `--migrate FILE` · `--validate FILE` · `--urdf FILE` · `--protocol ID`
//!       `--gate hard|report` · `--verify-determinism full|sample|off`
//!       `--fingerprints FILE` · `--force`
//! Output: `<out>/lerobot-v21/` [+ `<out>/lerobot-v30/`] [+ `<out>/npz/`] +
//!         `<out>/eval_report.json` + `<out>/run_manifest.json`

use oriera_agent::embodied_env::{CameraFrame, EmbodiedEnv, ObsLayout, TableTopEnv, TaskKind};
use oriera_agent::eval::{self, EpisodeOutcome, EpisodeRow, EvalProtocol, EvalResult, Gate};
use oriera_agent::lerobot::LeRobotV21;
use oriera_agent::manifest::{write_run_manifest_opts, ManifestExtra};
use oriera_agent::spec::SceneSpec;
use oriera_subsys::rigid3d::robot_identity_hash;
use oriera_subsys::RobotSpec;
use std::io::Write as _;

/// One phase of a scripted baseline: an IK waypoint lifted from the
/// observation, a gripper demand, and a dwell long enough for the PD
/// motors to converge.
struct Phase {
    gripper: f32,
    ticks: u64,
    target: fn(obj: [f32; 3], goal: [f32; 3]) -> [f32; 3],
}

fn above(p: [f32; 3], h: f32) -> [f32; 3] {
    [p[0], p[1] + h, p[2]]
}

/// A point `d` from `p` along the horizontal unit direction `dir`
/// (the behind-the-cube waypoint for the push baseline).
fn toward(p: [f32; 3], dir: [f32; 3], d: f32) -> [f32; 3] {
    let n = (dir[0] * dir[0] + dir[2] * dir[2]).sqrt().max(1e-6);
    [p[0] + dir[0] / n * d, p[1], p[2] + dir[2] / n * d]
}

fn pick_place_phases() -> Vec<Phase> {
    vec![
        Phase {
            gripper: 0.0,
            ticks: 70,
            target: |o, _g| above(o, 0.12),
        },
        Phase {
            gripper: 0.0,
            ticks: 60,
            target: |o, _g| above(o, 0.030),
        },
        Phase {
            gripper: 1.0,
            ticks: 40,
            target: |o, _g| above(o, 0.030),
        },
        Phase {
            gripper: 1.0,
            ticks: 70,
            target: |o, _g| above(o, 0.20),
        },
        Phase {
            gripper: 1.0,
            ticks: 95,
            target: |_o, g| above(g, 0.20),
        },
        Phase {
            gripper: 1.0,
            ticks: 70,
            target: |_o, g| above(g, 0.06),
        },
        Phase {
            gripper: 0.0,
            ticks: 35,
            target: |_o, g| above(g, 0.06),
        },
        Phase {
            gripper: 0.0,
            ticks: 45,
            target: |_o, g| above(g, 0.22),
        },
    ]
}

fn reach_phases() -> Vec<Phase> {
    vec![
        Phase {
            gripper: 0.0,
            ticks: 70,
            target: |o, _g| above(o, 0.12),
        },
        Phase {
            gripper: 0.0,
            ticks: 70,
            target: |o, _g| above(o, 0.030),
        },
        Phase {
            gripper: 0.0,
            ticks: 400,
            target: |o, _g| above(o, 0.030),
        },
    ]
}

fn push_phases() -> Vec<Phase> {
    // Contact push: approach a point 9cm behind the cube (relative to
    // the goal), then drive the tip THROUGH the goal at cube-centre
    // height, nudging the cube along. Gripper stays open — the honest
    // non-prehensile baseline (drag-by-attachment stays legal in the
    // env but doesn't count as pushing).
    vec![
        Phase {
            gripper: 0.0,
            ticks: 70,
            target: |o, g| above(toward(o, [o[0] - g[0], 0.0, o[2] - g[2]], 0.09), 0.12),
        },
        Phase {
            gripper: 0.0,
            ticks: 60,
            target: |o, g| above(toward(o, [o[0] - g[0], 0.0, o[2] - g[2]], 0.09), 0.035),
        },
        Phase {
            gripper: 0.0,
            ticks: 180,
            target: |_o, g| above(g, 0.035),
        },
        Phase {
            gripper: 0.0,
            ticks: 60,
            target: |_o, g| above(g, 0.20),
        },
    ]
}

/// The arm geometry the analytic IK needs, read off the RobotSpec
/// (M0a: the spec is the authority — no `ArmDescription` side channel).
/// Every float is the exact value the preset projection stored, so the
/// legacy `ik(&ArmDescription)` numbers are reproduced bit for bit.
struct IkArm {
    base: [f32; 3],
    shoulder_y: f32,
    /// shoulder→elbow, elbow→wrist, wrist→tip.
    links: [f32; 3],
    limits: [(f32, f32); 4],
}

impl IkArm {
    fn from_robot(r: &RobotSpec) -> Result<Self, String> {
        if r.dof() != 4 {
            return Err(format!(
                "脚本 IK 基线只认 4-DoF 桌面臂预设（yaw + 2R + wrist pitch），当前 robot 有 {} 个关节 —— \
                 其他构型的基线策略是 M2（DLS 数值 IK）",
                r.dof()
            ));
        }
        let g = r
            .gripper
            .as_ref()
            .ok_or("脚本 IK 基线需要 gripper（tcp_offset 定义指尖）")?;
        let shoulder_link = r.joints[1].child_link;
        Ok(Self {
            base: r.base,
            shoulder_y: r.links[shoulder_link].origin[1],
            links: [
                r.joints[2].anchor[0],
                r.joints[3].anchor[0],
                g.tcp_offset[0],
            ],
            limits: [
                r.joints[0].limits,
                r.joints[1].limits,
                r.joints[2].limits,
                r.joints[3].limits,
            ],
        })
    }
}

/// Analytic IK for the OriEra arm: base yaw + planar 2R (shoulder,
/// elbow) + wrist pitch aiming the tip straight down. Returns clamped
/// joint targets — the policy speaks target angles, the L6 PD motors
/// do the tracking.
fn ik(arm: &IkArm, target: [f32; 3]) -> [f32; 4] {
    let (l1, l2) = (arm.links[0], arm.links[1]);
    let shoulder_y = arm.shoulder_y;
    let dx = target[0] - arm.base[0];
    let dz = target[2] - arm.base[2];
    let yaw = f32::atan2(-dz, dx);
    let d = (dx * dx + dz * dz).sqrt().clamp(0.06, l1 + l2 - 0.005);
    // The 2R solves the WRIST position; the tip hangs l3 below it, so
    // aim the wrist l3 above the desired tip point.
    let h = (target[1] + arm.links[2] - shoulder_y).clamp(-0.38, 0.30);
    let cos_e = ((d * d + h * h - l1 * l1 - l2 * l2) / (2.0 * l1 * l2)).clamp(-1.0, 1.0);
    let elbow = -cos_e.acos();
    let shoulder = f32::atan2(h, d) - f32::atan2(l2 * elbow.sin(), l1 + l2 * elbow.cos());
    let wrist = -std::f32::consts::FRAC_PI_2 - shoulder - elbow;
    let clamp = |v: f32, i: usize| v.clamp(arm.limits[i].0, arm.limits[i].1);
    [
        clamp(yaw, 0),
        clamp(shoulder, 1),
        clamp(elbow, 2),
        clamp(wrist, 3),
    ]
}

/// Where the policy reads its waypoints in the observation vector —
/// looked up by name in the [`ObsLayout`], not hardcoded indices.
struct ObsIdx {
    ee: usize,
    obj_rel: usize,
    goal_rel: usize,
}

impl ObsIdx {
    fn from_layout(layout: &ObsLayout) -> Self {
        let find = |n: &str| {
            layout.index_of(n).unwrap_or_else(|| {
                fail(&format!(
                    "obs layout 缺少 {n}（脚本基线需要 ee/obj_rel/goal_rel）——\
                     当前 spec 的任务/机器人不提供这些观测维度，脚本基线跑不了它"
                ))
            })
        };
        Self {
            ee: find("ee.x"),
            obj_rel: find("obj_rel.x"),
            goal_rel: find("goal_rel.x"),
        }
    }
}

/// The scripted baseline policy: pure function of the observation —
/// the same contract a trained policy would consume.
fn policy(obs: &[f32], idx: &ObsIdx, phases: &[Phase], step: u64, arm: &IkArm) -> [f32; 5] {
    let ee = [obs[idx.ee], obs[idx.ee + 1], obs[idx.ee + 2]];
    let o = idx.obj_rel;
    let g = idx.goal_rel;
    let obj = [ee[0] + obs[o], ee[1] + obs[o + 1], ee[2] + obs[o + 2]];
    let goal = [ee[0] + obs[g], ee[1] + obs[g + 1], ee[2] + obs[g + 2]];
    let mut ticks = 0u64;
    for phase in phases {
        ticks += phase.ticks;
        if step < ticks {
            let mut a = ik(arm, (phase.target)(obj, goal)).to_vec();
            a.push(phase.gripper);
            return a.try_into().expect("5 action dims");
        }
    }
    // Past the script: hover above the goal, gripper open.
    let mut a = ik(arm, above(goal, 0.22)).to_vec();
    a.push(0.0);
    a.try_into().expect("5 action dims")
}

/// `--verify-determinism` modes: `full` = the whole collection runs
/// twice and must be bit-identical (the pre-M0b behaviour, default);
/// `sample` = single full pass + a bit-check re-run of the first kept
/// episode; `off` = single pass, trust the seed (batch sweeps use this).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verify {
    Full,
    Sample,
    Off,
}

struct Args {
    /// `--task` / `--phrase` (None ⇒ the spec's task, or pick_place).
    task: Option<TaskKind>,
    /// `--spec FILE` (None ⇒ the preset spec of `task`).
    spec: Option<String>,
    /// `--set a.b=v` overrides (applied after load, enter the hash).
    sets: Vec<String>,
    /// `--episodes N` (None ⇒ the protocol's default: course 12 / paper 100).
    episodes: Option<u32>,
    /// `--seed0 S` / `--seed N` (both set seed0; `--seed` wins).
    seed0: u64,
    cam: bool,
    out: String,
    /// Also write the LeRobot v3.0 tree (`<out>/lerobot-v30/`).
    lerobot_v3: bool,
    /// `--export npz`: also write the bare numpy archives (`<out>/npz/`).
    export_npz: bool,
    /// U1: URDF→SceneSpec 导出路径（设置即只做导入不采集）。
    urdf: Option<String>,
    /// `--migrate FILE`: v1 → v2 spec 落盘（不采集）。
    migrate: Option<String>,
    /// `--validate FILE`: 只校验 + 打印 spec_hash（不采集）。
    validate: Option<String>,
    /// `--protocol course@v1|paper@v1` (default course@v1).
    protocol: Option<String>,
    /// `--gate hard|report` (default: the protocol's own gate).
    gate: Option<Gate>,
    /// `--verify-determinism full|sample|off` (default full).
    verify: Verify,
    /// `--runs-root runs/`: write into `runs/<hash8>/<method>/<seed>/`
    /// with skip-if-done resume instead of direct `--out` writes.
    runs_root: Option<String>,
    /// `--fingerprints FILE`: write the scheme-v2 golden fingerprint
    /// file (the `contracts/golden/embodied_fingerprints.v2.json`
    /// generator flag).
    fingerprints: Option<String>,
    /// `--cameras-from-spec`: render the spec's `scene.cameras`
    /// (M3-A pinhole pipeline, multi RGB/depth tracks) instead of the
    /// legacy top view. Needs `--features cam`.
    cameras_from_spec: bool,
    /// `--sanitize`（P-B3 脱敏导出）：manifest 不带 resolved spec 全文，
    /// 只留 spec_hash；oriera_spec.json 不落盘。
    sanitize: bool,
    /// `--force`: re-run even when the runs-layout directory is done.
    force: bool,
    /// `--out` 是否显式给出（`--runs-root` 同给时打忽略警告——SRE 遗留）。
    out_given: bool,
}

const USAGE: &str = "--task/--phrase/--spec/--set/--episodes/--seed0/--seed/--cam/--no-cam/--cameras-from-spec/--out/--runs-root/--lerobot-v3/--export/--migrate/--validate/--urdf/--protocol/--gate/--verify-determinism/--fingerprints/--sanitize/--force/--help（--help 看分节说明）";

/// `--export FMT[,FMT]` → whether npz was requested. `parquet` is
/// refused with the converter route (no parquet crate in Rust — the
/// dependency discipline); anything else names the legal values.
fn parse_export(v: &str) -> bool {
    let mut npz = false;
    for fmt in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match fmt {
            "npz" => npz = true,
            "parquet" => fail(
                "--export parquet：引擎不直接写 parquet（Rust 侧没有零依赖的 parquet 写器，不为此引新 crate）——\
                 请走现有转换器：python scripts/jsonl_to_lerobot_v21.py <out>/lerobot-v21/（pyarrow 落 parquet）",
            ),
            other => fail(&format!("--export {other:?}：可选值只有 npz（numpy 直读的裸数组）")),
        }
    }
    if !npz {
        fail("--export 需要一个格式：npz");
    }
    npz
}

/// `--help` / `-h`: the full flag reference (USAGE above is the
/// one-line arg list for the unknown-arg error).
fn print_help() -> ! {
    println!(
        "OriEra rigid3d × LeRobot v2.1 批量采集（embodied_sim_collect）

用法
  cargo run -p oriera-agent --example embodied_sim_collect --features rigid3d,cam,spec,urdf -- [参数]

实验定义
  --task reach|pick_place|push    任务预设（默认 pick_place）
  --phrase \"...\"                  自然语言短语 → 任务（最小 NLU 桥）
  --spec FILE                     oriera.scene/2 JSON（v1 自动迁移；extends 对 specs/official/ 解析）
  --set a.b=v                     覆盖 spec 字段（进入 spec_hash）
  --urdf FILE                     URDF → SceneSpec 落盘（不采集）
  --migrate FILE / --validate FILE  spec v1→v2 落盘 / 只校验并打印 hash

协议与预算
  --protocol course@v1|paper@v1   course@v1 = 12 集 + 90% 硬闸（默认）；paper@v1 = ≥100 集只记录
  --episodes N                    集数覆盖（默认随协议）
  --seed0 S / --seed N            首集种子（--seed 优先；默认 1000）
  --gate hard|report              覆盖协议闸（paper@v1 拒绝 hard）

采集与输出
  --out DIR                       输出目录（默认 out/embodied-sim）
  --runs-root DIR                 改写 runs/<spec8>/<method-p<proto8>>/<seed>/ 布局（完成即跳过）；
                                  与 --out 同给时 --out 被忽略（会打警告）
  --cam / --no-cam                相机轨开关（默认 off——正交软光栅约吃一半吞吐）
  --cameras-from-spec             渲染 spec.scene.cameras（多 RGB/depth 轨；需 --features cam）
  --lerobot-v3                    额外写 LeRobot v3.0 树（<out>/lerobot-v30/）
  --export npz                    额外写 numpy 裸数组（<out>/npz/；无 parquet 直写：
                                  python scripts/jsonl_to_lerobot_v21.py <out>/lerobot-v21/）
  --sanitize                      脱敏导出（manifest 不带 spec 全文，只留 spec_hash）

确定性与金标
  --verify-determinism full|sample|off
                                  full = 双跑逐位一致（默认）；sample = 首集复核；off = 单跑信任种子
  --fingerprints FILE             写 scheme-v2 金标指纹文件

断点续跑（P1-A1）
  每完成一集即落盘 checkpoint：episode JSONL + 相机帧（最终 v2.1 布局）+ progress.json
  抽签日志。同一输出目录中途崩溃/kill 后，重跑同一条命令即续采——已记录的抽签按日志
  重放（保留集读回不重采，退化重抽按日志还原种子带），只补缺的集。结束时照旧写出全部
  meta 与 receipt（converter 不用改，resume 跑与连续跑逐位一致）。
  --force                         清空 checkpoint 与派生导出，全量重采
  （--runs-root 布局的「完成即跳过」：eval_report.json 已在则整跑跳过，语义不变）

其他
  --help / -h                     打印本说明

输出
  <out>/lerobot-v21/  [+ <out>/lerobot-v30/] [+ <out>/npz/] + <out>/eval_report.json + run_manifest.json"
    );
    std::process::exit(0)
}

fn parse_args() -> Args {
    let mut a = Args {
        task: None,
        spec: None,
        sets: Vec::new(),
        episodes: None,
        seed0: 1000,
        cam: false,
        out: "out/embodied-sim".into(),
        lerobot_v3: false,
        export_npz: false,
        urdf: None,
        migrate: None,
        validate: None,
        protocol: None,
        gate: None,
        verify: Verify::Full,
        runs_root: None,
        fingerprints: None,
        cameras_from_spec: false,
        sanitize: false,
        force: false,
        out_given: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => print_help(),
            "--task" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| fail(&format!("{arg} 需要一个值：reach|pick_place|push")));
                a.task =
                    Some(TaskKind::parse(&v).unwrap_or_else(|| {
                        fail(&format!("--task {v:?}：须为 reach|pick_place|push"))
                    }));
            }
            "--spec" => {
                a.spec = Some(
                    it.next()
                        .unwrap_or_else(|| fail(&format!("{arg} 需要一个文件路径"))),
                )
            }
            "--set" => a.sets.push(
                it.next()
                    .unwrap_or_else(|| fail(&format!("{arg} 需要 a.b.c=v"))),
            ),
            "--urdf" => {
                a.urdf = Some(
                    it.next()
                        .unwrap_or_else(|| fail(&format!("{arg} 需要一个文件路径"))),
                )
            }
            "--migrate" => {
                a.migrate = Some(
                    it.next()
                        .unwrap_or_else(|| fail(&format!("{arg} 需要一个文件路径"))),
                )
            }
            "--validate" => {
                a.validate = Some(
                    it.next()
                        .unwrap_or_else(|| fail(&format!("{arg} 需要一个文件路径"))),
                )
            }
            "--phrase" => {
                // 语言→具身最小桥：自然语言短语解析出任务（NLU→seed 组合
                // 由调用方继续），例如 --phrase "pick up the cube"。
                let v = it
                    .next()
                    .unwrap_or_else(|| fail(&format!("{arg} 需要一个值")));
                a.task = Some(TaskKind::from_phrase(&v).unwrap_or_else(|| {
                    fail(&format!(
                        "无法从短语解析任务：{v:?}（试试 --task reach|pick_place|push）"
                    ))
                }));
            }
            "--episodes" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| fail(&format!("{arg} 需要一个正整数")));
                a.episodes = Some(v.parse().unwrap_or_else(|_| {
                    fail(&format!("--episodes {v:?} 不是正整数（例：--episodes 12）"))
                }));
            }
            "--seed0" | "--seed" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| fail(&format!("{arg} 需要一个值")));
                a.seed0 = v
                    .parse()
                    .unwrap_or_else(|_| fail(&format!("{arg} {v:?} 不是非负整数")));
            }
            "--protocol" => {
                // 校验在 main（paper@v1 的集数下限要等 --episodes 到齐）
                a.protocol = Some(
                    it.next()
                        .unwrap_or_else(|| fail(&format!("{arg} 需要一个值：course@v1|paper@v1"))),
                );
            }
            "--gate" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| fail(&format!("{arg} 需要一个值")));
                a.gate = Some(match v.as_str() {
                    "hard" => Gate::Hard90,
                    "report" => Gate::Report,
                    other => fail(&format!(
                        "--gate {other:?}：须为 hard（90% 硬闸）或 report（只记录）"
                    )),
                });
            }
            "--verify-determinism" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| fail(&format!("{arg} 需要一个值")));
                a.verify = match v.as_str() {
                    "full" => Verify::Full,
                    "sample" => Verify::Sample,
                    "off" => Verify::Off,
                    other => fail(&format!(
                        "--verify-determinism {other:?}：须为 full（双跑逐位，默认）| sample（抽一集复核）| off（单跑）"
                    )),
                };
            }
            "--runs-root" => {
                a.runs_root = Some(
                    it.next()
                        .unwrap_or_else(|| fail(&format!("{arg} 需要一个目录"))),
                )
            }
            "--fingerprints" => {
                a.fingerprints = Some(
                    it.next()
                        .unwrap_or_else(|| fail(&format!("{arg} 需要一个文件路径"))),
                )
            }
            "--sanitize" => a.sanitize = true,
            "--force" => a.force = true,
            "--out" => {
                a.out = it
                    .next()
                    .unwrap_or_else(|| fail(&format!("{arg} 需要一个值")));
                a.out_given = true;
            }
            // P0-8：默认翻 off——正交软光栅吃掉 native 52% 吞吐（性能
            // 审查实测 2121→1027），评测入口不需要渲染；要图轨显式 --cam。
            "--cam" => a.cam = true,
            "--no-cam" => a.cam = false,
            "--cameras-from-spec" => a.cameras_from_spec = true,
            "--lerobot-v3" => a.lerobot_v3 = true,
            "--export" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| fail("--export 需要一个格式：npz"));
                a.export_npz |= parse_export(&v);
            }
            other => fail(&format!("未知参数 {other}（可用 {USAGE}）")),
        }
    }
    a
}

/// Resolve the experiment spec: `--spec` (v2, or v1 auto-migrated,
/// `extends` overlays resolved against `specs/official/`) + `--set`
/// overrides, else the preset of `--task` (default pick_place). Both
/// roads yield a validated [`SceneSpec`] — one code path.
fn resolve_spec(args: &Args) -> SceneSpec {
    let mut spec = match &args.spec {
        Some(path) => {
            let loaded = SceneSpec::load_file(std::path::Path::new(path))
                .unwrap_or_else(|e| fail(&format!("spec 加载失败：{e}")));
            if loaded.migrated {
                println!("  · {path} 是 v1 草案，已自动迁移为 oriera.scene/2（落盘用 --migrate）");
            }
            let mut s = loaded.spec;
            if let Some(t) = args.task {
                s.task.id = t.id().into();
            }
            s
        }
        None => SceneSpec::preset(args.task.unwrap_or(TaskKind::PickPlace)),
    };
    spec.apply_sets(&args.sets)
        .unwrap_or_else(|e| fail(&format!("--set 失败：{e}")));
    spec.validate()
        .unwrap_or_else(|e| fail(&format!("spec 校验失败：{e}")));
    spec
}

/// Human-readable fatal error (no backtrace noise for students).
fn fail(msg: &str) -> ! {
    eprintln!("错误：{msg}");
    std::process::exit(1)
}

struct RunResult {
    ds: LeRobotV21,
    /// (seed, success, steps, reward_sum) per episode.
    rows: Vec<(u64, bool, u64, f32)>,
    hashes: Vec<u64>,
    /// Per-episode randomization receipts (draws differ per episode).
    rand_receipts: Vec<String>,
    /// Total env steps taken (the throughput denominator).
    total_steps: u64,
    /// Total draws and degenerate re-draws (from `evaluate`).
    draws: u64,
    redraws: u64,
}

// ===================== episode 级 checkpoint/resume（P1-A1） =====================

/// progress.json 的格式代号（身份字段不兼容时人话拒绝续跑，不静默混采）。
const PROGRESS_FORMAT: &str = "oriera-progress/1";

/// 抽签日志的一条记录：本次抽签抽到的 seed 与结果。保留集带完整
/// [`EpisodeRow`]；退化重抽只记事实——正是这条事实钉住 65536 位移
/// 种子带，按日志重放即可逐位还原后续 seed 序列。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DrawRecord {
    draw: u64,
    seed: u64,
    degenerate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    row: Option<EpisodeRow>,
}

/// `progress.json`：一次采集的全部抽签日志 + 实验身份（spec/协议/种子/
/// 集数）。身份字段不匹配的续跑被人话拒绝——不同实验的集不能混。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Progress {
    format: String,
    spec_hash: String,
    protocol_id: String,
    protocol_parameter_hash: String,
    seed0: u64,
    episodes: u32,
    task: String,
    engine_commit: String,
    draws: Vec<DrawRecord>,
}

/// 一次采集 pass 的 checkpoint sink。`None` = 纯内存（`--verify-
/// determinism=full` 的第二跑必须真跑物理复核，不能对着日志自我印证）。
struct CollectIo<'a> {
    /// `<out>/lerobot-v21`——checkpoint 树根（与最终 v2.1 布局同路径）。
    root: &'a std::path::Path,
    /// 抽签日志（resume 时从 progress.json 播种，否则从空开始）。
    draws: Vec<DrawRecord>,
    /// 身份快照：每集写 progress 时用（spec_hash / git SHA 一次算好，
    /// 不逐集重算——git_provenance 会 shell out git）。
    spec_hash: String,
    engine_commit: String,
}

/// 读取并校验 progress.json 对当前实验身份。任何不匹配都是人话致命错：
/// 不同 spec/协议的 checkpoint 混进本跑会产出四不像的数据集。
fn load_progress(
    path: &std::path::Path,
    spec: &SceneSpec,
    protocol: &EvalProtocol,
) -> Result<Progress, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "progress.json 读取失败（{}）：{e}——删掉该文件或用 --force 全量重跑",
            path.display()
        )
    })?;
    let p: Progress = serde_json::from_str(&text)
        .map_err(|e| format!("progress.json 解析失败：{e}——文件损坏，删掉或用 --force 全量重跑"))?;
    if p.format != PROGRESS_FORMAT {
        return Err(format!(
            "progress.json 格式是 {:?}，本引擎只认 {PROGRESS_FORMAT:?}——不兼容的旧 checkpoint，用 --force 全量重跑",
            p.format
        ));
    }
    let here_spec = spec.spec_hash_hex();
    if p.spec_hash != here_spec {
        return Err(format!(
            "progress.json 属于 spec_hash {}，本跑是 {here_spec}——spec 变了不能混采，用 --force 全量重跑",
            p.spec_hash
        ));
    }
    if p.protocol_id != protocol.id || p.protocol_parameter_hash != protocol.protocol_hash_hex() {
        return Err(format!(
            "progress.json 属于协议 {}（参数 hash {}），本跑是 {}（{}）——协议变了不能混采，用 --force 全量重跑",
            p.protocol_id,
            p.protocol_parameter_hash,
            protocol.id,
            protocol.protocol_hash_hex()
        ));
    }
    if p.seed0 != protocol.seed0 {
        return Err(format!(
            "progress.json 的 seed0 是 {}，本跑是 {}——种子带不同不能续，用 --force 全量重跑",
            p.seed0, protocol.seed0
        ));
    }
    if p.episodes != protocol.episodes {
        return Err(format!(
            "progress.json 的目标集数是 {}，本跑是 {}——集数不同不能续，用 --force 全量重跑",
            p.episodes, protocol.episodes
        ));
    }
    Ok(p)
}

/// 重写 progress.json（tmp+rename 原子提交——这是每集的 checkpoint
/// commit 点：数据文件先落，日志后提交，kill 掉在中间 = 该集不在日志、
/// 下次重采，绝不出现「日志说有、盘上没有」的半集）。
fn write_progress(io: &CollectIo, protocol: &EvalProtocol, task: &str) -> Result<(), String> {
    let p = Progress {
        format: PROGRESS_FORMAT.into(),
        spec_hash: io.spec_hash.clone(),
        protocol_id: protocol.id.clone(),
        protocol_parameter_hash: protocol.protocol_hash_hex(),
        seed0: protocol.seed0,
        episodes: protocol.episodes,
        task: task.into(),
        engine_commit: io.engine_commit.clone(),
        draws: io.draws.clone(),
    };
    let json =
        serde_json::to_string_pretty(&p).map_err(|e| format!("progress.json 序列化失败：{e}"))?;
    std::fs::create_dir_all(io.root)
        .map_err(|e| format!("checkpoint 目录创建失败（{}）：{e}", io.root.display()))?;
    let tmp = io.root.join("progress.json.tmp");
    std::fs::write(&tmp, format!("{json}\n"))
        .map_err(|e| format!("progress.json 写入失败（{}）：{e}", tmp.display()))?;
    std::fs::rename(&tmp, io.root.join("progress.json"))
        .map_err(|e| format!("progress.json 提交失败：{e}"))?;
    Ok(())
}

/// 把刚完成的一集（`ds.episodes` 的最后一集）落到 checkpoint 树的
/// **最终 v2.1 路径**：逐集 JSONL（与 `LeRobotV21::write_dir` 的行格式
/// 逐字节相同——最短往返浮点）、相机 PNG 帧目录、`meta/episodes.jsonl`
/// 追加行。结束时 write_dir 会按内存里的同一份内容原样重写这些文件，
/// 所以中间态只多不少、最终逐位一致。
fn checkpoint_episode(root: &std::path::Path, ds: &LeRobotV21) -> Result<(), String> {
    let ep_index = ds.episodes.len() - 1;
    let ep = &ds.episodes[ep_index];
    let data_dir = root.join("data/chunk-000");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("checkpoint 数据目录创建失败（{}）：{e}", data_dir.display()))?;
    let global_start: u64 = ds.episodes[..ep_index]
        .iter()
        .map(|e| e.rows.len() as u64)
        .sum();
    let mut body = String::new();
    for (global, (ri, r)) in (global_start..).zip(ep.rows.iter().enumerate()) {
        // LeRobotV21::row_jsonl 的逐字节复刻（该方法是 crate 私有——
        // 行格式是 converter 契约的一部分，test 钉住两者一致）。
        body.push_str(&format!(
            concat!(
                "{{\"observation.state\":{:?},\"action\":{:?},",
                "\"timestamp\":{:.6},\"frame_index\":{},\"episode_index\":{},",
                "\"index\":{},\"task_index\":{}}}\n"
            ),
            r.state,
            r.action,
            ri as f32 / ds.fps,
            ri,
            ep_index,
            global,
            ep.task_index
        ));
    }
    std::fs::write(data_dir.join(format!("episode_{ep_index:06}.jsonl")), body).map_err(|e| {
        format!(
            "checkpoint episode_{ep_index:06}.jsonl 写入失败（{}）：{e}
常见原因：磁盘满 / 权限不足——清空间后重跑同一条命令即可续采",
            data_dir.display()
        )
    })?;
    // 相机帧按最终路径落盘（PNG 字节原样）。
    for cam in &ds.cameras {
        let track = ep.tracks.iter().find(|t| t.cam == cam.name);
        let (rgb, depth): (&[Vec<u8>], &[Vec<u8>]) = match track {
            Some(t) => (&t.rgb, &t.depth),
            None => (&[], &[]),
        };
        let write_frames = |prefix: &str, frames: &[Vec<u8>]| -> Result<(), String> {
            if frames.is_empty() {
                return Ok(());
            }
            let dir = root.join(format!("videos/chunk-000/{prefix}/episode_{ep_index:06}"));
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("checkpoint 相机目录创建失败（{}）：{e}", dir.display()))?;
            for (k, png) in frames.iter().enumerate() {
                std::fs::write(dir.join(format!("frame_{k:06}.png")), png).map_err(|e| {
                    format!(
                        "checkpoint {prefix}/episode_{ep_index:06}/frame_{k:06}.png 写入失败：{e}"
                    )
                })?;
            }
            Ok(())
        };
        write_frames(&format!("observation.images.{}", cam.name), rgb)?;
        write_frames(&format!("depth.{}", cam.name), depth)?;
    }
    // meta/episodes.jsonl 追加（converter 面向的集表；write_dir 最后
    // 会按全量重写规范版）。任务短语含引号/反斜杠则转义。
    let task = &ds.tasks[ep.task_index as usize];
    let esc = task.replace('\\', "\\\\").replace('"', "\\\"");
    let line = format!(
        "{{\"episode_index\":{ep_index},\"tasks\":[\"{esc}\"],\"length\":{}}}\n",
        ep.rows.len()
    );
    let meta_dir = root.join("meta");
    std::fs::create_dir_all(&meta_dir).map_err(|e| {
        format!(
            "checkpoint meta 目录创建失败（{}）：{e}",
            meta_dir.display()
        )
    })?;
    let episodes_path = meta_dir.join("episodes.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&episodes_path)
        .map_err(|e| format!("checkpoint {} 打开失败：{e}", episodes_path.display()))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("checkpoint {} 追加失败：{e}", episodes_path.display()))?;
    Ok(())
}

/// 续跑路径：把一集 checkpoint 从盘上读回 `ds`（行来自逐集 JSONL，
/// 相机帧来自 PNG 目录——字节原样，浮点经最短往返无损）。所有编号
/// 交叉校验必须成立；不一致 = 目录损坏或不是本跑的树，人话拒绝。
fn load_episode_into_ds(
    root: &std::path::Path,
    ds: &mut LeRobotV21,
    ep_index: usize,
    row: &EpisodeRow,
    task: &str,
) -> Result<(), String> {
    let path = root.join(format!("data/chunk-000/episode_{ep_index:06}.jsonl"));
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "断点续跑：读 checkpoint {} 失败：{e}
（若该目录已转换成 parquet——JSONL 移进了 raw_jsonl/——续跑数据源已不在，用 --force 全量重跑）",
            path.display()
        )
    })?;
    ds.start_episode(task);
    let task_index = ds.tasks.iter().position(|t| t == task).unwrap_or_default() as u32;
    let base = ds.total_frames() as u64;
    let mut n_rows = 0usize;
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("checkpoint {} 第 {i} 行不是合法 JSON：{e}", path.display()))?;
        let nums = |key: &str| -> Result<Vec<f32>, String> {
            let arr = v
                .get(key)
                .and_then(|x| x.as_array())
                .ok_or_else(|| format!("checkpoint {} 第 {i} 行缺 {key}", path.display()))?;
            arr.iter()
                .map(|x| {
                    x.as_f64().map(|f| f as f32).ok_or_else(|| {
                        format!("checkpoint {} 第 {i} 行 {key} 含非数字", path.display())
                    })
                })
                .collect()
        };
        let state = nums("observation.state")?;
        let action = nums("action")?;
        for (key, expect, what) in [
            ("frame_index", i as u64, "帧号"),
            ("episode_index", ep_index as u64, "集号"),
            ("index", base + i as u64, "全局行号"),
            ("task_index", task_index as u64, "任务号"),
        ] {
            let got = v
                .get(key)
                .and_then(|x| x.as_u64())
                .ok_or_else(|| format!("checkpoint {} 第 {i} 行缺 {key}", path.display()))?;
            if got != expect {
                return Err(format!(
                    "checkpoint {} 第 {i} 行 {what}（{key}）{got} ≠ 期望 {expect}——目录与本跑不一致，用 --force 全量重跑",
                    path.display()
                ));
            }
        }
        ds.push_step(&state, &action)
            .map_err(|e| format!("checkpoint {} 第 {i} 行读回失败：{e}", path.display()))?;
        n_rows += 1;
    }
    if n_rows != row.steps as usize {
        return Err(format!(
            "checkpoint {} 行数 {n_rows} ≠ progress 记录的 steps {}——目录与本跑不一致，用 --force 全量重跑",
            path.display(),
            row.steps
        ));
    }
    // 相机帧：PNG 字节原样往返。（先快照相机名——push_frame 要可变借
    // ds，不能在 &ds.cameras 的循环里做。深度轨目录只在有帧时存在，
    // 按名探测即可，无需声明。）
    let cams: Vec<String> = ds.cameras.iter().map(|c| c.name.clone()).collect();
    for name in &cams {
        let rgb = read_frame_dir(&root.join(format!(
            "videos/chunk-000/observation.images.{name}/episode_{ep_index:06}"
        )))?;
        if !rgb.is_empty() && rgb.len() != n_rows {
            return Err(format!(
                "checkpoint 相机 {name}（episode {ep_index}）帧数 {} ≠ 行数 {n_rows}——目录不完整，用 --force 全量重跑",
                rgb.len()
            ));
        }
        let depth = read_frame_dir(&root.join(format!(
            "videos/chunk-000/depth.{name}/episode_{ep_index:06}"
        )))?;
        if !depth.is_empty() && depth.len() != n_rows {
            return Err(format!(
                "checkpoint 深度轨 {name}（episode {ep_index}）帧数 {} ≠ 行数 {n_rows}——目录不完整，用 --force 全量重跑",
                depth.len()
            ));
        }
        if rgb.is_empty() && depth.is_empty() {
            continue;
        }
        for k in 0..n_rows {
            ds.push_frame(CameraFrame {
                cam: name.clone(),
                rgb_png: rgb.get(k).cloned(),
                depth_png: depth.get(k).cloned(),
            })
            .map_err(|e| format!("checkpoint 相机帧读回失败（episode {ep_index}）：{e}"))?;
        }
    }
    ds.finish_episode();
    Ok(())
}

/// 读 `frame_000000.png …` 帧目录（按名排序、编号必须连续）。目录不
/// 存在 = 该轨这一集没有帧（纯状态跑）——空 Vec 而非错误。
fn read_frame_dir(dir: &std::path::Path) -> Result<Vec<Vec<u8>>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(format!(
                "checkpoint 相机目录 {} 读取失败：{e}",
                dir.display()
            ))
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("frame_") && n.ends_with(".png"))
        .collect();
    names.sort();
    for (i, name) in names.iter().enumerate() {
        if *name != format!("frame_{i:06}.png") {
            return Err(format!(
                "checkpoint {} 帧序不连续（{name} ≠ frame_{i:06}.png）——目录不完整，用 --force 全量重跑",
                dir.display()
            ));
        }
    }
    names
        .iter()
        .map(|n| {
            std::fs::read(dir.join(n))
                .map_err(|e| format!("checkpoint 帧 {} 读取失败：{e}", dir.join(n).display()))
        })
        .collect()
}

fn make_env(
    spec: &SceneSpec,
    #[cfg_attr(not(feature = "cam"), allow(unused_variables))] cam: bool,
) -> TableTopEnv {
    #[cfg_attr(not(feature = "cam"), allow(unused_mut))]
    let mut env: TableTopEnv = spec.to_env();
    #[cfg(feature = "cam")]
    if cam {
        env.set_camera(Some(oriera_subsys::camera::CameraView::default()));
    }
    env
}

/// Mount the camera tracks: `--cameras-from-spec` puts the spec's
/// `scene.cameras` through the pinhole pipeline (multi RGB/depth
/// tracks, M3-A); the default `--cam` keeps the legacy top view
/// (byte-frozen). Returns the env ready to collect.
fn make_env_with_cameras(spec: &SceneSpec, args: &Args) -> Result<TableTopEnv, String> {
    #[cfg_attr(not(feature = "cam"), allow(unused_mut))]
    let mut env = spec.to_env();
    #[cfg(feature = "cam")]
    if args.cam && args.cameras_from_spec {
        if spec.scene.cameras.is_empty() {
            return Err(
                "--cameras-from-spec：spec.scene.cameras 为空——该 spec 没有声明相机（用带 cameras 字段的 spec，或去掉该 flag 走默认俯视）".into(),
            );
        }
        env.set_cameras(spec.scene.cameras.clone());
    } else if args.cam {
        env.set_camera(Some(oriera_subsys::camera::CameraView::default()));
    }
    #[cfg(not(feature = "cam"))]
    let _ = args;
    Ok(env)
}

/// The dataset's camera-track declarations mirror the env's mounts.
fn declare_camera_tracks(
    ds: &mut LeRobotV21,
    env: &TableTopEnv,
    spec: &SceneSpec,
    args: &Args,
) -> Result<(), String> {
    #[cfg(feature = "cam")]
    if args.cam && args.cameras_from_spec {
        for c in &spec.scene.cameras {
            let p = c.pinhole();
            ds.add_camera(oriera_agent::lerobot::CameraTrack {
                name: c.name.clone(),
                w: p.width,
                h: p.height,
                fps: (1.0 / env.dt).round(),
                depth: p.channels.depth,
            })?;
        }
    } else if args.cam {
        ds.set_camera(oriera_agent::lerobot::CameraTrack {
            name: "top".into(),
            w: 168,
            h: 168,
            fps: (1.0 / env.dt).round(), // 与控制率同频（60Hz 双轨错位修复）
            depth: false,
        });
    }
    #[cfg(not(feature = "cam"))]
    {
        let _ = (ds, env, spec, args);
    }
    Ok(())
}

/// One episode, exactly as the pre-M0b loop drove it: env.reset(seed),
/// the phase script, dataset rows+frames, the degenerate check
/// (spawn-in-goal ends on step ~1 → discard, redraw). When `ds` is
/// `None` the episode runs "dry" (the `sample` determinism re-check).
fn run_episode_once(
    env: &mut TableTopEnv,
    seed: u64,
    phases: &[Phase],
    arm: &IkArm,
    idx: &ObsIdx,
    mut ds: Option<&mut LeRobotV21>,
) -> Result<EpisodeOutcome, String> {
    let mut obs = env.reset(seed);
    if let Some(ds) = ds.as_deref_mut() {
        ds.start_episode(env.task.phrase());
    }
    let mut step = 0u64;
    let mut terminated = false;
    let mut reward_sum = 0.0f32;
    let max_steps = env.spec().max_steps;
    while step < max_steps && !terminated {
        let action = policy(&obs, idx, phases, step, arm);
        // env.step FIRST — it captures frame t for state_t — then
        // row t = (state_t, action_t), then drain. Frame count ==
        // row count by construction (off-by-one, round-2 review).
        let out = env.step(&action);
        if let Some(ds) = ds.as_deref_mut() {
            ds.push_step(&obs, &action)?;
            #[cfg(feature = "cam")]
            for (_ft, frame) in env.take_frames() {
                ds.push_frame(frame)?;
            }
        }
        obs = out.obs;
        reward_sum += out.reward;
        terminated = out.terminated;
        step += 1;
    }
    // Degenerate filter: a spawn landing in the goal zone ends on
    // step ~1 and would enter the dataset as a 1-row/1-frame entry
    // (round-2 finding) — discard and re-draw the seed.
    if step < 5 {
        if let Some(ds) = ds {
            ds.drop_episode();
        }
        return Ok(EpisodeOutcome::Degenerate);
    }
    let fp = env.fingerprint();
    let rand = env.randomization_receipt().to_string();
    if std::env::var("ORIERA_SIM_DEBUG").is_ok() {
        let c = env.cube_pos();
        let g = env.goal();
        eprintln!(
            "seed {seed}: ok={terminated} hold={} cube=({:.3},{:.3},{:.3}) goal=({:.3},{:.3})",
            env.holding(),
            c[0],
            c[1],
            c[2],
            g[0],
            g[2]
        );
    }
    if let Some(ds) = ds {
        ds.finish_episode();
    }
    Ok(EpisodeOutcome::Done(EpisodeRow {
        seed,
        success: terminated,
        steps: step,
        reward_sum,
        fingerprint_v1: fp,
        rand,
    }))
}

/// The whole protocol, through the ONE `evaluate()` entry (P-1):
/// seed band, degenerate redraw policy and episode budget all live in
/// `oriera_agent::eval`; this closure only knows how to run an episode.
/// `io` = `Some` 时逐集落盘 checkpoint 并续跑抽签日志（P1-A1）；`None`
/// = 纯内存（确定性复核的第二跑——必须真跑物理，不能对日志自我印证）。
fn collect(
    args: &Args,
    spec: &SceneSpec,
    protocol: &EvalProtocol,
    mut io: Option<&mut CollectIo>,
) -> Result<RunResult, String> {
    let mut env = make_env_with_cameras(spec, args)?;
    let arm = IkArm::from_robot(&env.robot)?;
    let idx = ObsIdx::from_layout(env.obs_layout());
    // Dataset metadata derives from the env's layouts (P-8): dim names
    // are joint names, modality groups are layout slices, robot_type is
    // the RobotSpec name.
    let mut ds = LeRobotV21::from_layout(
        (1.0 / env.dt).round(),
        &env.robot.name,
        env.obs_layout(),
        env.action_layout(),
    );
    declare_camera_tracks(&mut ds, &env, spec, args)?;
    let phases = match env.task {
        TaskKind::Reach => reach_phases(),
        TaskKind::PickPlace => pick_place_phases(),
        TaskKind::Push => push_phases(),
    };
    // 身份快照只算一次（git_provenance 会 shell out git，spec_hash 要
    // 全量序列化 spec——都不该逐集重算）。
    if let Some(io) = io.as_deref_mut() {
        io.spec_hash = spec.spec_hash_hex();
        io.engine_commit = oriera_agent::manifest::git_provenance().0;
    }
    let task_phrase = env.task.phrase().to_string();
    let mut total_steps = 0u64;
    let result = eval::evaluate(protocol, |seed, draw| {
        // 续跑重放：抽签日志里已有的 draw 按记录处理——保留集从盘上
        // 读回（不重采），退化集重放 65536 位移种子带。seed 对不上 =
        // 日志与协议不符，人话拒绝。
        if let Some(io) = io.as_deref_mut() {
            if let Some(rec) = io.draws.iter().find(|r| r.draw == draw) {
                if rec.seed != seed {
                    return Err(format!(
                        "progress.json draw {draw} 记录的 seed 是 {}，本协议算出 {seed}——日志与本跑不符，用 --force 全量重跑",
                        rec.seed
                    ));
                }
                if rec.degenerate {
                    println!("  ↷ 抽签 {draw}（seed {seed}）：progress 记录为退化集，按日志重放");
                    return Ok(EpisodeOutcome::Degenerate);
                }
                let row = rec.row.as_ref().ok_or_else(|| {
                    format!("progress.json draw {draw} 标记为保留集但缺 row 数据")
                })?;
                let ep_index = ds.episodes.len();
                load_episode_into_ds(io.root, &mut ds, ep_index, row, &task_phrase)?;
                println!(
                    "  ↷ 抽签 {draw}（seed {seed}）：checkpoint 已有（{} 步），读回不重采",
                    row.steps
                );
                return Ok(EpisodeOutcome::Done(row.clone()));
            }
        }
        let out = run_episode_once(&mut env, seed, &phases, &arm, &idx, Some(&mut ds))?;
        if let EpisodeOutcome::Done(row) = &out {
            total_steps += row.steps;
            if std::env::var("ORIERA_SIM_DEBUG").is_ok() {
                println!("  · 抽签 {draw}（seed {seed}）保留");
            }
        } else {
            println!("  · 抽签 {draw}（seed {seed}）生成即入区，已剔除重抽");
        }
        // 逐集 checkpoint：数据文件先落，progress.json 最后原子提交。
        if let Some(io) = io.as_deref_mut() {
            if matches!(out, EpisodeOutcome::Done(_)) {
                checkpoint_episode(io.root, &ds)?;
            }
            io.draws.push(DrawRecord {
                draw,
                seed,
                degenerate: matches!(out, EpisodeOutcome::Degenerate),
                row: match &out {
                    EpisodeOutcome::Done(row) => Some(row.clone()),
                    EpisodeOutcome::Degenerate => None,
                },
            });
            write_progress(io, protocol, &task_phrase)?;
        }
        Ok(out)
    })?;
    Ok(RunResult {
        rows: result
            .rows
            .iter()
            .map(|r| (r.seed, r.success, r.steps, r.reward_sum))
            .collect(),
        hashes: result.rows.iter().map(|r| r.fingerprint_v1).collect(),
        rand_receipts: result.rows.iter().map(|r| r.rand.clone()).collect(),
        ds,
        total_steps,
        draws: result.draws,
        redraws: result.redraws,
    })
}

/// U1: URDF → SceneSpec 落盘（不采集）——一次导入 = 一份实验。
fn import_urdf(args: &Args) {
    let urdf_path = args.urdf.clone().unwrap_or_default();
    let xml = std::fs::read_to_string(&urdf_path)
        .unwrap_or_else(|e| fail(&format!("读 URDF {urdf_path} 失败：{e}")));
    let rep = oriera_agent::urdf::UrdfReport::parse(&xml)
        .unwrap_or_else(|e| fail(&format!("URDF 解析失败（{urdf_path}）：{e}")));
    let (desc, skipped) = rep
        .first_revolute_chain(4)
        .unwrap_or_else(|e| fail(&format!("URDF 链提取失败（{urdf_path}）：{e}")));
    let mut spec = SceneSpec::preset(TaskKind::PickPlace);
    spec.robot = RobotSpec::from(&desc);
    spec.meta.name = "urdf-import".into();
    spec.meta.notes = Some(format!("URDF 导入：{}（skipped: {skipped:?}）", urdf_path));
    spec.validate()
        .unwrap_or_else(|e| fail(&format!("导入的 spec 校验失败：{e}")));
    let out_dir = std::path::Path::new(&args.out);
    std::fs::create_dir_all(out_dir)
        .unwrap_or_else(|e| fail(&format!("输出目录创建失败（{}）：{e}", out_dir.display())));
    let path = out_dir.join("scene_spec.json");
    let json = serde_json::to_string_pretty(&spec)
        .map_err(|e| format!("spec 序列化失败：{e}"))
        .unwrap_or_else(|e| fail(&e));
    std::fs::write(&path, json)
        .unwrap_or_else(|e| fail(&format!("写入 {} 失败：{e}", path.display())));
    println!(
        "  ✓ URDF → {}（skipped: {skipped:?}，spec_hash {}）",
        path.display(),
        spec.spec_hash_hex()
    );
}

/// `--migrate FILE`: load (v1 or v2), write the v2 JSON next to `--out`.
fn migrate_spec(args: &Args) {
    let path = args.migrate.clone().unwrap_or_default();
    let loaded = SceneSpec::load_file(std::path::Path::new(&path))
        .unwrap_or_else(|e| fail(&format!("spec 加载失败：{e}")));
    let out_dir = std::path::Path::new(&args.out);
    std::fs::create_dir_all(out_dir)
        .unwrap_or_else(|e| fail(&format!("输出目录创建失败（{}）：{e}", out_dir.display())));
    let dst = out_dir.join("spec_v2.json");
    let json = serde_json::to_string_pretty(&loaded.spec)
        .map_err(|e| format!("spec 序列化失败：{e}"))
        .unwrap_or_else(|e| fail(&e));
    std::fs::write(&dst, json)
        .unwrap_or_else(|e| fail(&format!("写入 {} 失败：{e}", dst.display())));
    println!(
        "  ✓ {} → {}（{}，spec_hash {}）",
        path,
        dst.display(),
        if loaded.migrated {
            "v1 → oriera.scene/2"
        } else {
            "已是 oriera.scene/2，原样规范化"
        },
        loaded.spec.spec_hash_hex()
    );
}

/// `--validate FILE`: parse + validate + print the hash; non-zero exit on error.
fn validate_spec(args: &Args) {
    let path = args.validate.clone().unwrap_or_default();
    let loaded = SceneSpec::load_file(std::path::Path::new(&path))
        .unwrap_or_else(|e| fail(&format!("spec 校验失败：{e}")));
    println!(
        "  ✓ {} 合法（schema {}，task {}，robot {}/{} DoF，{} 个 prop；spec_hash {}）{}",
        path,
        loaded.spec.schema,
        loaded.spec.task.id,
        loaded.spec.robot.name,
        loaded.spec.robot.dof(),
        loaded.spec.scene.props.len(),
        loaded.spec.spec_hash_hex(),
        if loaded.migrated {
            "［v1 草案，已按迁移规则解释］"
        } else {
            ""
        }
    );
}

fn main() {
    let args = parse_args();
    if args.urdf.is_some() {
        import_urdf(&args);
        return;
    }
    if args.migrate.is_some() {
        migrate_spec(&args);
        return;
    }
    if args.validate.is_some() {
        validate_spec(&args);
        return;
    }
    let spec = resolve_spec(&args);
    let spec_hash = spec.spec_hash_hex();

    // The named protocol (P-1): course@v1 is the default (= the legacy
    // classroom contract), paper@v1 records and never gates. `--seed`
    // overrides seed0; `--gate` overrides the protocol's own gate.
    let protocol = EvalProtocol::parse(
        args.protocol.as_deref().unwrap_or(eval::PROTOCOL_COURSE_V1),
        args.episodes,
        args.seed0,
    )
    .unwrap_or_else(|e| fail(&e))
    .with_gate(args.gate)
    .unwrap_or_else(|e| fail(&e));

    // Runs layout (P-5): --runs-root runs/ → runs/<hash8>/<method>/<seed>/
    // with skip-if-done resume; without it, --out direct writes (the
    // legacy behaviour, byte-identical). 同给两个 flag 不再静默（SRE
    // 遗留）：--out 被忽略，当面说清。
    if args.runs_root.is_some() && args.out_given {
        println!(
            "  ⚠ --runs-root 与 --out 同时给出：--out {:?} 被忽略，输出落 runs/ 布局（二者只能选一）",
            args.out
        );
    }
    let out: std::path::PathBuf = match &args.runs_root {
        Some(root) => {
            let proto8 = protocol.protocol_hash8();
            let method = format!("scripted-p{proto8}");
            let dir = std::path::Path::new(root)
                .join(spec.spec_hash8())
                .join(&method)
                .join(protocol.seed0.to_string());
            let done = dir.join("eval_report.json");
            if done.exists() && !args.force {
                println!(
                    "  ↷ 断点续跑：{} 已有 eval_report.json，跳过（--force 重跑）",
                    dir.display()
                );
                return;
            }
            dir
        }
        None => std::path::Path::new(&args.out).to_path_buf(),
    };
    std::fs::create_dir_all(&out).unwrap_or_else(|e| {
        fail(&format!(
            "输出目录创建失败（{}）：{e}
常见原因：权限不足 / 路径已存在同名文件 / 盘满——换 --out 或先清掉同名文件",
            out.display()
        ))
    });

    // Provenance first (P-3): even a crashed run leaves its manifest.
    write_run_manifest_opts(
        out.join("run_manifest.json"),
        &spec,
        &ManifestExtra {
            protocol_id: protocol.id.clone(),
            protocol_hash: protocol.protocol_hash_hex(),
            method: "scripted".into(),
            seed: Some(protocol.seed0),
        },
        args.sanitize,
    )
    .unwrap_or_else(|e| fail(&format!("run_manifest.json 写入失败：{e}")));

    let task = TaskKind::parse(&spec.task.id).expect("validated task id");
    if args.cameras_from_spec && args.cam && !cfg!(feature = "cam") {
        fail("--cameras-from-spec 需要 --features cam（无该特性引擎不渲染图像轨）");
    }
    let feat = if cfg!(feature = "cam") && args.cam {
        if args.cameras_from_spec {
            "rigid3d+cam+spec-cameras"
        } else {
            "rigid3d+cam"
        }
    } else {
        "rigid3d"
    };
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║  OriEra rigid3d × LeRobot v2.1 · 具身批量采集与评测          ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!(
        "  任务 {} · 协议 {}（{} 集，闸 {:?}）× ≤{} 步 · dt={:.4} · 种子 {}.. · 后端 {}",
        task.phrase(),
        protocol.id,
        protocol.episodes,
        protocol.gate,
        spec.max_steps,
        spec.dt,
        protocol.seed0,
        feat
    );
    println!(
        "  spec {} · robot {} ({} DoF) · prop[0] {} {:?} · spec_hash {} · protocol_hash {}",
        args.spec.as_deref().unwrap_or("<preset>"),
        spec.robot.name,
        spec.robot.dof(),
        spec.scene.props[0].name,
        spec.scene.props[0].shape,
        spec_hash,
        protocol.protocol_hash_hex()
    );

    // Episode 级断点续跑（P1-A1）：第一跑永远逐集落盘 checkpoint（kill
    // 随时可续）；有 progress.json 则按日志续采（身份校验不过 = 人话
    // 致命）。--force 清掉 checkpoint 树与派生导出全量重采；全新跑顺带
    // 清掉无日志的陈旧 checkpoint 文件（半集残留会被 converter 的
    // episode_*.jsonl glob 误吃——逐集落盘后必须清理，write_dir 不删旧）。
    let v21_root = out.join("lerobot-v21");
    let mut resumed_draws = Vec::new();
    if args.force {
        for dir in ["lerobot-v21", "lerobot-v30", "npz"] {
            let p = out.join(dir);
            if p.exists() {
                std::fs::remove_dir_all(&p)
                    .unwrap_or_else(|e| fail(&format!("--force 清理 {} 失败：{e}", p.display())));
            }
        }
    } else {
        let progress_path = v21_root.join("progress.json");
        if progress_path.exists() {
            let prog = load_progress(&progress_path, &spec, &protocol).unwrap_or_else(|e| fail(&e));
            let kept = prog.draws.iter().filter(|d| !d.degenerate).count();
            let engine_now = oriera_agent::manifest::git_provenance().0;
            if !prog.engine_commit.is_empty()
                && prog.engine_commit != engine_now
                && engine_now != "unknown"
            {
                println!(
                    "  ⚠ progress.json 的 engine_commit 是 {}，本树是 {engine_now}——引擎变了续跑请自查确定性（--verify-determinism=full 会兜底复核）",
                    prog.engine_commit
                );
            }
            println!(
                "  ↷ 断点续跑：{} 已有 progress.json（{} 抽签 / {}/{} 集已完成）——跳过已完成集，只补剩余 {} 集（--force 全量重跑）",
                progress_path.display(),
                prog.draws.len(),
                kept,
                protocol.episodes,
                (protocol.episodes as usize).saturating_sub(kept)
            );
            resumed_draws = prog.draws;
        } else if v21_root.exists() {
            // 无日志的残留树（上次跑被 kill 在首集之前等）：清掉，避免
            // 陈旧 episode 文件混进本次数据集。
            std::fs::remove_dir_all(&v21_root).unwrap_or_else(|e| {
                fail(&format!(
                    "清理陈旧 checkpoint {} 失败：{e}",
                    v21_root.display()
                ))
            });
        }
    }
    let mut collect_io = Some(CollectIo {
        root: &v21_root,
        draws: resumed_draws,
        spec_hash: String::new(),
        engine_commit: String::new(),
    });

    // The determinism receipt (P-5): full = collect twice, demand bit
    // equality; sample = re-run the first kept episode; off = trust the seed.
    let t0 = std::time::Instant::now();
    let r1 = collect(&args, &spec, &protocol, collect_io.as_mut())
        .unwrap_or_else(|e| fail(&format!("采集失败：{e}")));
    match args.verify {
        Verify::Full => {
            // 第二跑纯内存（io=None）：对盘上 checkpoint 重放出来的 r1
            // 逐位复核——resume 的「读回 = 重采」由此钉死。
            let r2 = collect(&args, &spec, &protocol, None)
                .unwrap_or_else(|e| fail(&format!("采集失败：{e}")));
            assert_eq!(
                r1.hashes, r2.hashes,
                "#66+#67: episode fingerprints must match"
            );
            assert_eq!(r1.rows, r2.rows, "episode outcomes must be bit-identical");
            for (i, (e1, e2)) in r1.ds.episodes.iter().zip(r2.ds.episodes.iter()).enumerate() {
                assert_eq!(e1.rows, e2.rows, "dataset rows must be bit-equal (ep {i})");
                assert_eq!(
                    e1.tracks, e2.tracks,
                    "camera frames must be bit-equal (ep {i})"
                );
            }
            println!("  ✓ 确定性复核（full）：双跑逐位一致");
        }
        Verify::Sample => {
            let seed0 = r1.rows.first().expect("at least one episode").0;
            let mut env = make_env(&spec, false);
            let arm = IkArm::from_robot(&env.robot).unwrap_or_else(|e| fail(&e));
            let idx = ObsIdx::from_layout(env.obs_layout());
            let phases = match env.task {
                TaskKind::Reach => reach_phases(),
                TaskKind::PickPlace => pick_place_phases(),
                TaskKind::Push => push_phases(),
            };
            let re = match run_episode_once(&mut env, seed0, &phases, &arm, &idx, None)
                .unwrap_or_else(|e| fail(&format!("sample 复核失败：{e}")))
            {
                EpisodeOutcome::Done(row) => row,
                EpisodeOutcome::Degenerate => panic!("sample 复核：首集不应是退化集"),
            };
            let orig = r1.rows.first().unwrap();
            assert_eq!(
                (
                    re.seed,
                    re.steps,
                    re.success,
                    re.reward_sum,
                    re.fingerprint_v1
                ),
                (orig.0, orig.2, orig.1, orig.3, r1.hashes[0]),
                "sample 复核：首集逐位不一致"
            );
            println!("  ✓ 确定性复核（sample）：首集 seed {seed0} 重跑逐位一致");
        }
        Verify::Off => {
            println!("  · 确定性复核关闭（--verify-determinism=off，批量扫参用）");
        }
    }
    let secs = t0.elapsed().as_secs_f64();

    let successes = r1.rows.iter().filter(|(_, ok, _, _)| *ok).count();
    let episodes = protocol.episodes as usize;
    println!(
        "  ✓ 采集 {} 集 · {} transitions · 成功 {} 集（{}%） · 吞吐 {:.0} steps/s（单跑 {:.1}s）",
        episodes,
        r1.ds.total_frames(),
        successes,
        successes * 100 / episodes.max(1),
        r1.total_steps as f64 / secs.max(1e-9),
        secs
    );

    // The unified eval result (one entry point, one number).
    let result = EvalResult {
        protocol: protocol.clone(),
        rows: r1
            .rows
            .iter()
            .zip(r1.hashes.iter().zip(r1.rand_receipts.iter()))
            .map(|((seed, ok, steps, ret), (fp, rand))| EpisodeRow {
                seed: *seed,
                success: *ok,
                steps: *steps,
                reward_sum: *ret,
                fingerprint_v1: *fp,
                rand: rand.clone(),
            })
            .collect(),
        draws: r1.draws,
        redraws: r1.redraws,
    };

    // Solvability gate: course@v1's Hard90 refuses to publish below
    // 90% (两级渐降相位机 + 肘限位 -2.5 后实测 pick 91.7% / reach
    // 100%）；push 的接触基线允许不稳，如实报告不拦流程；paper@v1 永远
    // 只记录。闸失败 = 人话 + 下一步（G-2），不是 backtrace。
    let verdict = result.gate_verdict(spec.task.id.as_str());
    if let eval::GateVerdict::Fail { threshold, rate } = verdict {
        eprintln!(
            "\n【90% 闸未过】脚本基线只成功 {successes}/{} 集（{:.1}%），低于 {:.0}%，引擎拒绝出数据——这是引擎自检在工作，不是你弄坏了电脑。\n\
             请按顺序排查：① 确认命令与 LAB-GUIDE §1 第 4 条逐字一致（集数改少会放大偶然失败）；\n\
             ② `git status` 看是否改过引擎文件，有就 `git checkout -- .`；③ 确认没改参数/没改 spec（spec_hash {spec_hash}）后重跑；\n\
             三步后仍失败，带着整段输出找助教。只要记录不拦流程请改用 --protocol paper@v1 或 --gate=report。",
            episodes,
            rate * 100.0,
            threshold * 100.0
        );
        std::process::exit(2);
    }
    if matches!(verdict, eval::GateVerdict::NotApplied) {
        // P0-3（20 角色审查）：report 协议下 gate_verdict 恒 not-applied，
        // 但 eval_report.baseline_gate 会照抄闸的「理论」判定——不读源码
        // 的实验室脚本会误判成 fail。这里把口径打在控制台正面。
        println!(
            "  · 闸：本协议/任务不设硬闸（{} × {:?}）——只记录不拦流程；             eval_report.baseline_gate 为理论参考值，**不是本跑的实际判定**             （实际判定以 exit code 与 gate 字段为准：两处均为非拦截）",
            protocol.id, protocol.gate
        );
    }

    let root = out.join("lerobot-v21");
    std::fs::create_dir_all(&root)
        .unwrap_or_else(|e| fail(&format!("数据集目录创建失败（{}）：{e}", root.display())));
    let n = r1.ds.write_dir(&root).unwrap_or_else(|e| {
        fail(&format!(
            "数据集写出失败（{}）：{e}
常见原因：磁盘满 / 权限不足——清空间或换 --out 后重跑（本跑数据在内存未丢，重跑即得）",
            root.display()
        ))
    });
    let engine = concat!("oriera-engine/", env!("CARGO_PKG_VERSION"));
    // 三口径指纹集（state/v1/v2）在 receipt 之前算好——数据集自带完整身份。
    let fps = eval::fingerprint_set(&spec.robot, spec.spec_hash(), &r1.hashes);
    LeRobotV21::write_receipt(
        &root,
        engine,
        feat,
        spec.dt,
        &spec_hash,
        &r1.hashes,
        successes as u32,
        &fps.fingerprint_scheme,
        &fps.episode_fingerprints_v2,
    )
    .unwrap_or_else(|e| {
        fail(&format!(
            "oriera_receipt.json 写入失败（{}）：{e}",
            root.display()
        ))
    });
    LeRobotV21::write_card(&root, task.phrase()).unwrap_or_else(|e| {
        fail(&format!(
            "数据集卡片（README.md）写入失败（{}）：{e}",
            root.display()
        ))
    });
    // The resolved spec rides with the dataset — the file IS the experiment.
    // 脱敏导出（--sanitize）不落盘：身份只经 manifest/receipt 的 spec_hash
    // 对账，不声称逐位可复现（P-B3）。
    if !args.sanitize {
        let spec_json = serde_json::to_string_pretty(&spec)
            .map_err(|e| format!("spec 序列化失败：{e}"))
            .unwrap_or_else(|e| fail(&e));
        std::fs::write(root.join("meta/oriera_spec.json"), spec_json).unwrap_or_else(|e| {
            fail(&format!(
                "meta/oriera_spec.json 写入失败（{}）：{e}",
                root.display()
            ))
        });
    }

    // LeRobot v3.0 tree (P-7): lerobot ≥ 0.4 reads only this layout.
    let root_v3 = out.join("lerobot-v30");
    if args.lerobot_v3 {
        std::fs::create_dir_all(&root_v3)
            .unwrap_or_else(|e| fail(&format!("v3 目录创建失败（{}）：{e}", root_v3.display())));
        r1.ds.write_dir_v3(&root_v3).unwrap_or_else(|e| {
            fail(&format!(
                "v3 数据集写出失败（{}）：{e}
常见原因：磁盘满 / 权限不足——清空间后重跑",
                root_v3.display()
            ))
        });
        LeRobotV21::write_receipt(
            &root_v3,
            engine,
            feat,
            spec.dt,
            &spec_hash,
            &r1.hashes,
            successes as u32,
            &fps.fingerprint_scheme,
            &fps.episode_fingerprints_v2,
        )
        .unwrap_or_else(|e| fail(&format!("v3 receipt 写入失败：{e}")));
        LeRobotV21::write_card_v3(&root_v3, task.phrase())
            .unwrap_or_else(|e| fail(&format!("v3 数据集卡片写入失败：{e}")));
        if !args.sanitize {
            let spec_json = serde_json::to_string_pretty(&spec)
                .map_err(|e| format!("spec 序列化失败：{e}"))
                .unwrap_or_else(|e| fail(&e));
            std::fs::write(root_v3.join("meta/oriera_spec.json"), spec_json).unwrap_or_else(|e| {
                fail(&format!(
                    "meta/oriera_spec.json 写入失败（{}）：{e}",
                    root_v3.display()
                ))
            });
        }
    }

    // npz bare export (U-8): the SAME r1.ds rows as raw float32 arrays —
    // one numpy.load-able archive per episode, receipt + spec alongside
    // (the identity travels with every encoding). Opt-in only.
    let root_npz = out.join("npz");
    if args.export_npz {
        r1.ds
            .write_npz(&root_npz)
            .unwrap_or_else(|e| fail(&format!("npz 写出失败：{e}")));
        LeRobotV21::write_receipt(
            &root_npz,
            engine,
            feat,
            spec.dt,
            &spec_hash,
            &r1.hashes,
            successes as u32,
            &fps.fingerprint_scheme,
            &fps.episode_fingerprints_v2,
        )
        .unwrap_or_else(|e| fail(&format!("npz receipt 写入失败：{e}")));
        if !args.sanitize {
            let spec_json = serde_json::to_string_pretty(&spec)
                .map_err(|e| format!("spec 序列化失败：{e}"))
                .unwrap_or_else(|e| fail(&e));
            std::fs::write(root_npz.join("meta/oriera_spec.json"), spec_json).unwrap_or_else(|e| {
                fail(&format!(
                    "meta/oriera_spec.json 写入失败（{}）：{e}",
                    root_npz.display()
                ))
            });
        }
    }

    // eval_report.json — the lab reporting format, now with protocol
    // id + parameter hash and the three fingerprint views (P-1/P-10).

    let (lo, hi) = result.wilson95();
    let per_ep: Vec<serde_json::Value> = result
        .rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "seed": row.seed,
                "success": row.success,
                "steps": row.steps,
                "reward_sum": ((row.reward_sum as f64) * 1000.0).round() / 1000.0,
                "rand": row.rand.replace('"', "'"),
            })
        })
        .collect();
    let report = serde_json::json!({
        "task": task.phrase(),
        "task_id": spec.task.id,
        "protocol": {
            "id": protocol.id,
            "parameter_hash": protocol.protocol_hash_hex(),
            "episodes": protocol.episodes,
            "seed0": protocol.seed0,
            "degenerate": protocol.degenerate,
            "gate": protocol.gate,
            "success_criteria": protocol.success_criteria,
        },
        "gate": match verdict {
            eval::GateVerdict::Pass => "pass",
            eval::GateVerdict::Fail { .. } => "fail",
            eval::GateVerdict::NotApplied => "not-applied",
        },
        "spec_hash": spec_hash,
        "engine": engine,
        "backend": feat,
        "dt": spec.dt,
        "episodes": episodes,
        "successes": successes,
        "success_rate": successes as f64 / episodes.max(1) as f64,
        "wilson95": [lo, hi],
        "throughput_steps_per_s": r1.total_steps as f64 / secs.max(1e-9),
        "verify_determinism": match args.verify { Verify::Full => "full", Verify::Sample => "sample", Verify::Off => "off" },
        "randomization": r1.rand_receipts.first().map(|s| s.replace('"', "'")).unwrap_or_default(),
        "runs": per_ep,
    });
    let mut report = report;
    // fingerprint block rides as its own object (scheme + 3 views)
    let fp_val = serde_json::to_value(&fps).expect("FingerprintSet serializes");
    report
        .as_object_mut()
        .unwrap()
        .insert("fingerprints".into(), fp_val);
    let report_path = out.join("eval_report.json");
    std::fs::write(
        &report_path,
        format!("{}\n", serde_json::to_string(&report).unwrap()),
    )
    .unwrap_or_else(|e| {
        fail(&format!(
            "eval_report.json 写入失败（{}）：{e}",
            report_path.display()
        ))
    });

    println!(
        "  ✓ 评测：成功率 {:.1}%（Wilson 95% CI [{:.1}%, {:.1}%]）· 协议 {}（{}）· 指纹 {}",
        result.success_rate() * 100.0,
        lo * 100.0,
        hi * 100.0,
        protocol.id,
        protocol.protocol_hash_hex(),
        fps.fingerprint_scheme
    );
    println!(
        "  ✓ 已写出 {}/（{n} 集）与 {}（manifest {}）",
        root.display(),
        report_path.display(),
        out.join("run_manifest.json").display()
    );
    if args.lerobot_v3 {
        println!(
            "  ✓ 已写出 {}/（LeRobot v3.0 布局；结构自检：python scripts/lerobot_v3_selfcheck.py {}）",
            root_v3.display(),
            root_v3.display()
        );
    }
    if args.export_npz {
        println!(
            "  ✓ 已写出 {}/（{} 个 episode_*.npz：obs_<i>/act_<i> float32 + meta.json，numpy.load 直读）",
            root_npz.display(),
            r1.ds.episodes.len()
        );
    }

    // --fingerprints FILE: the absolute golden (P-10) — commit it under
    // contracts/golden/ and any platform can compare receipts.
    if let Some(path) = &args.fingerprints {
        let golden = eval::FingerprintGolden {
            scheme: "oriera-embodied-v2".into(),
            purpose: format!(
                "M0b 绝对指纹金标（scheme v2）：默认桌面实验 {} 集逐集「纯状态哈希 + v1 折叠 + v2 完整身份」三口径（同一 12 seed 重跑逐位一致；变动须 physics-behavior-change 标签 + scheme 递增）",
                protocol.episodes
            ),
            engine_commit: oriera_agent::manifest::git_provenance().0,
            features: feat.into(),
            backend: feat.into(),
            dt: spec.dt,
            command: format!(
                "cargo run -p oriera-agent --example embodied_sim_collect --features rigid3d,cam,spec,urdf -- --task {} --episodes {} --seed {} --fingerprints <FILE>",
                task_base(&spec.task.id),
                protocol.episodes,
                protocol.seed0
            ),
            spec_hash: spec_hash.clone(),
            protocol_id: protocol.id.clone(),
            protocol_parameter_hash: protocol.protocol_hash_hex(),
            identity_hash: robot_identity_hash(&spec.robot),
            seeds: result.rows.iter().map(|r| r.seed).collect(),
            successes,
            success_rate: result.success_rate(),
            transitions: r1.ds.total_frames() as u64,
            fingerprints: fps,
        };
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&golden).unwrap()),
        )
        .unwrap_or_else(|e| fail(&format!("--fingerprints 写入失败：{e}")));
        println!(
            "  ✓ 指纹金标（scheme {}）已写出 {}",
            golden.fingerprints.fingerprint_scheme, path
        );
    }

    println!(
        "  下一步：python scripts/jsonl_to_lerobot_v21.py {}/ → lerobot-train 直读",
        root.display()
    );
}

/// `pick_place@v1` → `pick_place` (the CLI task spelling).
fn task_base(id: &str) -> String {
    id.split('@').next().unwrap_or(id).to_string()
}
