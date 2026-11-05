//! P2-W6B integration tests (PLAN-embodied-v3 §三「真 prismatic 抽屉」):
//! the `drawer-4dof-v2` preset end to end through the spec path — the
//! slide-prop panel on its prismatic rail, the drawer@v2 criterion
//! (pull ≥ target + released + still) with positive and negative
//! episodes, and the governance anchors (the old `drawer-4dof` v1
//! spec stays the frozen prop-approximation control with its
//! `ef18ac07` identity; the v2 preset is a new identity).
//!
//! The mechanism-level physics (force drag to the stop, drag back,
//! teleport adoption projected on the rail, the close-squeeze
//! geometry traps) is pinned in `oriera_subsys::rigid3d`'s in-module
//! tests; this file pins the ENV/spec layer.
#![cfg(all(feature = "rigid3d", feature = "spec"))]

use oriera_agent::embodied_env::{EmbodiedEnv, TableTopEnv};
use oriera_agent::spec::{official_specs_dir, SceneSpec};

/// The shipped v2 preset, resolved against the official library.
fn v2_spec() -> SceneSpec {
    let path = official_specs_dir().join("drawer-4dof-v2.json");
    SceneSpec::load_file(&path)
        .expect("drawer-4dof-v2 resolves against the official library")
        .spec
}

fn v2_env() -> TableTopEnv {
    let mut env = v2_spec().to_env();
    env.randomize = false; // nominal physics for the scripted runs
    env
}

/// The v1 control archive: the free box-prop approximation, frozen.
fn v1_spec() -> SceneSpec {
    let path = official_specs_dir().join("drawer-4dof.json");
    SceneSpec::load_file(&path)
        .expect("drawer-4dof (v1 control) resolves")
        .spec
}

/// The 4-DoF analytic IK (tool-down posture) — the same closed form
/// the baselines and the subsys drawer tests use.
fn drawer_ik(target: [f32; 3]) -> [f32; 4] {
    let d = oriera_subsys::ArmDescription::default();
    let (l1, l2) = (d.links[0], d.links[1]);
    let sh = d.base[1] + d.shoulder_height;
    let dx = target[0] - d.base[0];
    let dz = target[2] - d.base[2];
    let yaw = f32::atan2(-dz, dx);
    let r = (dx * dx + dz * dz).sqrt().clamp(0.06, l1 + l2 - 0.005);
    let h = (target[1] + d.links[2] - sh).clamp(-0.38, 0.30);
    let ce = ((r * r + h * h - l1 * l1 - l2 * l2) / (2.0 * l1 * l2)).clamp(-1.0, 1.0);
    let el = -ce.acos();
    let sh2 = f32::atan2(h, r) - f32::atan2(l2 * el.sin(), l1 + l2 * el.cos());
    let wr = -std::f32::consts::FRAC_PI_2 - sh2 - el;
    [
        yaw.clamp(d.limits[0].0, d.limits[0].1),
        sh2.clamp(d.limits[1].0, d.limits[1].1),
        el.clamp(d.limits[2].0, d.limits[2].1),
        wr.clamp(d.limits[3].0, d.limits[3].1),
    ]
}

/// One scripted phase: hold joint targets + gripper demand for `ticks`.
fn phase(env: &mut TableTopEnv, target: [f32; 3], ticks: u64, grip: f32) {
    let angles = drawer_ik(target);
    for _ in 0..ticks {
        let mut action = angles.to_vec();
        action.push(grip);
        env.step(&action);
    }
}

/// The carrot-drag phase: TCP leads the panel by ≤3 cm toward the
/// goal at the grasp height, re-read every tick (the baseline's
/// drawer_phases law) for `ticks`.
fn drag(env: &mut TableTopEnv, goal: [f32; 3], ticks: u64, grip: f32) {
    let grasp_h = 0.030_f32;
    for _ in 0..ticks {
        let o = env.cube_pos();
        let d = ((goal[0] - o[0]).powi(2) + (goal[2] - o[2]).powi(2)).sqrt();
        let lead = (d + 0.01).min(0.03);
        let toward = |p: f32, delta: f32| {
            if d < 1e-6 {
                p
            } else {
                p + delta / d * lead
            }
        };
        let want = [
            toward(o[0], goal[0] - o[0]),
            o[1] + grasp_h,
            toward(o[2], goal[2] - o[2]),
        ];
        let mut action = drawer_ik(want).to_vec();
        action.push(grip);
        env.step(&action);
    }
}

