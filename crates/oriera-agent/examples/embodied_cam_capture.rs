//! Camera capture harness (M3-A, PLAN-embodied-general §四 M3 / §9.1
//! U-5) — the "calibration board" of the synthetic cameras.
//!
//! Loads a spec carrying `scene.cameras` (the shipped demo:
//! `specs/lab/wrist-cam-demo.json` — an EE-mounted wrist camera
//! 160×120 RGB+depth with mild lens distortion, plus a 320×240
//! overhead), collects scripted pick-and-place episodes and exports a
//! **multi-video-track LeRobot dataset**: one
//! `observation.images.{name}` per camera plus one `depth.{name}`
//! u16-PNG track per depth-enabled camera.
//!
//! **Calibration self-check** (the renderer's quantified gate): three
//! world points with known coordinates — the gripper tip, the cube
//! centre, the goal centre — are pushed through 「外参 × 内参 × 畸变」
//! (`PinholeCam::project`) and compared against the rendered pixels of
//! the same state: the pixel nearest to the projection must carry that
//! object's colour and its z-buffer depth must match the point's
//! camera-frame depth. Gate: reprojection error **< 2 px** for every
//! (camera × point) that was ever in frame — the same idea as
//! projecting a physical calibration board, staged in simulation.
//!
//! Run:
//! `cargo run -p oriera-agent --example embodied_cam_capture --features rigid3d,cam,spec -- \
//!    --spec crates/oriera-embodied/specs/lab/wrist-cam-demo.json --episodes 2`
//! Args: `--spec FILE` · `--episodes N` · `--seed0 S` · `--out DIR`
//!       `--verify-determinism full|off`
//! Output: `<out>/lerobot-v21/` + `<out>/lerobot-v30/` +
//!         `<out>/eval_report.json` + `<out>/run_manifest.json`

use oriera_agent::embodied_env::{CameraShot, EmbodiedEnv, ObsLayout, TableTopEnv};
use oriera_agent::lerobot::{CameraTrack, LeRobotV21};
use oriera_agent::manifest::{write_run_manifest_opts, ManifestExtra};
use oriera_agent::spec::SceneSpec;
use oriera_subsys::RobotSpec;

/// The exact colours `TableTopEnv::scene_view` paints the three
/// calibration targets with — the render-side contract this check
/// pins (a colour change here is a renderer change and must move the
/// gate deliberately).
const TIP_COLOR: [u8; 3] = [91, 141, 239];
const CUBE_COLOR: [u8; 3] = [255, 206, 110];
const GOAL_COLOR: [u8; 3] = [64, 200, 132];

struct Args {
    spec: String,
    episodes: u32,
    seed0: u64,
    out: String,
    verify: bool,
    sanitize: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        spec: "crates/oriera-embodied/specs/lab/wrist-cam-demo.json".into(),
        episodes: 2,
        seed0: 1000,
        out: "out/embodied-cam".into(),
        verify: true,
        sanitize: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--spec" => a.spec = it.next().expect("--spec 需要一个文件路径"),
            "--episodes" => {
                a.episodes = it
                    .next()
                    .expect("--episodes 需要一个值")
                    .parse()
                    .expect("episodes: 数字")
            }
            "--seed0" | "--seed" => {
                a.seed0 = it.next().expect("需要值").parse().expect("seed: 数字")
            }
            "--out" => a.out = it.next().expect("--out 需要一个值"),
            "--verify-determinism" => {
                a.verify = match it.next().expect("--verify-determinism 需要一个值").as_str() {
                    "full" => true,
                    "off" => false,
                    other => fail(&format!(
                        "--verify-determinism {other:?}：须为 full（双跑逐位，默认）或 off"
                    )),
                }
            }
            other => fail(&format!(
                "未知参数 {other}（可用 --spec/--episodes/--seed0/--out/--verify-determinism）"
            )),
        }
    }
    a
}

fn fail(msg: &str) -> ! {
    eprintln!("错误：{msg}");
    std::process::exit(1)
}

// ---------------------------------------------------------------------------
// Scripted pick baseline — the same 4-DoF analytic IK the collect
// harness uses (examples are standalone; the DLS registry baseline is
// the M2 general route, this file only needs the demo arm to grip).
// ---------------------------------------------------------------------------

struct Phase {
    gripper: f32,
    ticks: u64,
    target: fn(obj: [f32; 3], goal: [f32; 3]) -> [f32; 3],
}

