//! P-12 MuJoCo 动力学对比 v2 — Rust 侧探针（PLAN-embodied-general §四 M3
//! 「MuJoCo 动力学级对比」/ §9.1 P-12）。
//!
//! URDF → [`oriera_agent::urdf::UrdfReport::to_robot_spec`]（Panda 预设增益
//! kp/kd/armature = 300/30/0.05，与 `specs/official/panda-7dof-pick.json`
//! 同款；effort/damping/限位/惯量来自 URDF）→
//! [`oriera_subsys::MultibodySystem::from_robot_fixtures`]（**空家具列表**：
//! 无接触，纯铰接动力学，对齐 MuJoCo 侧关掉接触）→ 按目标文件逐 tick 写
//! `JointTargets` → 每 `stride` tick 记一次关节角（rad）与有限差分角速度，
//! JSON 打到 stdout。`scripts/mjcf_crosscheck.py` 的动力学节读它，与
//! MuJoCo 同目标序列同步长的 `qpos` 逐 tick 对比——**只测量不调参**。
//!
//! 用法：
//!
//! ```text
//! cargo run -p oriera-agent --example urdf_traj_dump --features urdf -- \
//!     <robot.urdf> <targets.json> [--stride 30]
//! ```
//!
//! `targets.json`：`{"ticks_per_waypoint": N, "waypoints": [[q1..q7], ...],
//! "kp": 300, "kd": 30, "armature": 0.05}`（后三项可省，缺省即 Panda 预设）。
//! 时间基 = 引擎默认：tick 1/60 s、内部 2 子步（物理 1/120 s），重力 9.8。
//! 零新依赖：serde_json 随 `urdf` 特性（亦是 dev-dependency）。
//!
//! 门控：无 `urdf` 特性时 `main` 只打印提示——example 由 cargo 自动发现
//! （无 `[[example]]` 登记、无 required-features），不带特性的 `cargo
//! test` 也能编译通过，不重蹈 PR #165「门控模块 E0433」的红腿。

#[cfg(not(feature = "urdf"))]
fn main() {
    eprintln!("urdf_traj_dump 需要 --features urdf（URDF 导入 + serde_json）");
    std::process::exit(2);
}