/// The carrot-drag phase with a CONDITIONED exit: stop as soon as the
/// panel is `until` metres out of its spawn (a railed panel has no
/// table-friction attractor — the carrot chases it at servo pace, so
/// a fixed-tick drag runs into the travel stop and parks stressed on
/// the limit spring; the real criterion only needs `until` ≥ 8 cm).
fn drag_until(
    env: &mut TableTopEnv,
    goal: [f32; 3],
    spawn_z: f32,
    until: f32,
    max_ticks: u64,
) -> f32 {
    let grasp_h = 0.030_f32;
    let mut pulled = 0.0_f32;
    for _ in 0..max_ticks {
        let o = env.cube_pos();
        pulled = spawn_z - o[2];
        if pulled >= until {
            break;
        }
        let d = ((goal[0] - o[0]).powi(2) + (goal[2] - o[2]).powi(2)).sqrt();
        let lead = (d + 0.01).min(0.03);
        let toward = |p: f32, delta: f32| {
            if d < 1e-6 {
                p
            } else {
                p + delta / d * lead
            }
        };
        let want = [
            toward(o[0], goal[0] - o[0]),
            o[1] + grasp_h,
            toward(o[2], goal[2] - o[2]),
        ];
        let mut action = drawer_ik(want).to_vec();
        action.push(1.0);
        env.step(&action);
    }
    pulled
}

#[test]
fn v2_spec_is_a_new_identity_with_a_railed_panel() {
    let spec = v2_spec();
    spec.validate().expect("v2 spec validates");
    // The rail is on the task object (props[0]) and the task id is v2.
    assert_eq!(spec.task.id, "drawer@v2");
    let slide = spec.scene.props[0]
        .slide
        .as_ref()
        .expect("the panel must carry its rail");
    assert_eq!(slide.axis, [0.0, 0.0, -1.0]);
    assert_eq!(slide.limits, [0.0, 0.1]);
    // The cabinet is real fixtures (ground + table + back + two rails).
    assert_eq!(spec.scene.fixtures.len(), 5);
    // New experiment identity: differs from the frozen v1 control and
    // from the golden tabletop, and is stable across loads.
    let h = spec.spec_hash_hex();
    assert_eq!(h, v2_spec().spec_hash_hex(), "identity is deterministic");
    assert_eq!(v1_spec().task.id, "drawer@v1");
    assert!(
        v1_spec().scene.props[0].slide.is_none(),
        "the v1 control stays the free box-prop approximation"
    );
    assert_ne!(h, v1_spec().spec_hash_hex());
    eprintln!("drawer-4dof-v2 spec_hash = {h}");
}

#[test]
fn v2_env_holds_the_panel_through_settle_and_wiggle() {
    let mut env = v2_env();
    env.reset(11);
    assert_eq!(env.obs_layout().total, 21, "obs contract unchanged");
    let spawn = env.cube_pos();
    assert!((spawn[2] - 0.0).abs() < 1e-4, "panel spawns closed");
    // The joint owns 5 DoF: a wiggling zero-torque arm must not sag,
    // drop or knock the panel off its rail (SETTLE + 90 wiggled ticks).
    for i in 0..90u64 {
        let a = (i as f32 * 0.05).sin() * 0.3;
        env.step(&[a, 0.5 + a, -0.8 - a, 0.2, 0.0]);
    }
    let now = env.cube_pos();
    assert!(
        (now[0] - spawn[0]).abs() < 2e-3
            && (now[1] - spawn[1]).abs() < 1e-3
            && (now[2] - spawn[2]).abs() < 2e-3,
        "the rail holds the panel against the wiggling arm: {spawn:?} → {now:?}"
    );
}

