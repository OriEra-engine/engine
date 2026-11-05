//! M0b acceptance ①②: the GripperSpec *plate frame* — an external
//! gripper hanging DOWN off a hand-written 7-axis arm (Panda-shaped:
//! yaw + 2R planar + pitch + roll + yaw + distal pitch), placed by
//! `approach_axis`/`gap_axis` data instead of the preset's `+X/+Z`
//! convention, must grasp a cube under the TCP by plate friction,
//! carry it on lift and drop it on release — across seeds (smoke) —
//! plus placement/identity unit checks.
//!
//! This is the from_robot+抓取过闸 path of M0a acceptance ②'s second
//! half (PLAN-embodied-general §四 M0b): URDF-imported grippers are
//! wired through urdf.rs separately (WP-D), but the ENGINE contract
//! (oriented plates + friction grasp) is pinned here with a hand-written
//! RobotSpec, no ArmDescription in sight.
//!
//! ── 慢测试分层约定（PLAN-embodied-v3 P1 #20）────────────────────────
//! 本文件分两层，CI 只跑快层：
//! - 快层（默认 `cargo test` 可见）：`mirror_constants_match_the_preset`、
//!   `plates_hang_below_the_flange_per_the_axes`（280 tick 物理——板架
//!   朝向/放置契约，断言核心）、`orientation_is_identity_in_v2_but_not_v1`、
//!   `bad_plate_frames_are_named_build_errors`——零/短物理 tick。
//! - 慢层（`#[ignore = "slow: …"]`）：`four_seed_grasp_smoke_at_least_
//!   three_pass`——4 集抓取 + 同种子确定性双跑 ≈3000 tick（原整文件
//!   ~188s 的主体）。本地/夜间跑：`cargo test -p oriera-embodied --features
//!   rigid3d --test gripper_down_smoke -- --ignored`（CI 的 `--ignored`
//!   计数会因此变化，属于分层设计而非回归）。
#![cfg(feature = "rigid3d")]

use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, World};
use oriera_subsys::rigid3d::{
    sim_fingerprint, ArmDescription, JointTargets, JointTelemetry, MultibodySystem, Pose3, Scene3D,
};
use oriera_subsys::{
    Gravity, GripperKind, GripperSpec, JointSpec, LinkShape, LinkSpec, RobotSpec, RobotSpecError,
    SimObject, TimeScale,
};
use std::f32::consts::PI;

const DT: f32 = 1.0 / 60.0;
/// Table-top rest height of the 5 cm cube's centre (table top 0.5).
const CY: f32 = 0.525;
/// Cube spawn x (directly under the home TCP).
const CX: f32 = 0.40;

/// The build constants this test's robot uses, read off the 4-DoF
/// preset projection (one source — no hardcoded magic numbers here).
const ARMATURE: f32 = 0.02; // == rigid3d::ARMATURE (pub(crate); asserted against the preset below)
const JOINT_DAMPING: f32 = 0.05; // == rigid3d::JOINT_DAMPING
const LINK_DENSITY: f32 = 1500.0; // == rigid3d::LINK_DENSITY
const LINK_ADDED_MASS: f32 = 0.2; // == rigid3d::LINK_ADDED_MASS
const GRIPPER_EFFORT: f32 = 20.0; // == rigid3d::GRIPPER_EFFORT
const FINGER_GAP_RATE: f32 = 0.004; // == rigid3d::FINGER_GAP_RATE

/// Guard the mirror constants against drift: they must equal the
/// preset's projection field by field (the test robot is otherwise
/// independent of the preset).
#[test]
fn mirror_constants_match_the_preset() {
    let preset = RobotSpec::from(&ArmDescription::default());
    let j = &preset.joints[0];
    assert_eq!((j.armature, j.damping), (ARMATURE, JOINT_DAMPING));
    let l = &preset.links[1];
    assert_eq!((l.density, l.added_mass), (LINK_DENSITY, LINK_ADDED_MASS));
    let g = preset.gripper.as_ref().unwrap();
    assert_eq!((g.effort, g.gap_rate), (GRIPPER_EFFORT, FINGER_GAP_RATE));
}