#[cfg(feature = "urdf")]
fn main() {
    use oriera_agent::urdf::{RobotSpecOpts, UrdfReport};
    use oriera_common::{FrameIndex, Grants, Timestamp};
    use oriera_core::{FrameCtx, System};
    use oriera_ecs::World;
    use oriera_subsys::{Gravity, JointTargets, JointTelemetry, MultibodySystem, TimeScale};

    const DT: f32 = 1.0 / 60.0;
    let fail = |msg: &str| -> ! {
        eprintln!("urdf_traj_dump：{msg}");
        std::process::exit(1)
    };

    let mut args = std::env::args().skip(1);
    let urdf_path = args
        .next()
        .unwrap_or_else(|| fail("缺参数：<robot.urdf> <targets.json>"));
    let targets_path = args
        .next()
        .unwrap_or_else(|| fail("缺参数：<targets.json>"));
    let mut stride: u64 = 30;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--stride" => {
                stride = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|v: &u64| *v > 0)
                    .unwrap_or_else(|| fail("--stride 需要正整数"))
            }
            other => fail(&format!("未知参数 {other:?}（只认 --stride N）")),
        }
    }

    let xml = std::fs::read_to_string(&urdf_path)
        .unwrap_or_else(|e| fail(&format!("读 {urdf_path} 失败：{e}")));
    let rep = UrdfReport::parse(&xml).unwrap_or_else(|e| fail(&format!("URDF 解析失败：{e}")));
    let tj: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&targets_path)
            .unwrap_or_else(|e| fail(&format!("读 {targets_path} 失败：{e}"))),
    )
    .unwrap_or_else(|e| fail(&format!("targets JSON 非法：{e}")));
    let num = |k: &str, d: f32| tj.get(k).and_then(|v| v.as_f64()).map_or(d, |v| v as f32);
    let per = tj
        .get("ticks_per_waypoint")
        .and_then(|v| v.as_u64())
        .filter(|v| *v > 0)
        .unwrap_or_else(|| fail("targets.ticks_per_waypoint 须为正整数"));
    let waypoints: Vec<Vec<f32>> = tj
        .get("waypoints")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| fail("targets.waypoints 须为数组"))
        .iter()
        .map(|w| {
            w.as_array()
                .unwrap_or_else(|| fail("waypoint 须为数组"))
                .iter()
                .map(|x| x.as_f64().unwrap_or_else(|| fail("waypoint 分量须为数")) as f32)
                .collect()
        })
        .collect();
    if waypoints.is_empty() {
        fail("targets.waypoints 为空");
    }

    let mut opts = RobotSpecOpts {
        default_kp: num("kp", 300.0),
        default_kd: num("kd", 30.0),
        default_armature: num("armature", 0.05),
        ..RobotSpecOpts::default()
    };
    let spec = rep
        .to_robot_spec(&mut opts)
        .unwrap_or_else(|e| fail(&format!("to_robot_spec 失败：{e}")));
    let n = spec.dof();
    if let Some(bad) = waypoints.iter().find(|w| w.len() != n) {
        fail(&format!("waypoint 长度 {} ≠ 关节数 {n}", bad.len()));
    }
    let names: Vec<String> = spec.joint_names().iter().map(|s| s.to_string()).collect();
    let joint_rows: Vec<serde_json::Value> = spec
        .joints
        .iter()
        .map(|j| {
            serde_json::json!({
                "name": j.name, "limits": [j.limits.0, j.limits.1], "effort": j.effort,
                "velocity_limit": j.velocity_limit, "kp": j.kp, "kd": j.kd,
                "armature": j.armature, "damping": j.damping,
            })
        })
        .collect();

    let mut sys = MultibodySystem::from_robot_fixtures(spec, &[])
        .unwrap_or_else(|e| fail(&format!("构建失败：{e}")));
    let mut world = World::new();
    world.insert_resource(Gravity(9.8));
    world.insert_resource(TimeScale(1.0));
    let arm = world.spawn();
    world.insert(
        arm,
        JointTargets {
            angles: waypoints[0].clone(),
            gripper: 0.0,
        },
    );
    world.insert(arm, JointTelemetry::default());

    let total = per * waypoints.len() as u64;
    let mut samples = vec![serde_json::json!({
        "tick": 0, "q": sys.angles(), "qd": vec![0.0f32; n],
    })];
    let started = std::time::Instant::now();
    for tick in 0..total {
        let w = &waypoints[(tick / per) as usize];
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.angles = w.clone();
        }
        sys.run(
            &FrameCtx {
                frame: FrameIndex(tick),
                dt: DT,
                now: Timestamp(0),
                grants: Grants::EMPTY,
            },
            &mut world,
        );
        let done = tick + 1;
        if done % stride == 0 {
            let tel = world
                .get::<JointTelemetry>(arm)
                .cloned()
                .unwrap_or_default();
            samples
                .push(serde_json::json!({ "tick": done, "q": tel.angles, "qd": tel.velocities }));
        }
    }
    let wall = started.elapsed().as_secs_f64();
    let out = serde_json::json!({
        "probe": "urdf_traj_dump",
        "robot": rep.name,
        "joint_names": names,
        "joints": joint_rows,
        "dt": DT,
        "substeps": 2,
        "gravity": 9.8,
        "fixtures": 0,
        "ticks": total,
        "ticks_per_waypoint": per,
        "stride": stride,
        "waypoints": waypoints,
        "initial_q": "build pose (all joints 0; limits outside 0 are pushed back by the joint-limit constraint)",
        "steps_per_s": total as f64 / wall.max(1e-9),
        "samples": samples,
    });
    println!("{}", serde_json::to_string(&out).expect("serialize"));
}