/// The positive episode: grasp the panel, drag it ≥ pull_distance
/// along −Z, release, let it settle — drawer@v2 must terminate with
/// success (pulled + released + still).
#[test]
fn v2_episode_drag_to_distance_released_and_still_succeeds() {
    let mut env = v2_env();
    let obs = env.reset(1000);
    assert_eq!(obs.len(), 21);
    let goal = env.goal();
    assert!(
        (goal[2] - (-0.08)).abs() < 1e-4,
        "goal = spawn + pull_axis·pull_distance: {goal:?}"
    );
    let spawn = env.cube_pos();
    phase(&mut env, [spawn[0], spawn[1] + 0.16, spawn[2]], 70, 0.0);
    phase(&mut env, [spawn[0], spawn[1] + 0.030, spawn[2]], 60, 0.0);
    phase(&mut env, [spawn[0], spawn[1] + 0.030, spawn[2]], 40, 1.0);
    let pulled = drag_until(&mut env, goal, spawn[2], 0.086, 360);
    assert!(
        pulled >= 0.08,
        "the friction drag must pull ≥ 8 cm (pulled {pulled:.4})"
    );
    // Hold the reached position under squeeze first (the arm settles,
    // the panel's momentum dies in the heavy slide damping) — then
    // release and let the rest window run.
    let dragged = env.cube_pos();
    phase(
        &mut env,
        [dragged[0], dragged[1] + 0.030, dragged[2]],
        45,
        1.0,
    );
    phase(
        &mut env,
        [dragged[0], dragged[1] + 0.030, dragged[2]],
        30,
        0.0,
    );
    let mut fired = None;
    for _ in 0..90 {
        let out = env.step(&[0.0; 5]);
        if out.terminated {
            fired = Some(out.success);
            break;
        }
    }
    assert_eq!(fired, Some(true), "released + still at ≥ 8 cm ⇒ success");
}

/// Negative 1: a 3 cm pull (short of the 8 cm target) never succeeds,
/// release and rest notwithstanding.
#[test]
fn v2_episode_short_pull_never_succeeds() {
    let mut env = v2_env();
    env.reset(1000);
    let spawn = env.cube_pos();
    phase(&mut env, [spawn[0], spawn[1] + 0.16, spawn[2]], 70, 0.0);
    phase(&mut env, [spawn[0], spawn[1] + 0.030, spawn[2]], 60, 0.0);
    phase(&mut env, [spawn[0], spawn[1] + 0.030, spawn[2]], 40, 1.0);
    // Drag only to a point 3 cm out (a third of the way).
    let short = [spawn[0], spawn[1], spawn[2] - 0.03];
    let pulled = drag_until(&mut env, short, spawn[2], 0.03, 300);
    let dragged = env.cube_pos();
    assert!(
        pulled < 0.08,
        "sanity: the short drag stayed short of the criterion ({pulled:.4})"
    );
    phase(
        &mut env,
        [dragged[0], dragged[1] + 0.030, dragged[2]],
        30,
        0.0,
    );
    for _ in 0..60 {
        let out = env.step(&[0.0; 5]);
        assert!(
            !out.terminated && !out.success,
            "a 3 cm pull must never fire drawer@v2"
        );
    }
}

/// Negative 2: the panel dragged the full distance but the gripper
/// never releases — the release half of the @v2 predicate must hold
/// the success down (the physics upgraded, the predicate did not).
#[test]
fn v2_episode_pulled_but_still_gripped_never_succeeds() {
    let mut env = v2_env();
    env.reset(1000);
    let goal = env.goal();
    let spawn = env.cube_pos();
    phase(&mut env, [spawn[0], spawn[1] + 0.16, spawn[2]], 70, 0.0);
    phase(&mut env, [spawn[0], spawn[1] + 0.030, spawn[2]], 60, 0.0);
    phase(&mut env, [spawn[0], spawn[1] + 0.030, spawn[2]], 40, 1.0);
    drag(&mut env, goal, 240, 1.0);
    let dragged = env.cube_pos();
    let pulled = spawn[2] - dragged[2];
    assert!(pulled >= 0.08, "the full drag landed {pulled:.4}");
    // Keep gripping; park above the dragged panel for the rest window.
    for _ in 0..120 {
        let mut action = drawer_ik([dragged[0], dragged[1] + 0.030, dragged[2]]).to_vec();
        action.push(1.0);
        let out = env.step(&action);
        assert!(
            !out.success,
            "gripper commanded closed ⇒ no release ⇒ no success"
        );
    }
}

/// The bare family alias still resolves to the frozen v1 entry, and
/// the v1 control archive runs its own (approximation) physics.
#[test]
fn v1_control_stays_the_frozen_prop_approximation() {
    let mut env = v1_spec().to_env();
    env.randomize = false;
    assert_eq!(env.task_id(), "drawer@v1");
    let obs = env.reset(1000);
    assert_eq!(obs.len(), 21);
    // The v1 handle is a free prop: give it a nudge of settling time
    // and it rests ON the table (the v2 panel rides its rail instead —
    // covered by `v2_env_holds_the_panel_through_settle_and_wiggle`).
    for _ in 0..30u64 {
        env.step(&[0.0; 5]);
    }
    let p = env.cube_pos();
    assert!(
        (p[1] - 0.525).abs() < 2e-3,
        "the free handle settles onto the table: {p:?}"
    );
}
