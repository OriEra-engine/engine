//! M3 prismatic joints (PLAN-embodied-general §四 M3「移动底盘（prismatic
//! /平面关节）」): the ENGINE contract of `JointKind::Prismatic`.
//!
//! - a single slide follows a commanded displacement (metres), its
//!   readout is the body's real displacement along the axis (the joint
//!   anchor removed), limits / `velocity_limit` are in metres / m/s;
//! - the ForceBased motor is the same PD as a revolute stage with N/m
//!   gains: a vertical slide holds its weight at the `mg/kp` sag and
//!   drops to its stop once `effort` (N) is below the weight;
//! - identity: the kind folds into the v2 robot identity only when
//!   prismatic — the 4-DoF preset's pinned identity hash is unchanged —
//!   and a prismatic run is bit-deterministic;
//! - the synthetic slide-arm fixture (linear rail × the tabletop 4-axis
//!   arm, the "合成滑轨臂" acceptance item — no public Panda URDF ships a
//!   prismatic chain) tracks rail + arm targets and the TCP moves
//!   exactly with the rail.
#![cfg(feature = "rigid3d")]

use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, World};
use oriera_subsys::rigid3d::robot_identity_hash;
use oriera_subsys::{
    sim_fingerprint, ArmDescription, FixtureSlab, Gravity, JointKind, JointSpec, JointTargets,
    JointTelemetry, LinkShape, LinkSpec, MultibodySystem, RobotSpec, Scene3D, TimeScale,
};

const DT: f32 = 1.0 / 60.0;
/// Rail mount, out of the table's footprint (table z ∈ ±0.32).
const RAIL: [f32; 3] = [0.0, 0.8, 0.8];

fn ctx(step: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(step),
        dt: DT,
        now: Timestamp(0),
        grants: Grants::EMPTY,
    }
}

/// A fixed rail + one 6 cm carriage on a prismatic joint along `axis`,
/// the carriage origin offset from the rail by `anchor` (consistent
/// chain: `origin[child] = origin[root] + anchor`). Struct-update from
/// the preset so future LinkSpec/RobotSpec fields inherit defaults.
fn slide_spec(axis: [f32; 3], anchor: [f32; 3], limits: (f32, f32)) -> RobotSpec {
    let preset = RobotSpec::from(&ArmDescription::default());
    let child_o = [
        RAIL[0] + anchor[0],
        RAIL[1] + anchor[1],
        RAIL[2] + anchor[2],
    ];
    let links = vec![
        LinkSpec {
            name: "rail".into(),
            shape: LinkShape::Box { half: [0.02; 3] },
            origin: RAIL,
            collider_offset: [0.0; 3],
            ..preset.links[0].clone()
        },
        LinkSpec {
            name: "carriage".into(),
            shape: LinkShape::Box { half: [0.03; 3] },
            origin: child_o,
            collider_offset: [0.0; 3],
            ..preset.links[1].clone()
        },
    ];
    let joints = vec![JointSpec {
        name: "slide".into(),
        kind: JointKind::Prismatic,
        parent_link: 0,
        child_link: 1,
        anchor,
        axis,
        limits,
        effort: None,
        velocity_limit: None,
        kp: 400.0, // N/m
        kd: 40.0,  // N·s/m
        armature: 0.02,
        damping: 0.5,
    }];
    RobotSpec {
        name: "slide-1dof".into(),
        base: RAIL,
        links,
        joints,
        gripper: None,
        home: vec![0.0],
        mass_scale: 1.0,
        action_scale: vec![0.1],
        ..preset
    }
}

fn bench(spec: RobotSpec) -> (World, MultibodySystem, Entity) {
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
    let sys = MultibodySystem::from_robot_fixtures(spec, &Scene3D::default().fixture_slabs())
        .expect("spec builds");
    (world, sys, arm)
}

/// Hold `targets` for `ticks` starting at tick `t0`; returns the final
/// telemetry.
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
    world
        .get::<JointTelemetry>(arm)
        .cloned()
        .expect("telemetry")
}

