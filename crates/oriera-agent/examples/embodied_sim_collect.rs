//! Embodied 3D collection (PLAN-embodied-sim) — the world-as-benchmark
//! loop, now with a real arm.
//!
//! Drives [`PickPlace3D`] (L9 environment contract × L6 `rigid3d`
//! multibody backend) with an analytic-IK scripted baseline policy,
//! records every step into a **LeRobotDataset v2.1** tree (JSONL rows +
//! full meta; `scripts/jsonl_to_lerobot_v21.py` converts to parquet)
//! and pins the reproducibility claim: the whole collection runs TWICE
//! and both runs must be bit-identical — dataset, rewards and the
//! per-episode `sim_fingerprint` receipts (#66 + #67 at sim
//! granularity).
//!
//! Run: `cargo run -p oriera-agent --example embodied_sim_collect --features rigid3d`
//! Output: `out/embodied-sim/lerobot-v21/`

use oriera_agent::embodied_env::{EmbodiedEnv, PickPlace3D, TASK_PHRASE};
use oriera_agent::lerobot::LeRobotV21;
use oriera_subsys::ArmDescription;

const EPISODES: u32 = 8;
const MAX_STEPS: u64 = 700;
const DT: f32 = 1.0 / 60.0;
const FIRST_SEED: u64 = 1000;

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

/// One phase of the scripted baseline: an IK waypoint lifted from the
/// observation, a gripper demand, and a dwell long enough for the PD
/// motors to converge.
struct Phase {
    gripper: f32,
    ticks: u64,
    target: fn(obj: [f32; 3], goal: [f32; 3]) -> [f32; 3],
}

const PHASES: &[Phase] = &[
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
];

/// Hover `h` above point `p` (relative — h is a lift height, not a
/// world coordinate).
fn above(p: [f32; 3], h: f32) -> [f32; 3] {
    [p[0], p[1] + h, p[2]]
}

/// The scripted baseline policy: pure function of the observation —
/// the same contract a trained policy would consume.
fn policy(obs: &[f32], desc: &ArmDescription, step: u64) -> [f32; 5] {
    let ee = [obs[9], obs[10], obs[11]];
    let obj = [ee[0] + obs[12], ee[1] + obs[13], ee[2] + obs[14]];
    let goal = [ee[0] + obs[15], ee[1] + obs[16], ee[2] + obs[17]];
    let mut ticks = 0u64;
    for phase in PHASES {
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

/// One full collection run. Returns everything the determinism assert
/// needs: row-count, per-episode receipts, successes.
fn collect() -> (LeRobotV21, Vec<u64>, u32) {
    let desc = ArmDescription::default();
    let mut ds = LeRobotV21::new(
        1.0 / DT,
        "oriera-arm-6",
        (0..21).map(|i| format!("s{i}")).collect(),
        (0..5).map(|i| format!("a{i}")).collect(),
    );
    // Real column names come from the env spec.
    let mut env = PickPlace3D::new();
    ds.obs_names = env.obs_names();
    ds.action_names = env.action_names();

    let mut hashes = Vec::new();
    let mut successes = 0;
    for ep in 0..EPISODES {
        let mut obs = env.reset(FIRST_SEED + ep as u64);
        ds.start_episode(TASK_PHRASE);
        let mut step = 0u64;
        let mut terminated = false;
        while step < MAX_STEPS && !terminated {
            let action = policy(&obs, &desc, step);
            // LeRobot row t = (state_t, action_t); out.obs is state_{t+1}
            // and becomes the next row's state.
            ds.push_step(&obs, &action);
            let out = env.step(&action);
            obs = out.obs;
            terminated = out.terminated;
            step += 1;
        }
        hashes.push(env.fingerprint());
        if terminated {
            successes += 1;
        }
        ds.finish_episode();
    }
    (ds, hashes, successes)
}

fn main() {
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║  OriEra rigid3d × LeRobot v2.1 · 具身 3D 采集（桌面抓放）  ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!(
        "  任务 {} · {} 集 × ≤{} 步 · dt={:.4} · 首种子 {}",
        TASK_PHRASE, EPISODES, MAX_STEPS, DT, FIRST_SEED
    );

    let (ds1, hashes1, ok1) = collect();
    let (ds2, hashes2, ok2) = collect();
    assert_eq!(hashes1, hashes2, "#66+#67: episode fingerprints must match");
    assert_eq!(ok1, ok2);
    for (i, (e1, e2)) in ds1.episodes.iter().zip(ds2.episodes.iter()).enumerate() {
        for (r1, r2) in e1.rows.iter().zip(e2.rows.iter()) {
            assert_eq!(r1.state, r2.state, "episode {i}: states must be bit-equal");
            assert_eq!(
                r1.action, r2.action,
                "episode {i}: actions must be bit-equal"
            );
        }
    }
    println!(
        "  ✓ 双跑逐位一致：{} 集 · {} transitions · 成功 {} 集（{}%）",
        EPISODES,
        ds1.total_frames(),
        ok1,
        ok1 * 100 / EPISODES
    );
    for (i, h) in hashes1.iter().enumerate() {
        println!("    episode {i:02} · fingerprint {h:016x}");
    }
    assert!(
        ok1 * 2 >= EPISODES,
        "scripted baseline must solve at least half the seeds (got {ok1}/{EPISODES})"
    );

    let root = std::path::Path::new("out/embodied-sim/lerobot-v21");
    std::fs::create_dir_all(root).unwrap();
    let n = ds1.write_dir(root).unwrap();
    LeRobotV21::write_receipt(
        root,
        concat!("oriera-engine/", env!("CARGO_PKG_VERSION")),
        "rigid3d-rapier3d",
        DT,
        &hashes1,
        ok1,
    )
    .unwrap();
    LeRobotV21::write_card(root, TASK_PHRASE).unwrap();
    println!(
        "  ✓ 已写出 {}/（{n} 集，meta + receipt + card 全套）",
        root.display()
    );
    println!(
        "  下一步：python scripts/jsonl_to_lerobot_v21.py {} → lerobot-train 直读",
        root.display()
    );
}
