//! Embodied 3D collection (PLAN-embodied-sim) — the lab batch runner.
//!
//! Drives [`TableTopEnv`] (L9 environment contract × L6 `rigid3d`
//! multibody backend) with analytic-IK scripted baselines across the
//! task family, records every step into a **LeRobotDataset v2.1** tree
//! (JSONL rows + camera PNG frames + full meta) and pins the
//! reproducibility claim: the whole collection runs TWICE and both
//! runs must be bit-identical — dataset, rewards and the per-episode
//! `sim_fingerprint` receipts (#66 + #67 at sim granularity).
//!
//! An `eval_report.json` lands next to the dataset: per-episode
//! outcome plus the success rate with a Wilson 95% interval — the
//! reporting format lab experiments are held to.
//!
//! Run:
//! `cargo run -p oriera-agent --example embodied_sim_collect --features rigid3d,cam -- --task pick_place --episodes 16 --cam`
//! Args: `--task reach|pick_place|push` · `--episodes N` · `--seed0 S`
//!       `--cam` / `--no-cam` · `--out DIR`
//! Output: `<out>/lerobot-v21/` + `<out>/eval_report.json`

use oriera_agent::embodied_env::{EmbodiedEnv, TableTopEnv, TaskKind};
use oriera_agent::lerobot::{CameraTrack, LeRobotV21};
use oriera_subsys::ArmDescription;

const DT: f32 = 1.0 / 60.0;

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

/// Analytic IK for the OriEra arm: base yaw + planar 2R (shoulder,
/// elbow) + wrist pitch aiming the tip straight down. Returns clamped
/// joint targets — the policy speaks target angles, the L6 PD motors
/// do the tracking.
fn ik(desc: &ArmDescription, target: [f32; 3]) -> [f32; 4] {
    let (l1, l2) = (desc.links[0], desc.links[1]);
    let shoulder_y = desc.base[1] + desc.shoulder_height;
    let dx = target[0] - desc.base[0];
    let dz = target[2] - desc.base[2];
    let yaw = f32::atan2(-dz, dx);
    let d = (dx * dx + dz * dz).sqrt().clamp(0.06, l1 + l2 - 0.005);
    // The 2R solves the WRIST position; the tip hangs l3 below it, so
    // aim the wrist l3 above the desired tip point.
    let h = (target[1] + desc.links[2] - shoulder_y).clamp(-0.38, 0.30);
    let cos_e = ((d * d + h * h - l1 * l1 - l2 * l2) / (2.0 * l1 * l2)).clamp(-1.0, 1.0);
    let elbow = -cos_e.acos();
    let shoulder = f32::atan2(h, d) - f32::atan2(l2 * elbow.sin(), l1 + l2 * elbow.cos());
    let wrist = -std::f32::consts::FRAC_PI_2 - shoulder - elbow;
    let clamp = |v: f32, i: usize| v.clamp(desc.limits[i].0, desc.limits[i].1);
    [
        clamp(yaw, 0),
        clamp(shoulder, 1),
        clamp(elbow, 2),
        clamp(wrist, 3),
    ]
}

/// The scripted baseline policy: pure function of the observation —
/// the same contract a trained policy would consume.
fn policy(obs: &[f32], phases: &[Phase], step: u64, desc: &ArmDescription) -> [f32; 5] {
    let ee = [obs[9], obs[10], obs[11]];
    let obj = [ee[0] + obs[12], ee[1] + obs[13], ee[2] + obs[14]];
    let goal = [ee[0] + obs[15], ee[1] + obs[16], ee[2] + obs[17]];
    let mut ticks = 0u64;
    for phase in phases {
        ticks += phase.ticks;
        if step < ticks {
            let mut a = ik(desc, (phase.target)(obj, goal)).to_vec();
            a.push(phase.gripper);
            return a.try_into().expect("5 action dims");
        }
    }
    // Past the script: hover above the goal, gripper open.
    let mut a = ik(desc, above(goal, 0.22)).to_vec();
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
    task: TaskKind,
    episodes: u32,
    seed0: u64,
    cam: bool,
    out: String,
}

fn parse_args() -> Args {
    let mut a = Args {
        task: TaskKind::PickPlace,
        episodes: 8,
        seed0: 1000,
        cam: true,
        out: "out/embodied-sim".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--task" => {
                let v = it.next().expect("--task 需要一个值");
                a.task = TaskKind::parse(&v).expect("task: reach|pick_place|push");
            }
            "--phrase" => {
                // 语言→具身最小桥：自然语言短语解析出任务（NLU→seed 组合
                // 由调用方继续），例如 --phrase "pick up the cube"。
                let v = it.next().expect("--phrase 需要一个值");
                a.task =
                    TaskKind::from_phrase(&v).unwrap_or_else(|| panic!("无法从短语解析任务：{v}"));
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
            other => {
                panic!("未知参数 {other}（可用 --task/--episodes/--seed0/--cam/--no-cam/--out）")
            }
        }
    }
    a
}

struct RunResult {
    ds: LeRobotV21,
    /// (seed, success, steps, reward_sum) per episode.
    rows: Vec<(u64, bool, u64, f32)>,
    hashes: Vec<u64>,
}

