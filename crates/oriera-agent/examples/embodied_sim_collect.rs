//! Embodied 3D collection (PLAN-embodied-sim) — the lab batch runner.
//!
//! Drives [`TableTopEnv`] (L9 environment contract × L6 `rigid3d`
//! multibody backend) with analytic-IK scripted baselines across the
//! task family, records every step into a **LeRobotDataset v2.1** tree
//! (JSONL rows + camera PNG frames + full meta; `--lerobot-v3` adds the
//! v3.0 tree) and pins the reproducibility claim: the whole collection
//! runs TWICE and both runs must be bit-identical — dataset, rewards
//! and the per-episode `sim_fingerprint` receipts (#66 + #67 at sim
//! granularity).
//!
//! **M0a (PLAN-embodied-general)**: the experiment is a spec. `--spec`
//! loads a `oriera.scene/2` JSON (v1 drafts auto-migrate), `--set
//! a.b=v` overrides fields (they enter the hash), and the legacy
//! `--task` path builds the exact same spec through
//! `SceneSpec::preset` — both paths run the same code and the receipt
//! carries `spec_hash`. The scripted IK baseline reads the arm geometry
//! from the RobotSpec (4-DoF preset only — honest error otherwise).
//!
//! An `eval_report.json` lands next to the dataset: per-episode
//! outcome plus the success rate with a Wilson 95% interval — the
//! reporting format lab experiments are held to.
//!
//! Run:
//! `cargo run -p oriera-agent --example embodied_sim_collect --features rigid3d,cam,spec,urdf -- --task pick_place --episodes 12 --cam`
//! `… -- --spec crates/oriera-agent/specs/tabletop-4dof-pick.json --episodes 12`
//! Args: `--task reach|pick_place|push` · `--spec FILE` · `--set a.b=v`
//!       `--episodes N` · `--seed0 S` · `--cam` / `--no-cam` · `--out DIR`
//!       `--lerobot-v3` · `--migrate FILE` · `--validate FILE` · `--urdf FILE`
//! Output: `<out>/lerobot-v21/` [+ `<out>/lerobot-v30/`] + `<out>/eval_report.json`

use oriera_agent::embodied_env::{EmbodiedEnv, ObsLayout, TableTopEnv, TaskKind};
use oriera_agent::lerobot::LeRobotV21;
use oriera_agent::spec::SceneSpec;
// CameraTrack 只被 cam 腿消费（set_camera 两个 cfg 块）——无 cam 特性下
// 不引入，否则 `--features urdf,spec --all-targets` 的 clippy -D warnings
// 以 unused-imports 红灯。
#[cfg(feature = "cam")]
use oriera_agent::lerobot::CameraTrack;
use oriera_subsys::RobotSpec;

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
            links: [r.joints[2].anchor[0], r.joints[3].anchor[0], g.tcp_offset[0]],
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
            layout
                .index_of(n)
                .unwrap_or_else(|| panic!("obs layout 缺少 {n}（脚本基线需要 ee/obj_rel/goal_rel）"))
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

/// Wilson 95% score interval — the honest way to report a success rate
/// from n episodes (never a bare point estimate).
fn wilson95(k: usize, n: usize) -> (f64, f64) {
    let z = 1.959_963_984_540_054_f64; // 0.975 quantile
    if n == 0 {
        return (0.0, 1.0);
    }
    let (k, n) = (k as f64, n as f64);
    let p = k / n;
    let denom = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denom;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denom;
    (
        (center - half).clamp(0.0, 1.0),
        (center + half).clamp(0.0, 1.0),
    )
}

struct Args {
    /// `--task` / `--phrase` (None ⇒ the spec's task, or pick_place).
    task: Option<TaskKind>,
    /// `--spec FILE` (None ⇒ the preset spec of `task`).
    spec: Option<String>,
    /// `--set a.b=v` overrides (applied after load, enter the hash).
    sets: Vec<String>,
    episodes: u32,
    seed0: u64,
    cam: bool,
    out: String,
    /// Also write the LeRobot v3.0 tree (`<out>/lerobot-v30/`).
    lerobot_v3: bool,
    /// U1: URDF→SceneSpec 导出路径（设置即只做导入不采集）。
    urdf: Option<String>,
    /// `--migrate FILE`: v1 → v2 spec 落盘（不采集）。
    migrate: Option<String>,
    /// `--validate FILE`: 只校验 + 打印 spec_hash（不采集）。
    validate: Option<String>,
}

