//! M3 移动底盘预设 (`crates/oriera-agent/specs/official/mobile-arm-4dof.json`)
//! — the ENGINE side: the preset's robot block mirrored in Rust (same
//! numbers, same order; the agent-side test
//! `tests/urdf_robot_spec.rs::mobile_arm_preset_json_*` loads the JSON
//! itself) settles at home, tracks base + arm targets, and the placement
//! claim of the preset ("the cube and the goal are both outside the
//! parked arm's reach; only a moved base reaches them") holds in the
//! simulated geometry.
//!
//! Chain: fixed `rail_mount` → `slide_x` (prismatic X) → `slide_z`
//! (prismatic Z — the "y" of a top-down/ROS planar base; OriEra is
//! Y-up) → `chassis_yaw` (revolute Y, the box chassis) → the tabletop
//! 4-axis arm (`From<&ArmDescription>` stages and geometry, its base
//! column folded into the chassis since RobotSpec has no fixed joints)
//! with the legacy plates. The base is a gantry abstraction: the
//! chassis rides its prismatic joints 2 cm above the floor (no wheel
//! contact model — honest scope, docs/POSITIONING.md).
#![cfg(feature = "rigid3d")]

use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, World};
use oriera_subsys::{
    ArmDescription, FixtureSlab, Gravity, JointKind, JointSpec, JointTargets, JointTelemetry,
    LinkShape, LinkSpec, MultibodySystem, RobotSpec, TimeScale,
};
use std::f32::consts::PI;

const DT: f32 = 1.0 / 60.0;

/// Preset placement bands (mirrors the JSON `scene.props[0].placement`
/// and `task.params.goal`): cube on the −Z side, goal on the +Z side.
const CUBE_X: [f32; 2] = [0.14, 0.22];
const CUBE_Z: [f32; 2] = [-0.52, -0.46];
const GOAL_X: [f32; 2] = [0.14, 0.22];
const GOAL_Z: [f32; 2] = [0.48, 0.54];

fn ctx(step: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(step),
        dt: DT,
        now: Timestamp(0),
        grants: Grants::EMPTY,
    }
}

/// The preset's floor (its only fixture).
fn floor() -> Vec<FixtureSlab> {
    vec![FixtureSlab {
        center: [0.0, -0.05, 0.0],
        half: [2.0, 0.05, 1.5],
        friction: 0.9,
    }]
}

/// Mirror of `mobile-arm-4dof.json` → `robot` (keep in lockstep).
fn mobile_arm_spec() -> RobotSpec {
    let preset = RobotSpec::from(&ArmDescription::default());
    let link = |name: &str, half: [f32; 3], origin: [f32; 3], off: [f32; 3], added: f32| LinkSpec {
        name: name.into(),
        shape: LinkShape::Box { half },
        origin,
        collider_offset: off,
        density: 1500.0,
        friction: 0.7,
        added_mass: added,
        inertial: None,
        mesh_uri: None,
    };
    let links = vec![
        LinkSpec {
            name: "rail_mount".into(),
            shape: LinkShape::Box { half: [0.02; 3] },
            origin: [-0.03, 0.03, 0.0],
            collider_offset: [0.0; 3],
            density: 1.0,
            friction: 0.5,
            added_mass: 0.0,
            inertial: None,
            mesh_uri: None,
        },
        link("carriage_x", [0.02, 0.01, 0.02], [-0.03, 0.03, 0.0], [0.0; 3], 0.5),
        link("carriage_z", [0.02, 0.01, 0.02], [-0.03, 0.03, 0.0], [0.0; 3], 0.5),
        link("chassis", [0.10, 0.03, 0.08], [-0.03, 0.05, 0.0], [0.0; 3], 1.0),
        link("yaw_carrier", [0.015; 3], [0.02, 0.12, 0.0], [0.0; 3], 0.2),
        link("upper_arm", [0.1, 0.022, 0.022], [0.02, 0.12, 0.0], [0.1, 0.0, 0.0], 0.2),
        link("forearm", [0.09, 0.022, 0.022], [0.22, 0.12, 0.0], [0.09, 0.0, 0.0], 0.2),
        link("wrist", [0.045, 0.022, 0.022], [0.40, 0.12, 0.0], [0.045, 0.0, 0.0], 0.2),
    ];
    #[allow(clippy::too_many_arguments)]
    let j = |name: &str,
             kind: JointKind,
             p: usize,
             c: usize,
             anchor: [f32; 3],
             axis: [f32; 3],
             limits: (f32, f32),
             effort: Option<f32>,
             velocity_limit: Option<f32>,
             gains: (f32, f32, f32, f32)| JointSpec {
        name: name.into(),
        kind,
        parent_link: p,
        child_link: c,
        anchor,
        axis,
        limits,
        effort,
        velocity_limit,
        kp: gains.0,
        kd: gains.1,
        armature: gains.2,
        damping: gains.3,
    };
    use JointKind::{Prismatic, Revolute};
    let slide = (500.0, 80.0, 0.5, 5.0); // N/m, N·s/m, kg, N·s/m
    let arm = (60.0, 4.0, 0.02, 0.05); // the tabletop preset's servo
    let joints = vec![
        j("slide_x", Prismatic, 0, 1, [0.0; 3], [1.0, 0.0, 0.0], (-0.20, 0.20), Some(150.0), Some(0.5), slide),
        j("slide_z", Prismatic, 1, 2, [0.0; 3], [0.0, 0.0, 1.0], (-0.45, 0.45), Some(150.0), Some(0.5), slide),
        j(
            "chassis_yaw",
            Revolute,
            2,
            3,
            [0.0, 0.02, 0.0],
            [0.0, 1.0, 0.0],
            (-PI, PI),
            None,
            Some(1.5),
            (80.0, 8.0, 0.05, 0.1),
        ),
        j("base_yaw", Revolute, 3, 4, [0.05, 0.07, 0.0], [0.0, 1.0, 0.0], (-PI, PI), None, None, arm),
        j("shoulder", Revolute, 4, 5, [0.0; 3], [0.0, 0.0, 1.0], (-0.35, 1.45), None, None, arm),
        j("elbow", Revolute, 5, 6, [0.2, 0.0, 0.0], [0.0, 0.0, 1.0], (-2.5, 0.35), None, None, arm),
        j("wrist_pitch", Revolute, 6, 7, [0.18, 0.0, 0.0], [0.0, 0.0, 1.0], (-2.0, 2.0), None, None, arm),
    ];
    let mut gripper = preset.gripper.clone().expect("preset gripper");
    gripper.attach_link = 7;
    RobotSpec {
        name: "mobile-arm-4dof".into(),
        base: [-0.03, 0.0, 0.0],
        links,
        joints,
        gripper: Some(gripper),
        home: vec![0.0; 7],
        mass_scale: 1.0,
        action_scale: vec![0.1, 0.1, 2.0, 2.0, 2.0, 2.0, 2.0],
        ..preset
    }
}