#[test]
fn prismatic_slide_follows_target_displacement() {
    // Non-trivial anchor: the readout must subtract it.
    let anchor = [0.05, 0.0, 0.0];
    let (mut world, mut sys, arm) = bench(slide_spec([1.0, 0.0, 0.0], anchor, (-0.2, 0.2)));
    let tel = hold(&mut world, &mut sys, arm, &[0.0], 0, 60);
    assert!(
        tel.angles[0].abs() < 1e-3,
        "settles at 0 m: {}",
        tel.angles[0]
    );
    let x0 = RAIL[0] + anchor[0];
    assert!(
        (tel.ee_pos[0] - x0).abs() < 1e-3,
        "carriage at its origin: {:?}",
        tel.ee_pos
    );

    for (k, want) in [0.12f32, -0.15, 0.03].into_iter().enumerate() {
        let tel = hold(&mut world, &mut sys, arm, &[want], 60 + 240 * k as u64, 240);
        let q = tel.angles[0];
        assert!(
            (q - want).abs() < 2e-3,
            "slide must reach {want} m, got {q}"
        );
        // Readout == the body's real displacement along the axis.
        let moved = tel.ee_pos[0] - x0;
        assert!(
            (moved - q).abs() < 1e-4,
            "readout {q} vs body displacement {moved}"
        );
        // Only the slide axis moves (the joint locks the other 5 DoF).
        assert!(
            (tel.ee_pos[1] - RAIL[1]).abs() < 1e-3,
            "y locked: {:?}",
            tel.ee_pos
        );
        assert!(
            (tel.ee_pos[2] - RAIL[2]).abs() < 1e-3,
            "z locked: {:?}",
            tel.ee_pos
        );
        assert!(
            tel.velocities[0].abs() < 0.01,
            "at rest: {} m/s",
            tel.velocities[0]
        );
    }
}

#[test]
fn prismatic_limits_and_velocity_limit_are_in_metres() {
    // A target past the stop is clamped to it (command clamp in metres).
    let (mut world, mut sys, arm) = bench(slide_spec([0.0, 0.0, 1.0], [0.0; 3], (-0.2, 0.2)));
    let tel = hold(&mut world, &mut sys, arm, &[0.5], 0, 300);
    assert!(
        (tel.angles[0] - 0.2).abs() < 3e-3,
        "parks at the 0.2 m stop: {}",
        tel.angles[0]
    );

    // velocity_limit = 0.1 m/s: after 30 ticks (0.5 s) the command has
    // moved at most 0.05 m, and it still converges.
    let mut spec = slide_spec([0.0, 0.0, 1.0], [0.0; 3], (-0.2, 0.2));
    spec.joints[0].velocity_limit = Some(0.1);
    let (mut world, mut sys, arm) = bench(spec);
    let tel = hold(&mut world, &mut sys, arm, &[0.1], 0, 30);
    assert!(
        tel.angles[0] <= 30.0 * 0.1 * DT + 5e-3,
        "rate-limited in m/s: {} m after 30 ticks",
        tel.angles[0]
    );
    let tel = hold(&mut world, &mut sys, arm, &[0.1], 30, 300);
    assert!(
        (tel.angles[0] - 0.1).abs() < 3e-3,
        "converges: {}",
        tel.angles[0]
    );
}

#[test]
fn vertical_slide_holds_gravity_until_the_effort_clamp() {
    let spec = slide_spec([0.0, 1.0, 0.0], [0.0; 3], (-0.15, 0.15));
    // carriage = 6 cm cube at the preset link density + added mass
    let mass = 1500.0 * 0.06f32.powi(3) + spec.links[1].added_mass;
    let sag = mass * 9.8 / spec.joints[0].kp;
    let (mut world, mut sys, arm) = bench(spec.clone());
    let tel = hold(&mut world, &mut sys, arm, &[0.05], 0, 300);
    eprintln!(
        "vertical slide: q = {:.4} m, predicted mg/kp sag {:.4} m",
        tel.angles[0], sag
    );
    assert!(
        (tel.angles[0] - (0.05 - sag)).abs() < 5e-3,
        "holds at target − mg/kp: {} vs {}",
        tel.angles[0],
        0.05 - sag
    );

    // effort below the weight (N): the motor saturates, the carriage
    // sinks onto its lower stop.
    let mut weak = spec;
    weak.joints[0].effort = Some(0.25 * mass * 9.8);
    let (mut world, mut sys, arm) = bench(weak);
    let tel = hold(&mut world, &mut sys, arm, &[0.05], 0, 300);
    assert!(
        (tel.angles[0] + 0.15).abs() < 0.01,
        "sinks to the −0.15 m stop: {}",
        tel.angles[0]
    );
}

#[test]
fn prismatic_identity_and_determinism() {
    // The 4-DoF preset's v2 identity is the pinned golden value
    // (contracts/golden/embodied_fingerprints.v2.json "identity_hash"):
    // the M3 kind fold must not touch a revolute robot.
    assert_eq!(
        robot_identity_hash(&RobotSpec::from(&ArmDescription::default())),
        9_458_848_781_451_220_370,
        "preset identity moved"
    );
    let slide = slide_spec([1.0, 0.0, 0.0], [0.0; 3], (-0.2, 0.2));
    let mut as_revolute = slide.clone();
    as_revolute.joints[0].kind = JointKind::Revolute;
    assert_ne!(
        robot_identity_hash(&slide),
        robot_identity_hash(&as_revolute),
        "kind is identity"
    );

    let run = || {
        let (mut world, mut sys, arm) = bench(slide.clone());
        let mut fps = Vec::new();
        for i in 0..240u64 {
            if i % 40 == 0 {
                let t = (i as f32 / 40.0) * 0.03 - 0.06;
                if let Some(tg) = world.get_mut::<JointTargets>(arm) {
                    tg.angles = vec![t];
                }
            }
            sys.run(&ctx(i), &mut world);
            fps.push(sim_fingerprint(&world));
        }
        fps
    };
    assert_eq!(run(), run(), "prismatic runs must be bit-identical");
}