const USAGE: &str = "--task/--phrase/--spec/--set/--episodes/--seed0/--cam/--no-cam/--out/--lerobot-v3/--migrate/--validate/--urdf";

fn parse_args() -> Args {
    let mut a = Args {
        task: None,
        spec: None,
        sets: Vec::new(),
        episodes: 8,
        seed0: 1000,
        cam: true,
        out: "out/embodied-sim".into(),
        lerobot_v3: false,
        urdf: None,
        migrate: None,
        validate: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--task" => {
                let v = it.next().expect("--task 需要一个值");
                a.task = Some(TaskKind::parse(&v).expect("task: reach|pick_place|push"));
            }
            "--spec" => a.spec = Some(it.next().expect("--spec 需要一个文件路径")),
            "--set" => a.sets.push(it.next().expect("--set 需要 a.b.c=v")),
            "--urdf" => a.urdf = Some(it.next().expect("--urdf 需要一个文件路径")),
            "--migrate" => a.migrate = Some(it.next().expect("--migrate 需要一个文件路径")),
            "--validate" => a.validate = Some(it.next().expect("--validate 需要一个文件路径")),
            "--phrase" => {
                // 语言→具身最小桥：自然语言短语解析出任务（NLU→seed 组合
                // 由调用方继续），例如 --phrase "pick up the cube"。
                let v = it.next().expect("--phrase 需要一个值");
                a.task = Some(
                    TaskKind::from_phrase(&v).unwrap_or_else(|| panic!("无法从短语解析任务：{v}")),
                );
            }
            "--episodes" => {
                let v = it.next().expect("--episodes 需要一个值");
                a.episodes = v.parse().expect("episodes: 数字");
            }
            "--seed0" => {
                let v = it.next().expect("--seed0 需要一个值");
                a.seed0 = v.parse().expect("seed0: 数字");
            }
            "--out" => a.out = it.next().expect("--out 需要一个值"),
            "--cam" => a.cam = true,
            "--no-cam" => a.cam = false,
            "--lerobot-v3" => a.lerobot_v3 = true,
            other => {
                panic!("未知参数 {other}（可用 {USAGE}）")
            }
        }
    }
    a
}

/// Resolve the experiment spec: `--spec` (v2, or v1 auto-migrated) +
/// `--set` overrides, else the preset of `--task` (default pick_place).
/// Both roads yield a validated [`SceneSpec`] — one code path.
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
}