fn collect(args: &Args) -> Result<RunResult, String> {
    let desc = ArmDescription::default();
    let mut env = TableTopEnv::new();
    env.task = args.task;
    #[cfg(feature = "cam")]
    if args.cam {
        env.set_camera(Some(oriera_subsys::camera::CameraView::default()));
    }
    let mut ds = LeRobotV21::new(
        (1.0 / DT).round(),
        "oriera-arm-6",
        env.obs_names(),
        env.action_names(),
    );
    #[cfg(feature = "cam")]
    if args.cam {
        ds.set_camera(CameraTrack {
            name: "top".into(),
            w: 168,
            h: 168,
            fps: (1.0 / DT).round(), // 与控制率同频（60Hz 双轨错位修复）
        });
    }

    let phases = match args.task {
        TaskKind::Reach => reach_phases(),
        TaskKind::PickPlace => pick_place_phases(),
        TaskKind::Push => push_phases(),
    };

    let mut rows = Vec::new();
    let mut hashes = Vec::new();
    for ep in 0..args.episodes {
        let seed = args.seed0 + ep as u64;
        let mut obs = env.reset(seed);
        // episode 语言标注 = 任务自然语言短语（SmolVLA/π0 语言条件）
        ds.start_episode(env.task.phrase());
        let mut step = 0u64;
        let mut terminated = false;
        let mut reward_sum = 0.0f32;
        let max_steps = env.spec().max_steps;
        while step < max_steps && !terminated {
            let action = policy(&obs, &phases, step, &desc);
            // LeRobot row t = (state_t, action_t); the env's next obs
            // becomes the next row's state.
            ds.push_step(&obs, &action)?;
            #[cfg(feature = "cam")]
            for (_ft, png) in env.take_frames() {
                ds.push_frame(png)?;
            }
            let out = env.step(&action);
            obs = out.obs;
            reward_sum += out.reward;
            terminated = out.terminated;
            step += 1;
        }
        hashes.push(env.fingerprint());
        rows.push((seed, terminated, step, reward_sum));
        ds.finish_episode();
    }
    Ok(RunResult { ds, rows, hashes })
}

fn main() {
    let args = parse_args();
    let feat = if cfg!(feature = "cam") && args.cam {
        "rigid3d+cam"
    } else {
        "rigid3d"
    };
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║  OriEra rigid3d × LeRobot v2.1 · 具身批量采集与评测          ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!(
        "  任务 {} · {} 集 × ≤700 步 · dt={:.4} · 种子 {}.. · 后端 {}",
        args.task.phrase(),
        args.episodes,
        DT,
        args.seed0,
        feat
    );

    // The determinism receipt: collect twice, demand bit equality.
    let r1 = collect(&args).expect("采集失败");
    let r2 = collect(&args).expect("采集失败");
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

    // Solvability gate: ≥50% 是「任务可解」的硬闸（push 的接触基线允许
    // 不稳，如实报告）；≥90% 的优秀线当前基线未达标（reach/pick ≈62-66%，
    // IK 伺服打磨排期中）——打警告不拦流程。
    if matches!(args.task, TaskKind::Reach | TaskKind::PickPlace) {
        assert!(
            successes * 2 >= args.episodes as usize,
            "scripted baseline must solve at least half the seeds (got {successes}/{})",
            args.episodes
        );
        let rate = successes as f64 / args.episodes.max(1) as f64;
        if rate < 0.9 {
            println!(
                "  ⚠ 基线成功率 {:.0}% < 90% 优秀线（IK 伺服打磨待办，报告如实携带）",
                rate * 100.0
            );
        }
    }

    let root = std::path::Path::new(&args.out).join("lerobot-v21");
    std::fs::create_dir_all(&root).unwrap();
    let n = r1.ds.write_dir(&root).unwrap();
    LeRobotV21::write_receipt(
        &root,
        concat!("oriera-engine/", env!("CARGO_PKG_VERSION")),
        feat,
        DT,
        &r1.hashes,
        successes as u32,
    )
    .unwrap();
    LeRobotV21::write_card(&root, args.task.phrase()).unwrap();

    // eval_report.json — the lab reporting format.
    let (lo, hi) = wilson95(successes, args.episodes as usize);
    let per_ep: Vec<String> = r1
        .rows
        .iter()
        .map(|(seed, ok, steps, ret)| {
            format!(
                "{{\"seed\":{seed},\"success\":{ok},\"steps\":{steps},\"reward_sum\":{ret:.3}}}"
            )
        })
        .collect();
    let report = format!(
        "{{\"task\":\"{}\",\"engine\":\"oriera-engine/{}\",\"backend\":\"{}\",\"dt\":{},\"episodes\":{},\"successes\":{},\"success_rate\":{:.4},\"wilson95\":[{:.4},{:.4}],\"randomization\":\"cube_density 240-640 kg/m3, cube_friction 0.5-1.1, placements\",\"runs\":[{}]}}\n",
        args.task.phrase(),
        env!("CARGO_PKG_VERSION"),
        feat,
        DT,
        args.episodes,
        successes,
        successes as f64 / args.episodes.max(1) as f64,
        lo,
        hi,
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
    println!(
        "  下一步：python scripts/jsonl_to_lerobot_v21.py {}/ → lerobot-train 直读",
        root.display()
    );
}