fn bench() -> (World, MultibodySystem, Entity) {
    let spec = mobile_arm_spec();
    let mut world = World::new();
    world.insert_resource(Gravity(9.8));
    world.insert_resource(TimeScale(1.0));
    let arm = world.spawn();
    world.insert(
        arm,
        JointTargets {
            angles: spec.home.clone(),
            gripper: 0.0,
        },
    );
    world.insert(arm, JointTelemetry::default());
    let sys = MultibodySystem::from_robot_fixtures(spec, &floor()).expect("mobile preset builds");
    (world, sys, arm)
}

fn hold(
    world: &mut World,
    sys: &mut MultibodySystem,
    arm: Entity,
    targets: &[f32],
    t0: u64,
    ticks: u64,
) -> JointTelemetry {
    for i in t0..t0 + ticks {
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.angles = targets.to_vec();
        }
        sys.run(&ctx(i), world);
    }
    world.get::<JointTelemetry>(arm).cloned().expect("telemetry")
}

/// Shoulder (= `yaw_carrier` origin) in the XZ plane: `chain_points`
/// lists the child origins of joints 1.. — index 2 is `base_yaw`'s child.
fn shoulder_xz(sys: &MultibodySystem) -> [f32; 2] {
    let p = sys.chain_points()[2];
    [p[0], p[2]]
}

/// Distance from `s` to the nearest / farthest point of an XZ band.
fn band_dist(s: [f32; 2], x: [f32; 2], z: [f32; 2]) -> (f32, f32) {
    let near = |v: f32, b: [f32; 2]| v.clamp(b[0], b[1]) - v;
    let far = |v: f32, b: [f32; 2]| (b[0] - v).abs().max((b[1] - v).abs());
    let n = (near(s[0], x).powi(2) + near(s[1], z).powi(2)).sqrt();
    let f = (far(s[0], x).powi(2) + far(s[1], z).powi(2)).sqrt();
    (n, f)
}

#[test]
fn mobile_preset_mirror_is_valid_and_settles_at_home() {
    let spec = mobile_arm_spec();
    spec.validate().expect("mobile preset is a valid RobotSpec");
    assert_eq!(spec.dof(), 7);
    let kinds: Vec<_> = spec.joints.iter().map(|j| j.kind).collect();
    assert_eq!(&kinds[..2], &[JointKind::Prismatic, JointKind::Prismatic]);
    assert!(kinds[2..].iter().all(|k| *k == JointKind::Revolute));

    let (mut world, mut sys, arm) = bench();
    let tel = hold(&mut world, &mut sys, arm, &[0.0; 7], 0, 180);
    assert!(tel.angles.iter().chain(&tel.velocities).all(|v| v.is_finite()));
    assert!(tel.angles[0].abs() < 3e-3 && tel.angles[1].abs() < 3e-3, "base parked: {:?}", tel.angles);
    assert!(tel.angles[2].abs() < 0.02, "chassis yaw parked: {}", tel.angles[2]);
    assert!(
        tel.velocities.iter().all(|v| v.abs() < 0.05),
        "settled: {:?}",
        tel.velocities
    );
    // The chassis floats on its gantry: never sinks into the floor.
    assert!(tel.ee_pos[1] > 0.05, "TCP above the floor: {:?}", tel.ee_pos);
}