fn ctx(step: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(step),
        dt: DT,
        now: Timestamp(0),
        grants: Grants::EMPTY,
    }
}

/// The plate constants of the downward gripper (mirrors the preset's
/// squeeze/geometry discipline: 9 cm plate, 2 mm/side squeeze at closed).
const FINGER_X: f32 = 0.06; // TCP = plate centre, 6 cm below the flange
const GAP_OPEN: f32 = 0.075;
const GAP_CLOSED: f32 = 0.046;
/// Servo gains of the 7-axis arm. The 4-DoF preset's 60/4 is tuned for
/// a 0.2 m reach; this arm grasps at full extension (0.38 m lever) and
/// an industrial-class stiffness keeps the gravity sag at the TCP to
/// millimetres (rapier's ForceBased position motors solve implicitly,
/// so the high gain is stable).
const KP: f32 = 400.0;
const KD: f32 = 20.0;

/// Hand-written 7-axis arm whose home stretches along +X at shoulder
/// height 0.62 (identity rotations); commanding `wrist_roll = -π/2`
/// rolls the distal segment so the attach link's local **−Z** points at
/// world **−Y** — the gripper hangs down while the plate-frame axes do
/// the pointing. Gripper: approach `[0,0,-1]`, gap `[0,1,0]` — both
/// non-default, i.e. the placement is DATA, not the preset convention.
fn seven_axis_down_gripper_spec() -> RobotSpec {
    let sx = 0.02f32;
    let sy = 0.62f32;
    let base_h = 0.06f32;
    let r = 0.02f32;
    let link = |name: &str, half: [f32; 3], origin: [f32; 3], off: [f32; 3]| LinkSpec {
        name: name.into(),
        shape: LinkShape::Box { half },
        origin,
        collider_offset: off,
        density: LINK_DENSITY,
        friction: 0.7,
        added_mass: LINK_ADDED_MASS,
        inertial: None, // M2 opt-in: real inertia not used by this robot
        mesh_uri: None,
    };
    let links = vec![
        LinkSpec {
            name: "base".into(),
            shape: LinkShape::Box {
                half: [0.04, base_h, 0.04],
            },
            origin: [sx, 0.5 + base_h, 0.0],
            collider_offset: [0.0; 3],
            density: 1.0,
            friction: 0.5,
            added_mass: 0.0,
            inertial: None, // M2 opt-in
            mesh_uri: None,
        },
        link("carrier", [0.015; 3], [sx, sy, 0.0], [0.0; 3]),
        link("upper_arm", [0.07, r, r], [sx, sy, 0.0], [0.07, 0.0, 0.0]),
        link(
            "forearm",
            [0.06, r, r],
            [sx + 0.14, sy, 0.0],
            [0.06, 0.0, 0.0],
        ),
        link(
            "wrist",
            [0.03, r, r],
            [sx + 0.26, sy, 0.0],
            [0.03, 0.0, 0.0],
        ),
        link(
            "roll_seg",
            [0.015, r, r],
            [sx + 0.32, sy, 0.0],
            [0.015, 0.0, 0.0],
        ),
        link(
            "yaw_seg",
            [0.015, r, r],
            [sx + 0.35, sy, 0.0],
            [0.015, 0.0, 0.0],
        ),
        link(
            "flange",
            [0.01, r, r],
            [sx + 0.38, sy, 0.0],
            [0.01, 0.0, 0.0],
        ),
    ];
    let j =
        |name: &str, p: usize, c: usize, anchor: [f32; 3], axis: [f32; 3], limits: (f32, f32)| {
            JointSpec {
                name: name.into(),
                kind: oriera_subsys::JointKind::Revolute, // M3 field (revolute = legacy)
                parent_link: p,
                child_link: c,
                anchor,
                axis,
                limits,
                effort: None,
                velocity_limit: None,
                kp: KP,
                kd: KD,
                armature: ARMATURE,
                damping: JOINT_DAMPING,
            }
        };
    let joints = vec![
        j(
            "base_yaw",
            0,
            1,
            [0.0, 0.12 - base_h, 0.0],
            [0.0, 1.0, 0.0],
            (-PI, PI),
        ),
        j("shoulder", 1, 2, [0.0; 3], [0.0, 0.0, 1.0], (-0.35, 1.45)),
        j(
            "elbow",
            2,
            3,
            [0.14, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            (-2.5, 0.35),
        ),
        j(
            "wrist_pitch",
            3,
            4,
            [0.12, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            (-2.0, 2.0),
        ),
        j(
            "wrist_roll",
            4,
            5,
            [0.06, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            (-PI, PI),
        ),
        j(
            "wrist_yaw",
            5,
            6,
            [0.03, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            (-2.0, 2.0),
        ),
        j(
            "tool_pitch",
            6,
            7,
            [0.03, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            (-2.0, 2.0),
        ),
    ];
    let gripper = GripperSpec {
        attach_link: 7,
        finger_x: FINGER_X,
        tcp_offset: [0.0, 0.0, -FINGER_X], // TCP hangs along the approach axis
        finger_half: [0.030, 0.006, 0.045], // width X / gap thickness Z / length Y in attach frame
        gap_open: GAP_OPEN,
        gap_closed: GAP_CLOSED,
        friction: 1.4,
        effort: GRIPPER_EFFORT,
        gap_rate: FINGER_GAP_RATE,
        kind: GripperKind::ParallelPlates,
        approach_axis: Some([0.0, 0.0, -1.0]), // flange local −Z = world down (after the roll)
        gap_axis: Some([0.0, 1.0, 0.0]),       // plates separate along flange local +Y
        contact_mesh: false,                   // M2 opt-in (primitive plates)
        holding_criterion: oriera_subsys::HoldingCriterion::default(), // v1 tip-radius
    };
    RobotSpec {
        name: "tabletop-7dof-down-gripper".into(),
        base: [sx, 0.5, 0.0],
        links,
        joints,
        gripper: Some(gripper),
        home: vec![0.0; 7],
        mass_scale: 1.0,
        action_scale: vec![2.0; 7],
        self_collision: oriera_subsys::SelfCollision::Off, // M2 opt-in (legacy off)
    }
}

/// Settled home joint targets: `[0, 0, 0, 0, -π/2, 0, 0]` — the roll
/// is what turns flange local −Z into world down. At this pose the TCP
/// sits at the cube's grasp height.
fn home_targets() -> Vec<f32> {
    vec![0.0, 0.0, 0.0, 0.0, -std::f32::consts::FRAC_PI_2, 0.0, 0.0]
}

/// Parallelogram pose: shoulder `+a`, elbow `−a` (the distal chain and
/// hence the gripper orientation stay exactly where they were); `roll`
/// is the wrist-roll angle. TCP height rises by `0.14·sin a`.
fn pose_targets(a_deg: f32, roll: f32) -> Vec<f32> {
    let a = a_deg.to_radians();
    vec![0.0, a, -a, 0.0, roll, 0.0, 0.0]
}

/// Lift targets: +30° parallelogram, rolled — net +7 cm of TCP height
/// with the gripper still pointing down.
fn lift_targets() -> Vec<f32> {
    pose_targets(30.0, -std::f32::consts::FRAC_PI_2)
}

/// Phase runner: hold `targets`/`grip` for `ticks`.
fn run_phase(
    world: &mut World,
    sys: &mut MultibodySystem,
    arm: Entity,
    targets: &[f32],
    grip: f32,
    ticks: u64,
    i: &mut u64,
) {
    for _ in 0..ticks {
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.angles = targets.to_vec();
            t.gripper = grip;
        }
        sys.run(&ctx(*i), world);
        *i += 1;
    }
}

/// Approach choreography shared by every test — the way a real pick
/// approaches, sequenced so the 9 cm plates never sweep through the
/// cube column: (A) raise the arm with the wrist unrolled (plates point
/// sideways, well above the cube), (B) roll the wrist at height so the
/// plates swing down into their hanging pose with ~9 cm of clearance,
/// (C) descend straight onto the cube with the plates straddling it.
fn approach(world: &mut World, sys: &mut MultibodySystem, arm: Entity, i: &mut u64) {
    run_phase(world, sys, arm, &pose_targets(60.0, 0.0), 0.0, 80, i);
    run_phase(
        world,
        sys,
        arm,
        &pose_targets(60.0, -std::f32::consts::FRAC_PI_2),
        0.0,
        80,
        i,
    );
    run_phase(world, sys, arm, &home_targets(), 0.0, 120, i);
}

/// World + arm entity + the cube under the TCP (seed-jittered spawn).
fn bench(seed: u64) -> (World, MultibodySystem, Entity, Entity) {
    // tiny deterministic jitter of the spawn point (the "episodes")
    let j1 = ((seed.wrapping_mul(6364136223846793005) >> 33) as f32 / (u32::MAX >> 1) as f32) - 1.0;
    let j2 = ((seed.wrapping_mul(2862933555777941757) >> 33) as f32 / (u32::MAX >> 1) as f32) - 1.0;
    let cube = [CX + j1 * 0.004, CY, j2 * 0.008];
    let mut world = World::new();
    world.insert_resource(Gravity(9.8));
    world.insert_resource(TimeScale(1.0));
    let arm = world.spawn();
    world.insert(
        arm,
        JointTargets {
            angles: pose_targets(60.0, 0.0),
            gripper: 0.0,
        },
    );
    world.insert(arm, JointTelemetry::default());
    let obj = world.spawn();
    world.insert(
        obj,
        Pose3 {
            pos: cube,
            ..Default::default()
        },
    );
    world.insert(obj, SimObject);
    let sys = MultibodySystem::from_robot(seven_axis_down_gripper_spec(), Scene3D::default())
        .expect("7-axis down-gripper spec builds");
    (world, sys, arm, obj)
}

fn pose(world: &World, e: Entity) -> Pose3 {
    *world.get::<Pose3>(e).expect("prop pose")
}

fn telemetry(world: &World, e: Entity) -> JointTelemetry {
    world
        .get::<JointTelemetry>(e)
        .expect("arm telemetry")
        .clone()
}

/// Unit: the oriented plates hang BELOW the flange (approach `[0,0,-1]`
/// × the rolled attach frame) and straddle the cube along world Z (gap
/// axis `[0,1,0]`), open gap per spec.
#[test]
fn plates_hang_below_the_flange_per_the_axes() {
    let (mut world, mut sys, arm, obj) = bench(11);
    let mut i = 0u64;
    approach(&mut world, &mut sys, arm, &mut i);
    // the approach must not have disturbed the cube
    let cube = pose(&world, obj).pos;
    assert!(
        (cube[1] - CY).abs() < 0.01 && (cube[0] - CX).abs() < 0.02 && cube[2].abs() < 0.03,
        "approach must leave the cube at rest under the TCP, got {cube:?}"
    );
    let (plates, _counts) = sys.debug_contact_state();
    let mid = [
        (plates[0][0] + plates[1][0]) * 0.5,
        (plates[0][1] + plates[1][1]) * 0.5,
        (plates[0][2] + plates[1][2]) * 0.5,
    ];
    let sep_z = (plates[0][2] - plates[1][2]).abs();
    let sep_y = (plates[0][1] - plates[1][1]).abs();
    // plate centre = TCP = 7 cm below the flange at y 0.62, under the cube
    assert!(
        (mid[1] - (CY + 0.025)).abs() < 0.02,
        "plates must hang at TCP height ≈0.55, got y={:.4}",
        mid[1]
    );
    assert!(
        (mid[0] - CX).abs() < 0.02 && mid[2].abs() < 0.01,
        "plates must sit under the cube column: {mid:?}"
    );
    assert!(
        (sep_z - GAP_OPEN).abs() < 2e-3,
        "plates separate along world Z (gap axis): {sep_z}"
    );
    assert!(
        sep_y < 2e-3,
        "plates must NOT separate along world Y (approach axis): {sep_y}"
    );
    // and the reported tip is the TCP (cube centre within grasp radius)
    let tel = telemetry(&world, arm);
    let d = ((tel.ee_pos[0] - CX).powi(2) + (tel.ee_pos[1] - CY).powi(2) + tel.ee_pos[2].powi(2))
        .sqrt();
    assert!(
        d < 0.06,
        "tip must be at the plate centre, |tip-cube| = {d:.4}"
    );
}

/// Unit: the identity/fingerprint layer sees the orientation — a spec
/// whose only difference is the plate frame differs in `identity_hash`
/// and `fingerprint_v2` (the v1 triple fold is documented-blind to it).
#[test]
fn orientation_is_identity_in_v2_but_not_v1() {
    let mut flat = seven_axis_down_gripper_spec();
    {
        let g = flat.gripper.as_mut().unwrap();
        g.approach_axis = None;
        g.gap_axis = None;
    }
    let down = seven_axis_down_gripper_spec();
    flat.validate().unwrap();
    down.validate().unwrap();
    let h_flat = oriera_subsys::rigid3d::robot_identity_hash(&flat);
    let h_down = oriera_subsys::rigid3d::robot_identity_hash(&down);
    assert_ne!(h_flat, h_down, "plate frame is v2 identity");
    let (w, _s, _arm, _obj) = bench(11);
    let sys_flat = MultibodySystem::from_robot(flat, Scene3D::default()).unwrap();
    let sys_down = MultibodySystem::from_robot(down, Scene3D::default()).unwrap();
    // v1 folds only joints[0] kp/kd + mass_scale — same here, so v1
    // cannot tell the two robots apart (documented blind spot)
    assert_eq!(sys_flat.fingerprint(&w), sys_down.fingerprint(&w));
    assert_ne!(
        sys_flat.fingerprint_v2(&w, None),
        sys_down.fingerprint_v2(&w, None)
    );
}

/// Unit: validation still refuses a degenerate plate frame on a
/// full 7-axis spec (zero / parallel axes) with a message naming the
/// field — an honest build error, not a misplaced plate.
#[test]
fn bad_plate_frames_are_named_build_errors() {
    let mut zero = seven_axis_down_gripper_spec();
    zero.gripper.as_mut().unwrap().approach_axis = Some([0.0; 3]);
    match MultibodySystem::from_robot(zero, Scene3D::default()) {
        Ok(_) => panic!("zero approach_axis must be refused"),
        Err(RobotSpecError::BadGripper(msg)) => assert!(msg.contains("approach_axis"), "{msg}"),
        Err(e) => panic!("expected BadGripper, got {e}"),
    }
    let mut parallel = seven_axis_down_gripper_spec();
    {
        let g = parallel.gripper.as_mut().unwrap();
        g.approach_axis = Some([0.0, 0.0, -1.0]);
        g.gap_axis = Some([0.0, 0.0, 2.0]); // parallel to the approach
    }
    match MultibodySystem::from_robot(parallel, Scene3D::default()) {
        Ok(_) => panic!("parallel plate axes must be refused"),
        Err(RobotSpecError::BadGripper(msg)) => assert!(msg.contains("parallel"), "{msg}"),
        Err(e) => panic!("expected BadGripper(parallel), got {e}"),
    }
    // sanity: the preset path still builds (the ArmDescription preset is
    // the +X/+Z fallback)
    let preset = MultibodySystem::new(ArmDescription::default(), Scene3D::default());
    let _ = preset;
}

/// The 4-seed grasp smoke (acceptance ②: ≥3/4): spawn the cube under
/// the TCP, close the downward plates, lift via the parallelogram
/// configuration, assert the cube RODE the gripper (friction carry),
/// then release and assert it fell. Deterministic per seed.
fn grasp_episode(seed: u64) -> bool {
    let (mut world, mut sys, arm, obj) = bench(seed);
    let mut i = 0u64;
    // 1. roll in high, descend onto the cube (plates straddle it, open)
    approach(&mut world, &mut sys, arm, &mut i);
    let cube_after_approach = pose(&world, obj).pos;
    // 2. close onto the cube
    run_phase(&mut world, &mut sys, arm, &home_targets(), 1.0, 80, &mut i);
    let holding = telemetry(&world, arm).holding;
    let cube_after_close = pose(&world, obj).pos;
    // 3. lift (parallelogram: gripper stays pointing down)
    run_phase(&mut world, &mut sys, arm, &lift_targets(), 1.0, 150, &mut i);
    let carried = pose(&world, obj).pos[1];
    let carried_holding = telemetry(&world, arm).holding;
    let carry_ok = carried > CY + 0.04 && carried_holding;
    // 4. release at height
    run_phase(&mut world, &mut sys, arm, &lift_targets(), 0.0, 100, &mut i);
    let dropped = pose(&world, obj).pos[1];
    let drop_ok = dropped < carried - 0.02;
    eprintln!(
        "seed {seed}: cube@approach=({:.3},{:.3},{:.3}) cube@close=({:.3},{:.3},{:.3}) holding@close={holding} carried={carried:.3} (want >{:.3}) holding@carry={carried_holding} dropped={dropped:.3} ⇒ {}",
        cube_after_approach[0], cube_after_approach[1], cube_after_approach[2],
        cube_after_close[0], cube_after_close[1], cube_after_close[2],
        CY + 0.04,
        if carry_ok && drop_ok { "PASS" } else { "FAIL" }
    );
    carry_ok && drop_ok && holding
}

#[test]
#[ignore = "slow: 4 集抓取 + 确定性双跑 ≈3000 tick 物理长跑——本地/夜间跑 cargo test -- --ignored"]
fn four_seed_grasp_smoke_at_least_three_pass() {
    let seeds = [11u64, 23, 37, 41];
    let passed = seeds.iter().filter(|&&s| grasp_episode(s)).count();
    assert!(
        passed >= 3,
        "external downward gripper must grasp ≥3/4 seeds, got {passed}/4"
    );
    // determinism: the same seed replays to the same per-tick state
    let (mut w1, mut s1, a1, o1) = bench(37);
    let (mut w2, mut s2, a2, o2) = bench(37);
    let script = |world: &mut World, sys: &mut MultibodySystem, arm: Entity, obj: Entity| {
        let mut i = 0u64;
        let mut out = Vec::new();
        for (targets, grip, ticks) in [
            (pose_targets(60.0, 0.0), 0.0, 80),
            (pose_targets(60.0, -std::f32::consts::FRAC_PI_2), 0.0, 80),
            (home_targets(), 0.0, 120),
            (home_targets(), 1.0, 80),
            (lift_targets(), 1.0, 150),
        ] {
            for _ in 0..ticks {
                if let Some(t) = world.get_mut::<JointTargets>(arm) {
                    t.angles = targets.clone();
                    t.gripper = grip;
                }
                sys.run(&ctx(i), world);
                i += 1;
                out.push((
                    telemetry(world, arm),
                    pose(world, obj),
                    sim_fingerprint(world),
                ));
            }
        }
        out
    };
    let r1 = script(&mut w1, &mut s1, a1, o1);
    let r2 = script(&mut w2, &mut s2, a2, o2);
    assert_eq!(r1, r2, "same seed ⇒ bit-identical grasp episode");
}