fn collect(args: &Args, spec: &SceneSpec) -> Result<RunResult, String> {
    let mut env: TableTopEnv = spec.to_env();
    let arm = IkArm::from_robot(&env.robot)?;
    let idx = ObsIdx::from_layout(env.obs_layout());
    #[cfg(feature = "cam")]
    if args.cam {
        env.set_camera(Some(oriera_subsys::camera::CameraView::default()));
    }
    // Dataset metadata derives from the env's layouts (P-8): dim names
    // are joint names, modality groups are layout slices, robot_type is
    // the RobotSpec name.
    let mut ds = LeRobotV21::from_layout(
        (1.0 / env.dt).round(),
        &env.robot.name,
        env.obs_layout(),
        env.action_layout(),
    );
    #[cfg(feature = "cam")]
    if args.cam {
        ds.set_camera(CameraTrack {
            name: "top".into(),
            w: 168,
            h: 168,
            fps: (1.0 / env.dt).round(), // 与控制率同频（60Hz 双轨错位修复）
        });
    }

    let phases = match env.task {
        TaskKind::Reach => reach_phases(),
        TaskKind::PickPlace => pick_place_phases(),
        TaskKind::Push => push_phases(),
    };

    let mut rows = Vec::new();
    let mut hashes = Vec::new();
    let mut rand_receipts: Vec<String> = Vec::new();
    // 重抽语义：退化集不占 episodes 预算（Rust 架构师 review——原实现
    // 是"少采"不是"重抽"，成功率分母失真）。displaced seed band 保证
    // 同种子不再撞同一 in-zone 生成。
    let mut kept: u32 = 0;
    let mut redraws: u64 = 0;
    let mut draws: u64 = 0;
    while kept < args.episodes {
        draws += 1;
        let seed = args.seed0 + (draws - 1) + 65536 * redraws;
        let mut obs = env.reset(seed);
        // episode 语言标注 = 任务自然语言短语（SmolVLA/π0 语言条件）
        ds.start_episode(env.task.phrase());
        let mut step = 0u64;
        let mut terminated = false;
        let mut reward_sum = 0.0f32;
        let max_steps = env.spec().max_steps;
        while step < max_steps && !terminated {
            let action = policy(&obs, &idx, &phases, step, &arm);
            // env.step FIRST — it captures frame t for state_t — then
            // row t = (state_t, action_t), then drain. Frame count ==
            // row count by construction (off-by-one, round-2 review).
            let out = env.step(&action);
            ds.push_step(&obs, &action)?;
            #[cfg(feature = "cam")]
            for (_ft, png) in env.take_frames() {
                ds.push_frame(png)?;
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
            ds.drop_episode();
            redraws += 1;
            println!("  · 抽签 {draws}（seed {seed}）生成即入区，已剔除重抽");
            continue;
        }
        kept += 1;
        hashes.push(env.fingerprint());
        rand_receipts.push(env.randomization_receipt().to_string());
        rows.push((seed, terminated, step, reward_sum));
        if std::env::var("ORIERA_SIM_DEBUG").is_ok() {
            let c = env.cube_pos();
            let g = env.goal();
            eprintln!(
                "抽签 {draws}: ok={terminated} hold={} cube=({:.3},{:.3},{:.3}) goal=({:.3},{:.3})",
                env.holding(),
                c[0],
                c[1],
                c[2],
                g[0],
                g[2]
            );
        }
        ds.finish_episode();
    }
    Ok(RunResult {
        ds,
        rows,
        hashes,
        rand_receipts,
    })
}

/// U1: URDF → SceneSpec 落盘（不采集）——一次导入 = 一份实验。
fn import_urdf(args: &Args) {
    let xml = std::fs::read_to_string(args.urdf.as_ref().unwrap()).expect("读 URDF 失败");
    let rep = oriera_agent::urdf::UrdfReport::parse(&xml).expect("URDF 解析失败");
    let (desc, skipped) = rep.first_revolute_chain(4).expect("URDF 链提取失败");
    let mut spec = SceneSpec::preset(TaskKind::PickPlace);
    spec.robot = RobotSpec::from(&desc);
    spec.meta.name = "urdf-import".into();
    spec.meta.notes = Some(format!("URDF 导入：{}（skipped: {skipped:?}）", args.urdf.as_ref().unwrap()));
    spec.validate().unwrap_or_else(|e| fail(&format!("导入的 spec 校验失败：{e}")));
    let out_dir = std::path::Path::new(&args.out);
    std::fs::create_dir_all(out_dir).unwrap();
    let path = out_dir.join("scene_spec.json");
    std::fs::write(&path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    println!(
        "  ✓ URDF → {}（skipped: {skipped:?}，spec_hash {}）",
        path.display(),
        spec.spec_hash_hex()
    );
}

/// `--migrate FILE`: load (v1 or v2), write the v2 JSON next to `--out`.
fn migrate_spec(args: &Args) {
    let path = args.migrate.as_ref().unwrap();
    let loaded = SceneSpec::load_file(std::path::Path::new(path))
        .unwrap_or_else(|e| fail(&format!("spec 加载失败：{e}")));
    let out_dir = std::path::Path::new(&args.out);
    std::fs::create_dir_all(out_dir).unwrap();
    let dst = out_dir.join("spec_v2.json");
    std::fs::write(&dst, serde_json::to_string_pretty(&loaded.spec).unwrap()).unwrap();
    println!(
        "  ✓ {} → {}（{}，spec_hash {}）",
        path,
        dst.display(),
        if loaded.migrated { "v1 → oriera.scene/2" } else { "已是 oriera.scene/2，原样规范化" },
        loaded.spec.spec_hash_hex()
    );
}

/// `--validate FILE`: parse + validate + print the hash; non-zero exit on error.
fn validate_spec(args: &Args) {
    let path = args.validate.as_ref().unwrap();
    let loaded = SceneSpec::load_file(std::path::Path::new(path))
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
        if loaded.migrated { "［v1 草案，已按迁移规则解释］" } else { "" }
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
    let task = TaskKind::parse(&spec.task.id).expect("validated task id");
    let feat = if cfg!(feature = "cam") && args.cam {
        "rigid3d+cam"
    } else {
        "rigid3d"
    };
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║  OriEra rigid3d × LeRobot v2.1 · 具身批量采集与评测          ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!(
        "  任务 {} · {} 集 × ≤{} 步 · dt={:.4} · 种子 {}.. · 后端 {}",
        task.phrase(),
        args.episodes,
        spec.max_steps,
        spec.dt,
        args.seed0,
        feat
    );
    println!(
        "  spec {} · robot {} ({} DoF) · prop[0] {} {:?} · spec_hash {}",
        args.spec.as_deref().unwrap_or("<preset>"),
        spec.robot.name,
        spec.robot.dof(),
        spec.scene.props[0].name,
        spec.scene.props[0].shape,
        spec_hash
    );

    // The determinism receipt: collect twice, demand bit equality.
    let r1 = collect(&args, &spec).unwrap_or_else(|e| fail(&format!("采集失败：{e}")));
    let r2 = collect(&args, &spec).unwrap_or_else(|e| fail(&format!("采集失败：{e}")));
    assert_eq!(
        r1.hashes, r2.hashes,
        "#66+#67: episode fingerprints must match"
    );
    assert_eq!(r1.rows, r2.rows, "episode outcomes must be bit-identical");
    for (i, (e1, e2)) in r1.ds.episodes.iter().zip(r2.ds.episodes.iter()).enumerate() {
        assert_eq!(e1.rows, e2.rows, "dataset rows must be bit-equal (ep {i})");
        assert_eq!(
            e1.frames, e2.frames,
            "camera frames must be bit-equal (ep {i})"
        );
    }
    let successes = r1.rows.iter().filter(|(_, ok, _, _)| *ok).count();
    println!(
        "  ✓ 双跑逐位一致：{} 集 · {} transitions · 成功 {} 集（{}%）",
        args.episodes,
        r1.ds.total_frames(),
        successes,
        successes * 100 / args.episodes.max(1) as usize
    );

    // Solvability gate: ≥90% 硬闸（两级渐降相位机 + 肘限位 -2.5 后
    // 实测 pick 91.7% / reach 100%）；push 的接触基线允许不稳，如实
    // 报告不拦流程。闸失败 = 人话 + 下一步（G-2），不是 backtrace。
    if matches!(task, TaskKind::Reach | TaskKind::PickPlace) {
        let rate = successes as f64 / args.episodes.max(1) as f64;
        if rate < 0.9 {
            eprintln!(
                "\n【90% 闸未过】脚本基线只成功 {successes}/{} 集（{:.1}%），低于 90%，引擎拒绝出数据——这是引擎自检在工作，不是你弄坏了电脑。\n\
                 请按顺序排查：① 确认命令与 LAB-GUIDE §1 第 4 条逐字一致（集数改少会放大偶然失败）；\n\
                 ② `git status` 看是否改过引擎文件，有就 `git checkout -- .`；③ 确认没改参数/没改 spec（spec_hash {spec_hash}）后重跑；\n\
                 三步后仍失败，带着整段输出找助教。",
                args.episodes,
                rate * 100.0
            );
            std::process::exit(2);
        }
    }

    let root = std::path::Path::new(&args.out).join("lerobot-v21");
    std::fs::create_dir_all(&root).unwrap();
    let n = r1.ds.write_dir(&root).unwrap();
    let engine = concat!("oriera-engine/", env!("CARGO_PKG_VERSION"));
    LeRobotV21::write_receipt(
        &root,
        engine,
        feat,
        spec.dt,
        &spec_hash,
        &r1.hashes,
        successes as u32,
    )
    .unwrap();
    LeRobotV21::write_card(&root, task.phrase()).unwrap();
    // The resolved spec rides with the dataset — the file IS the experiment.
    std::fs::write(
        root.join("meta/oriera_spec.json"),
        serde_json::to_string_pretty(&spec).unwrap(),
    )
    .unwrap();

    // LeRobot v3.0 tree (P-7): lerobot ≥ 0.4 reads only this layout.
    let root_v3 = std::path::Path::new(&args.out).join("lerobot-v30");
    if args.lerobot_v3 {
        std::fs::create_dir_all(&root_v3).unwrap();
        r1.ds.write_dir_v3(&root_v3).unwrap();
        LeRobotV21::write_receipt(
            &root_v3,
            engine,
            feat,
            spec.dt,
            &spec_hash,
            &r1.hashes,
            successes as u32,
        )
        .unwrap();
        LeRobotV21::write_card_v3(&root_v3, task.phrase()).unwrap();
        std::fs::write(
            root_v3.join("meta/oriera_spec.json"),
            serde_json::to_string_pretty(&spec).unwrap(),
        )
        .unwrap();
    }

    // eval_report.json — the lab reporting format.
    let (lo, hi) = wilson95(successes, args.episodes as usize);
    let per_ep: Vec<String> = r1
        .rows
        .iter()
        .enumerate()
        .map(|(i, (seed, ok, steps, ret))| {
            let rand = r1.rand_receipts[i].replace('"', "'");
            format!(
                "{{\"seed\":{seed},\"success\":{ok},\"steps\":{steps},\"reward_sum\":{ret:.3},\"rand\":\"{rand}\"}}"
            )
        })
        .collect();
    let report = format!(
        "{{\"task\":\"{}\",\"task_id\":\"{}\",\"spec_hash\":\"{}\",\"engine\":\"oriera-engine/{}\",\"backend\":\"{}\",\"dt\":{},\"episodes\":{},\"successes\":{},\"success_rate\":{:.4},\"wilson95\":[{:.4},{:.4}],\"randomization\":\"{}\",\"runs\":[{}]}}\n",
        task.phrase(),
        spec.task.id,
        spec_hash,
        env!("CARGO_PKG_VERSION"),
        feat,
        spec.dt,
        args.episodes,
        successes,
        successes as f64 / args.episodes.max(1) as f64,
        lo,
        hi,
        r1.rand_receipts.first().map(|s| s.replace('"', "'")).unwrap_or_default(),
        per_ep.join(",")
    );
    let report_path = std::path::Path::new(&args.out).join("eval_report.json");
    std::fs::write(&report_path, &report).unwrap();

    println!(
        "  ✓ 评测：成功率 {:.1}%（Wilson 95% CI [{:.1}%, {:.1}%]）",
        successes as f64 / args.episodes.max(1) as f64 * 100.0,
        lo * 100.0,
        hi * 100.0
    );
    println!(
        "  ✓ 已写出 {}/（{n} 集）与 {}",
        root.display(),
        report_path.display()
    );
    if args.lerobot_v3 {
        println!(
            "  ✓ 已写出 {}/（LeRobot v3.0 布局；结构自检：python scripts/lerobot_v3_selfcheck.py {}）",
            root_v3.display(),
            root_v3.display()
        );
    }
    println!(
        "  下一步：python scripts/jsonl_to_lerobot_v21.py {}/ → lerobot-train 直读",
        root.display()
    );
}