#[test]
fn mobile_base_tracks_drive_targets_and_carries_the_arm() {
    let (mut world, mut sys, arm) = bench();
    let home = hold(&mut world, &mut sys, arm, &[0.0; 7], 0, 180);
    let pivot0 = [-0.03f32, 0.0]; // chassis_yaw axis (XZ) at home
    let drive = [0.15f32, -0.30, 0.5];
    let mut targets = vec![0.0f32; 7];
    targets[..3].copy_from_slice(&drive);
    let tel = hold(&mut world, &mut sys, arm, &targets, 180, 360);
    assert!((tel.angles[0] - drive[0]).abs() < 5e-3, "slide_x: {}", tel.angles[0]);
    assert!((tel.angles[1] - drive[1]).abs() < 5e-3, "slide_z: {}", tel.angles[1]);
    assert!((tel.angles[2] - drive[2]).abs() < 0.03, "chassis_yaw: {}", tel.angles[2]);
    // TCP = pivot' + R_y(θ)·(TCP_home − pivot): the parked arm rides the
    // base rigidly (same posture ⇒ same sag).
    let (s, c) = drive[2].sin_cos();
    let rel = [home.ee_pos[0] - pivot0[0], home.ee_pos[2] - pivot0[1]];
    let want = [
        pivot0[0] + drive[0] + c * rel[0] + s * rel[1],
        pivot0[1] + drive[1] - s * rel[0] + c * rel[1],
    ];
    let err = ((tel.ee_pos[0] - want[0]).powi(2) + (tel.ee_pos[2] - want[1]).powi(2)).sqrt();
    eprintln!(
        "mobile drive: TCP {:?} vs predicted [{:.4}, _, {:.4}] (err {:.4} m)",
        tel.ee_pos, want[0], want[1], err
    );
    assert!(err < 0.02, "TCP must ride the base: err {err} m");
    assert!((tel.ee_pos[1] - home.ee_pos[1]).abs() < 0.02, "height kept: {:?}", tel.ee_pos);

    // Arm stages still track on the moved base.
    let mut reach = targets.clone();
    reach[3..].copy_from_slice(&[0.3, 0.6, -0.9, 0.2]);
    let tel = hold(&mut world, &mut sys, arm, &reach, 540, 300);
    for (k, (got, want)) in tel.angles.iter().zip(&reach).enumerate() {
        let tol = if k < 2 { 5e-3 } else { 0.08 };
        assert!((got - want).abs() < tol, "joint {k}: got {got}, want {want}");
    }
}

#[test]
fn cube_and_goal_are_reachable_only_after_the_base_moves() {
    let d = ArmDescription::default();
    // Horizontal reach: stretched (l1 + l2 + tcp) and tool-down (l1 + l2,
    // the TCP hangs below the wrist joint) — the pick needs the latter.
    let stretched = d.links[0] + d.links[1] + d.links[2];
    let tool_down = d.links[0] + d.links[1];
    let (mut world, mut sys, arm) = bench();
    hold(&mut world, &mut sys, arm, &[0.0; 7], 0, 120);
    let home = shoulder_xz(&sys);
    let (cube_near, _) = band_dist(home, CUBE_X, CUBE_Z);
    let (goal_near, _) = band_dist(home, GOAL_X, GOAL_Z);
    eprintln!("parked shoulder {home:?}: cube ≥ {cube_near:.3} m, goal ≥ {goal_near:.3} m (reach {stretched})");
    assert!(cube_near > stretched, "cube must be out of the parked arm's reach");
    assert!(goal_near > stretched, "goal must be out of the parked arm's reach");

    // Drive to the cube side (−Z): the whole cube band enters the
    // tool-down reach; then to the goal side (+Z): the goal band does.
    let mut t = vec![0.0f32; 7];
    t[1] = -0.40;
    hold(&mut world, &mut sys, arm, &t, 120, 240);
    let (_, cube_far) = band_dist(shoulder_xz(&sys), CUBE_X, CUBE_Z);
    t[1] = 0.40;
    hold(&mut world, &mut sys, arm, &t, 360, 300);
    let (_, goal_far) = band_dist(shoulder_xz(&sys), GOAL_X, GOAL_Z);
    eprintln!("moved: cube ≤ {cube_far:.3} m, goal ≤ {goal_far:.3} m (tool-down reach {tool_down})");
    assert!(cube_far < tool_down, "after slide_z −0.40 the cube band is reachable");
    assert!(goal_far < tool_down, "after slide_z +0.40 the goal band is reachable");
}