/// The synthetic slide-arm: a fixed rail under the tabletop arm's base
/// column (now a dynamic carriage on a prismatic X joint), the legacy
/// 4 revolute stages re-indexed on top. The carriage collider is 5 mm
/// shorter at the bottom so it clears the table top while sliding.
fn slide_arm_spec() -> RobotSpec {
    let mut arm = RobotSpec::from(&ArmDescription::default());
    let column = arm.links[0].clone();
    let LinkShape::Box { half } = &column.shape else {
        unreachable!("preset base is a box")
    };
    let half = *half;
    let rail = LinkSpec {
        name: "rail".into(),
        shape: LinkShape::Box { half: [0.01; 3] },
        ..column.clone()
    };
    let carriage = LinkSpec {
        name: "carriage".into(),
        shape: LinkShape::Box {
            half: [half[0], half[1] - 0.005, half[2]],
        },
        collider_offset: [0.0, 0.005, 0.0],
        density: arm.links[1].density,
        friction: arm.links[1].friction,
        added_mass: 1.0,
        ..column
    };
    let mut links = vec![rail, carriage];
    links.extend(arm.links.drain(1..));
    let mut joints = vec![JointSpec {
        name: "rail_x".into(),
        kind: JointKind::Prismatic,
        parent_link: 0,
        child_link: 1,
        anchor: [0.0; 3],
        axis: [1.0, 0.0, 0.0],
        limits: (-0.05, 0.25),
        effort: Some(200.0),
        velocity_limit: None,
        kp: 600.0,
        kd: 60.0,
        armature: 0.2,
        damping: 1.0,
    }];
    for mut j in arm.joints.drain(..) {
        j.parent_link += 1;
        j.child_link += 1;
        joints.push(j);
    }
    let mut gripper = arm.gripper.take().expect("preset gripper");
    gripper.attach_link += 1;
    RobotSpec {
        name: "slide-arm-5dof".into(),
        links,
        joints,
        gripper: Some(gripper),
        home: vec![0.0; 5],
        action_scale: vec![0.1, 2.0, 2.0, 2.0, 2.0],
        ..arm
    }
}

#[test]
fn slide_arm_rail_carries_the_tabletop_arm() {
    let spec = slide_arm_spec();
    spec.validate().expect("slide-arm spec is valid");
    assert_eq!(spec.dof(), 5);
    let (mut world, mut sys, arm) = bench(spec);
    let pose = [0.0f32, 0.3, 0.6, -0.9, 0.2];
    let a = hold(&mut world, &mut sys, arm, &pose, 0, 300);
    let mut moved = pose;
    moved[0] = 0.15;
    let b = hold(&mut world, &mut sys, arm, &moved, 300, 300);
    for (k, (got, want)) in b.angles.iter().zip(moved).enumerate() {
        let tol = if k == 0 { 5e-3 } else { 0.08 };
        assert!(
            (got - want).abs() < tol,
            "joint {k}: got {got}, want {want}"
        );
    }
    // Same arm posture, rail moved 0.15 m along +X ⇒ the TCP translates
    // by exactly that (the arm's own sag is posture-only).
    let d = [
        b.ee_pos[0] - a.ee_pos[0],
        b.ee_pos[1] - a.ee_pos[1],
        b.ee_pos[2] - a.ee_pos[2],
    ];
    eprintln!("slide-arm TCP shift {d:?} (rail 0 → 0.15 m)");
    assert!((d[0] - 0.15).abs() < 0.01, "TCP follows the rail: {d:?}");
    assert!(
        d[1].abs() < 0.01 && d[2].abs() < 0.01,
        "no cross-talk: {d:?}"
    );
    assert!(b.velocities.iter().all(|v| v.is_finite()));
}

/// A 5-slab world (legacy pair + three extra slabs with their own
/// friction) is a plain `from_robot_fixtures` input for any robot,
/// prismatic or not.
#[test]
fn prismatic_robot_builds_in_a_multi_fixture_world() {
    let mut slabs = Scene3D::default().fixture_slabs();
    for (i, f) in [0.2f32, 0.6, 1.3].into_iter().enumerate() {
        slabs.push(FixtureSlab {
            center: [-0.8, 0.05, -0.6 + 0.6 * i as f32],
            half: [0.1, 0.05, 0.1],
            friction: f,
        });
    }
    let sys = MultibodySystem::from_robot_fixtures(slide_arm_spec(), &slabs);
    assert!(sys.is_ok(), "{:?}", sys.err());
}