fn above(p: [f32; 3], h: f32) -> [f32; 3] {
    [p[0], p[1] + h, p[2]]
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

struct IkArm {
    base: [f32; 3],
    shoulder_y: f32,
    links: [f32; 3],
    limits: [(f32, f32); 4],
}

impl IkArm {
    fn from_robot(r: &RobotSpec) -> Result<Self, String> {
        if r.dof() != 4 {
            return Err(format!(
                "本示例的脚本基线只认 4-DoF 桌面臂预设（当前 {} 个关节）；wrist-cam-demo 继承金标即为 4-DoF",
                r.dof()
            ));
        }
        let g = r
            .gripper
            .as_ref()
            .ok_or("脚本基线需要 gripper（tcp_offset 定义指尖）")?;
        Ok(Self {
            base: r.base,
            shoulder_y: r.links[r.joints[1].child_link].origin[1],
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

fn ik(arm: &IkArm, target: [f32; 3]) -> [f32; 4] {
    let (l1, l2) = (arm.links[0], arm.links[1]);
    let dx = target[0] - arm.base[0];
    let dz = target[2] - arm.base[2];
    let yaw = f32::atan2(-dz, dx);
    let d = (dx * dx + dz * dz).sqrt().clamp(0.06, l1 + l2 - 0.005);
    let h = (target[1] + arm.links[2] - arm.shoulder_y).clamp(-0.38, 0.30);
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
                .unwrap_or_else(|| panic!("obs layout 缺少 {n}"))
        };
        Self {
            ee: find("ee.x"),
            obj_rel: find("obj_rel.x"),
            goal_rel: find("goal_rel.x"),
        }
    }
}

fn policy(obs: &[f32], idx: &ObsIdx, phases: &[Phase], step: u64, arm: &IkArm) -> [f32; 5] {
    let ee = [obs[idx.ee], obs[idx.ee + 1], obs[idx.ee + 2]];
    let (o, g) = (idx.obj_rel, idx.goal_rel);
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
    let mut a = ik(arm, above(goal, 0.22)).to_vec();
    a.push(0.0);
    a.try_into().expect("5 action dims")
}

// ---------------------------------------------------------------------------
// The calibration self-check (U-5 标定板思路仿真版)
// ---------------------------------------------------------------------------

/// One calibration target: a world point, the colour its surface
/// renders with, its name, and whether the z-buffer depth at the match
/// must agree with the point's own camera-frame depth. Surface targets
/// (cube / goal) cross-check depth; the tip rides the marker pass (its
/// beacon paints over its own capsule, so the pixel's depth is the
/// capsule, not the beacon) and skips it.
struct Target {
    name: &'static str,
    color: [u8; 3],
    depth_check: bool,
}

/// Squared-residual budget of the shade-invariant colour match: the
/// rasterizer's flat shading multiplies a surface's colour by ONE
/// scalar (head-light · face angle), so the check fits `pixel ≈ a·target`
/// per pixel and accepts residuals within ~±3.5 units per channel.
/// The four scene colours are tens of units apart in this metric, so
/// the match stays specific while any tilt/angle of the surface passes.
const SHADE_RESIDUAL2: f32 = 40.0;

/// Shade-invariant colour match: best scalar `a` minimizing
/// Σ(pixel − a·target)², accepted when the residual is small. A plain
/// per-channel tolerance fails here by construction — a face tilted
/// away from the head-light shifts ALL channels by up to 55%.
fn matches_target(pixel: [u8; 3], target: [u8; 3]) -> bool {
    let p = [pixel[0] as f32, pixel[1] as f32, pixel[2] as f32];
    let t = [target[0] as f32, target[1] as f32, target[2] as f32];
    let a = (p[0] * t[0] + p[1] * t[1] + p[2] * t[2]) / (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]);
    let e0 = p[0] - a * t[0];
    let e1 = p[1] - a * t[1];
    let e2 = p[2] - a * t[2];
    e0 * e0 + e1 * e1 + e2 * e2 <= SHADE_RESIDUAL2
}

/// Best sighting of `target` near `proj` in the rendered frame:
/// nearest shade-invariant colour-matched pixel whose z-buffer depth
/// agrees with the point's camera-frame depth (the surface the point
/// belongs to, not a foreground blotch). Returns (error px, depth mm)
/// of the match.
fn sight(
    rgb: &[u8],
    depth: Option<&[u16]>,
    w: u32,
    h: u32,
    proj: (f32, f32, f32),
    color: [u8; 3],
) -> Option<(f32, f32)> {
    let (u, v, z) = proj;
    let (px, py) = (u.round() as i32, v.round() as i32);
    let (w_i, h_i) = (w as i32, h as i32);
    let at = |x: i32, y: i32| -> [u8; 3] {
        let i = (y as usize * w as usize + x as usize) * 3;
        [rgb[i], rgb[i + 1], rgb[i + 2]]
    };
    let mut best: Option<(f32, f32)> = None;
    for dy in -4i32..=4 {
        for dx in -4i32..=4 {
            let (x, y) = (px + dx, py + dy);
            if x < 0 || y < 0 || x >= w_i || y >= h_i {
                continue;
            }
            if !matches_target(at(x, y), color) {
                continue;
            }
            if let Some(dbuf) = depth {
                let mm = dbuf[y as usize * w as usize + x as usize] as f32;
                // surface, not the interior point: within the target's
                // own extent (radius ≤ 4.5 cm) of the projected depth
                if (mm - z * 1000.0).abs() > 45.0 {
                    continue;
                }
            }
            let err = ((dx * dx + dy * dy) as f32).sqrt();
            if best.is_none_or(|(be, _)| err < be) {
                best = Some((err, z * 1000.0));
            }
        }
    }
    best
}

/// One (camera × point) sighting record across a run: the best (min)
/// reprojection error and the tick it was seen at.
#[derive(Debug, Clone, Copy)]
struct Sighting {
    err_px: f32,
    tick: u64,
}

impl Sighting {
    fn better(&mut self, err: f32, tick: u64) {
        if err < self.err_px {
            self.err_px = err;
            self.tick = tick;
        }
    }
}

fn main() {
    let args = parse_args();
    let loaded = SceneSpec::load_file(std::path::Path::new(&args.spec))
        .unwrap_or_else(|e| fail(&format!("spec 加载失败：{e}")));
    let spec = loaded.spec;
    spec.validate()
        .unwrap_or_else(|e| fail(&format!("spec 校验失败：{e}")));
    if spec.scene.cameras.is_empty() {
        fail(&format!(
            "{} 没有声明 scene.cameras——本示例采集标定相机轨（示例 spec：crates/oriera-embodied/specs/lab/wrist-cam-demo.json）",
            args.spec
        ));
    }
    let spec_hash = spec.spec_hash_hex();

    let mut env: TableTopEnv = spec.to_env();
    env.set_cameras(spec.scene.cameras.clone());
    let arm = IkArm::from_robot(&env.robot).unwrap_or_else(|e| fail(&e));
    let idx = ObsIdx::from_layout(env.obs_layout());
    let phases = pick_place_phases();

    let mut ds = LeRobotV21::from_layout(
        (1.0 / env.dt).round(),
        &env.robot.name,
        env.obs_layout(),
        env.action_layout(),
    );
    for c in &spec.scene.cameras {
        let p = c.pinhole();
        // rgb=false ⇒ 深度-only 相机：只声明 depth.{name}（P2-W1-D）
        ds.add_camera_channels(
            CameraTrack {
                name: c.name.clone(),
                w: p.width,
                h: p.height,
                fps: (1.0 / env.dt).round(),
                depth: p.channels.depth,
            },
            p.channels.rgb,
        )
        .unwrap_or_else(|e| fail(&e));
    }

    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║  OriEra M3-A 相机采集 × 标定自检（腕相机 + 俯拍 × RGB/depth）║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!(
        "  spec {} · robot {} · 相机 {} · spec_hash {}",
        args.spec,
        env.robot.name,
        spec.scene
            .cameras
            .iter()
            .map(|c| {
                let p = c.pinhole();
                format!(
                    "{}({}×{}{}{})",
                    c.name,
                    p.width,
                    p.height,
                    if p.channels.rgb { "+rgb" } else { "" },
                    if p.channels.depth { "+depth" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join(" "),
        spec_hash
    );

    // (camera × point) → best sighting over the whole run
    let points = [
        Target {
            name: "tip",
            color: TIP_COLOR,
            depth_check: false,
        },
        Target {
            name: "cube",
            color: CUBE_COLOR,
            depth_check: true,
        },
        Target {
            name: "goal",
            color: GOAL_COLOR,
            depth_check: true,
        },
    ];
    let mut best: Vec<[Option<Sighting>; 3]> =
        spec.scene.cameras.iter().map(|_| [None; 3]).collect();
    let mut last_shot: Option<CameraShot> = None;
    let mut last_wrist: Option<CameraShot> = None;
    let mut total_steps = 0u64;

    let t0 = std::time::Instant::now();
    // (seed, success, steps, fingerprint)
    let mut episode_rows: Vec<(u64, bool, u64, u64)> = Vec::new();
    let mut seed = args.seed0;
    let mut kept = 0u32;
    while kept < args.episodes {
        let mut obs = env.reset(seed);
        ds.start_episode(env.task.phrase());
        let mut step = 0u64;
        let mut terminated = false;
        let max_steps = env.spec().max_steps;
        let mut grip_checked = false;
        while step < max_steps && !terminated {
            let action = policy(&obs, &idx, &phases, step, &arm);
            let out = env.step(&action);
            obs = out.obs;
            ds.push_step(&obs, &action).unwrap_or_else(|e| fail(&e));
            for (_ft, frame) in env.take_frames() {
                ds.push_frame(frame).unwrap_or_else(|e| fail(&e));
            }
            // calibration self-check: at the grip moment and every 5th
            // tick (min error over the run absorbs transient occlusion)
            if (env.holding() && !grip_checked) || step.is_multiple_of(5) {
                grip_checked = grip_checked || env.holding();
                let tip = [obs[idx.ee], obs[idx.ee + 1], obs[idx.ee + 2]];
                let (o, g) = (idx.obj_rel, idx.goal_rel);
                let cube = [tip[0] + obs[o], tip[1] + obs[o + 1], tip[2] + obs[o + 2]];
                let goal = [tip[0] + obs[g], tip[1] + obs[g + 1], tip[2] + obs[g + 2]];
                let world = [tip, cube, goal];
                for (ci, shot) in env.render_cameras().into_iter().enumerate() {
                    let is_wrist = shot.cam == "wrist";
                    let rgb = shot.rgb.as_deref().unwrap_or(&[]);
                    let depth = shot.depth.as_deref();
                    for (pi, tgt) in points.iter().enumerate() {
                        let Some((u, v, z)) = shot.pinhole.project(world[pi]) else {
                            continue; // behind the near plane
                        };
                        if u < -3.0
                            || v < -3.0
                            || u >= shot.w as f32 + 3.0
                            || v >= shot.h as f32 + 3.0
                        {
                            continue; // out of frame
                        }
                        if rgb.is_empty() {
                            continue; // depth-only camera: no colour probe
                        }
                        if let Some((err, _mm)) = sight(
                            rgb,
                            if tgt.depth_check { depth } else { None },
                            shot.w,
                            shot.h,
                            (u, v, z),
                            tgt.color,
                        ) {
                            let slot = &mut best[ci][pi];
                            if let Some(s) = slot {
                                s.better(err, step);
                            } else {
                                *slot = Some(Sighting {
                                    err_px: err,
                                    tick: step,
                                });
                            }
                        }
                    }
                    if is_wrist {
                        last_wrist = Some(shot);
                    } else {
                        last_shot = Some(shot);
                    }
                }
            }
            terminated = out.terminated;
            step += 1;
        }
        if step < 5 {
            // degenerate draw (spawn-in-goal ends at ~1 step — not a
            // dataset entry): advance the seed and redraw, the same
            // policy as the collect harness's eval protocol
            ds.drop_episode();
            println!("  · seed {seed} 生成即入区（{step} 步），已剔除重抽");
            seed += 1;
            continue;
        }
        let fp = env.fingerprint();
        ds.finish_episode();
        let ok = terminated;
        episode_rows.push((seed, ok, step, fp));
        total_steps += step;
        kept += 1;
        println!(
            "  ✓ 集 {kept}（seed {seed}）：{step} 步 · {} · 指纹 {fp}",
            if ok { "成功" } else { "未过终判" },
        );
        seed += 1;
    }
    let secs = t0.elapsed().as_secs_f64();

    // ---- the calibration gate report (3 points × N cameras) --------
    println!("\n  ── 标定自检（外参×内参×畸变 → 渲染像素，门槛 < 2 px）──");
    let mut gate_ok = true;
    for (ci, cam) in spec.scene.cameras.iter().enumerate() {
        for (pi, tgt) in points.iter().enumerate() {
            let cell = best[ci][pi];
            match cell {
                Some(s) if s.err_px < 2.0 => println!(
                    "    {:>6} × {:<4}  重投影误差 {:.2}px（tick {}）✓",
                    cam.name, tgt.name, s.err_px, s.tick
                ),
                Some(s) => {
                    gate_ok = false;
                    println!(
                        "    {:>6} × {:<4}  重投影误差 {:.2}px（tick {}）✗ 超 2px 门槛",
                        cam.name, tgt.name, s.err_px, s.tick
                    );
                }
                None => println!(
                    "    {:>6} × {:<4}  整局未见（出画或持续遮挡）——不判",
                    cam.name, tgt.name
                ),
            }
        }
    }
    if !gate_ok {
        fail("标定自检未过 2px 门槛：渲染器与投影不自洽（上表 ✗ 行）");
    }

    // ---- write the multi-video dataset ------------------------------
    let out = std::path::Path::new(&args.out);
    std::fs::create_dir_all(out).unwrap();
    write_run_manifest_opts(
        out.join("run_manifest.json"),
        &spec,
        &ManifestExtra {
            protocol_id: "camera-capture@v1".into(),
            protocol_hash: String::new(),
            method: "scripted-cam".into(),
            seed: Some(args.seed0),
        },
        args.sanitize,
    )
    .unwrap_or_else(|e| fail(&format!("run_manifest.json 写入失败：{e}")));

    let root = out.join("lerobot-v21");
    let n = ds.write_dir(&root).unwrap();
    let engine = concat!("oriera-engine/", env!("CARGO_PKG_VERSION"));
    let hashes: Vec<u64> = episode_rows.iter().map(|(_, _, _, fp)| *fp).collect();
    let fps = oriera_agent::eval::fingerprint_set(&spec.robot, spec.spec_hash(), &hashes);
    oriera_agent::lerobot::LeRobotV21::write_receipt(
        &root,
        engine,
        "rigid3d+cam+spec-cameras",
        spec.dt,
        &spec_hash,
        &hashes,
        episode_rows.iter().filter(|(_, ok, _, _)| *ok).count() as u32,
        &fps.fingerprint_scheme,
        &fps.episode_fingerprints_v2,
    )
    .unwrap();
    oriera_agent::lerobot::LeRobotV21::write_card(&root, env.task.phrase()).unwrap();
    if !args.sanitize {
        std::fs::write(
            root.join("meta/oriera_spec.json"),
            serde_json::to_string_pretty(&spec).unwrap(),
        )
        .unwrap();
    }

    let root_v3 = out.join("lerobot-v30");
    ds.write_dir_v3(&root_v3).unwrap();
    oriera_agent::lerobot::LeRobotV21::write_card_v3(&root_v3, env.task.phrase()).unwrap();
    if !args.sanitize {
        std::fs::write(
            root_v3.join("meta/oriera_spec.json"),
            serde_json::to_string_pretty(&spec).unwrap(),
        )
        .unwrap();
    }

    // ---- pixel statistics of one frame (the acceptance probe: the
    // wrist camera's RGB+depth when present, else the last shot) ------
    let stats_shot = last_wrist.as_ref().or(last_shot.as_ref());
    if let Some(shot) = stats_shot {
        println!(
            "\n  ── 像素统计（最后自检帧：{} {}×{}）──",
            shot.cam, shot.w, shot.h
        );
        if let Some(rgb) = &shot.rgb {
            let mut stats = [[0usize; 256]; 3];
            for px in rgb.as_chunks::<3>().0 {
                for c in 0..3 {
                    stats[c][px[c] as usize] += 1;
                }
            }
            for (c, name) in ["R", "G", "B"].iter().enumerate() {
                let n = rgb.len() / 3;
                let sum: usize = stats[c].iter().enumerate().map(|(v, k)| v * k).sum();
                let (min, max) = (
                    stats[c].iter().position(|k| *k > 0).unwrap_or(0),
                    stats[c].iter().rposition(|k| *k > 0).unwrap_or(255),
                );
                println!(
                    "    {name}: mean {:.1} · min {min} · max {max}",
                    sum as f64 / n as f64
                );
            }
        }
        if let Some(depth) = &shot.depth {
            let hit: Vec<u32> = depth
                .iter()
                .filter(|d| **d > 0)
                .map(|d| *d as u32)
                .collect();
            if hit.is_empty() {
                println!("    depth: 全空（无回波）");
            } else {
                let sum: u64 = hit.iter().map(|d| *d as u64).sum();
                println!(
                    "    depth: min {} mm · max {} mm · mean {:.0} mm · 命中 {}/{} px",
                    hit.iter().min().unwrap(),
                    hit.iter().max().unwrap(),
                    sum as f64 / hit.len() as f64,
                    hit.len(),
                    depth.len()
                );
            }
        }
    }

    // ---- determinism re-run (full): episode 0 twice, bit-equal ------
    if args.verify {
        let mut env2: TableTopEnv = spec.to_env();
        env2.set_cameras(spec.scene.cameras.clone());
        let mut ds2 = LeRobotV21::from_layout(
            (1.0 / env2.dt).round(),
            &env2.robot.name,
            env2.obs_layout(),
            env2.action_layout(),
        );
        for c in &spec.scene.cameras {
            let p = c.pinhole();
            ds2.add_camera_channels(
                CameraTrack {
                    name: c.name.clone(),
                    w: p.width,
                    h: p.height,
                    fps: (1.0 / env2.dt).round(),
                    depth: p.channels.depth,
                },
                p.channels.rgb,
            )
            .unwrap();
        }
        let arm2 = IkArm::from_robot(&env2.robot).unwrap();
        let mut obs = env2.reset(args.seed0);
        ds2.start_episode(env2.task.phrase());
        let mut step = 0u64;
        while step < env2.spec().max_steps {
            let action = policy(&obs, &idx, &phases, step, &arm2);
            let out = env2.step(&action);
            obs = out.obs;
            ds2.push_step(&obs, &action).unwrap();
            for (_ft, frame) in env2.take_frames() {
                ds2.push_frame(frame).unwrap();
            }
            if out.terminated {
                break;
            }
            step += 1;
        }
        ds2.finish_episode();
        let a = ds.episodes.first().expect("at least one episode");
        let b = ds2.episodes.first().expect("re-run");
        assert_eq!(a.rows, b.rows, "rows must be bit-equal");
        assert_eq!(a.tracks, b.tracks, "camera frames must be bit-equal");
        println!("  ✓ 确定性复核（full）：首集双跑 rows+相机帧逐位一致");
    }

    // eval_report.json — the run's outcomes + the calibration table
    let successes = episode_rows.iter().filter(|(_, ok, _, _)| *ok).count();
    let report = serde_json::json!({
        "task": env.task.phrase(),
        "task_id": spec.task.id,
        "protocol": {"id": "camera-capture@v1", "episodes": args.episodes, "seed0": args.seed0},
        "spec_hash": spec_hash,
        "engine": engine,
        "backend": "rigid3d+cam+spec-cameras",
        "cameras": spec.scene.cameras.iter().map(|c| {
            let p = c.pinhole();
            serde_json::json!({
                "name": c.name,
                "width": p.width, "height": p.height,
                "fx": p.fx, "fy": p.fy, "cx": p.cx, "cy": p.cy,
                "dist": {"k1": p.dist.k1, "k2": p.dist.k2, "p1": p.dist.p1, "p2": p.dist.p2},
                "attach": format!("{:?}", p.attach),
                "rgb": p.channels.rgb, "depth": p.channels.depth,
            })
        }).collect::<Vec<_>>(),
        "episodes": episode_rows.len(),
        "successes": successes,
        "calibration": {
            "gate_px": 2.0,
            "targets": ["tip", "cube", "goal"],
            "rows": spec.scene.cameras.iter().enumerate().map(|(ci, cam)| {
                serde_json::json!({
                    "camera": cam.name,
                    "tip": best[ci][0].map(|s| s.err_px),
                    "cube": best[ci][1].map(|s| s.err_px),
                    "goal": best[ci][2].map(|s| s.err_px),
                })
            }).collect::<Vec<_>>(),
        },
        "throughput_steps_per_s": total_steps as f64 / secs.max(1e-9),
        "verify_determinism": if args.verify { "full" } else { "off" },
        "runs": episode_rows.iter().map(|(seed, ok, steps, fp)| serde_json::json!({
            "seed": seed, "success": ok, "steps": steps, "fingerprint": fp,
        })).collect::<Vec<_>>(),
    });
    let report_path = out.join("eval_report.json");
    std::fs::write(
        &report_path,
        format!("{}\n", serde_json::to_string(&report).unwrap()),
    )
    .unwrap();

    println!(
        "\n  ✓ 已写出 {}/ 与 {}/（{n} 集 · 多视频轨）",
        root.display(),
        root_v3.display()
    );
    println!(
        "  ✓ {} 与 {}",
        report_path.display(),
        out.join("run_manifest.json").display()
    );
    println!(
        "  ✓ 吞吐（RGB+depth 逐 tick 渲染）：{:.0} steps/s（{total_steps} 步 / {secs:.1}s）",
        total_steps as f64 / secs.max(1e-9)
    );
    println!(
        "  下一步：python scripts/jsonl_to_lerobot_v21.py {}/",
        root_v3.display()
    );
}
