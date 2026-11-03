//! M2 acceptance (PLAN-embodied-general §四 M2 / §9.1 U-6): the
//! contact-level grasp criterion. The M0b down-gripper scenario
//! (`crates/oriera-agent/tests/gripper_down_smoke.rs`, engine contract
//! pinned there with the same hand-written spec) is replayed with
//! `GripperSpec::holding_criterion = Contact { min_normal_force,
//! max_slip }`:
//!
//! - `holding` must be FALSE while the demand is closing but the plates
//!   have not yet touched the cube (the window where the v1 tip-radius
//!   heuristic already says "held" — the exact false-label complaint
//!   U-6 is about);
//! - TRUE once both plates press the same prop with ≥ `min_normal_force`
//!   and slip ≤ `max_slip`, and stay TRUE through the lift;
//! - FALSE again after release;
//! - same-seed episodes must be bit-identical (determinism contract);
//! - agreement with the TipRadius criterion on the success set is
//!   REPORTED (人话数字 for the migration table), not pinned — the two
//!   labels differ by design in the pre-touch window.
#![cfg(feature = "rigid3d")]

use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use oriera_subsys::rigid3d::{JointTargets, JointTelemetry, MultibodySystem, Pose3};
use oriera_subsys::{
    Gravity, GripperKind, GripperSpec, HoldingCriterion, JointSpec, LinkShape, LinkSpec, RobotSpec,
    SelfCollision, SimObject, TimeScale,
};
use std::f32::consts::PI;

const DT: f32 = 1.0 / 60.0;
/// Table-top rest height of the 5 cm cube's centre (table top 0.5).
const CY: f32 = 0.525;
/// Cube spawn x (directly under the home TCP).
const CX: f32 = 0.40;
/// Contact criterion thresholds: 1 N per plate (the 50 g cube needs
/// ≈0.18 N per plate through μ=1.4 friction — the threshold sits well
/// above gravity load but far below the squeeze force), 5 cm/s slip.
const MIN_NORMAL_FORCE: f32 = 1.0;
const MAX_SLIP: f32 = 0.05;

/// The M0b seven-axis down-gripper robot, verbatim scenario constants
/// (gripper_down_smoke.rs is the agent-side acceptance; this file pins
/// the ENGINE contract for the Contact criterion on the same robot).
fn seven_axis_down_gripper_spec(criterion: HoldingCriterion) -> RobotSpec {
    let sx = 0.02f32;
    let sy = 0.62f32;
    let base_h = 0.06f32;
    let r = 0.02f32;
    let link = |name: &str, half: [f32; 3], origin: [f32; 3], off: [f32; 3]| LinkSpec {
        name: name.into(),
        shape: LinkShape::Box { half },
        origin,
        collider_offset: off,
        density: 1500.0,
        friction: 0.7,
        added_mass: 0.2,
        inertial: None,
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
            inertial: None,
            mesh_uri: None,
        },
        link("carrier", [0.015; 3], [sx, sy, 0.0], [0.0; 3]),
        link("upper_arm", [0.07, r, r], [sx, sy, 0.0], [0.07, 0.0, 0.0]),
        link("forearm", [0.06, r, r], [sx + 0.14, sy, 0.0], [0.06, 0.0, 0.0]),
        link("wrist", [0.03, r, r], [sx + 0.26, sy, 0.0], [0.03, 0.0, 0.0]),
        link("roll_seg", [0.015, r, r], [sx + 0.32, sy, 0.0], [0.015, 0.0, 0.0]),
        link("yaw_seg", [0.015, r, r], [sx + 0.35, sy, 0.0], [0.015, 0.0, 0.0]),
        link("flange", [0.01, r, r], [sx + 0.38, sy, 0.0], [0.01, 0.0, 0.0]),
    ];
    let j = |name: &str, p: usize, c: usize, anchor: [f32; 3], axis: [f32; 3], limits: (f32, f32)| {
        JointSpec {
            name: name.into(),
            kind: oriera_subsys::JointKind::Revolute,
            parent_link: p,
            child_link: c,
            anchor,
            axis,
            limits,
            effort: None,
            velocity_limit: None,
            kp: 400.0,
            kd: 20.0,
            armature: 0.02,
            damping: 0.05,
        }
    };
    let joints = vec![
        j("base_yaw", 0, 1, [0.0, 0.12 - base_h, 0.0], [0.0, 1.0, 0.0], (-PI, PI)),
        j("shoulder", 1, 2, [0.0; 3], [0.0, 0.0, 1.0], (-0.35, 1.45)),
        j("elbow", 2, 3, [0.14, 0.0, 0.0], [0.0, 0.0, 1.0], (-2.5, 0.35)),
        j("wrist_pitch", 3, 4, [0.12, 0.0, 0.0], [0.0, 0.0, 1.0], (-2.0, 2.0)),
        j("wrist_roll", 4, 5, [0.06, 0.0, 0.0], [1.0, 0.0, 0.0], (-PI, PI)),
        j("wrist_yaw", 5, 6, [0.03, 0.0, 0.0], [0.0, 1.0, 0.0], (-2.0, 2.0)),
        j("tool_pitch", 6, 7, [0.03, 0.0, 0.0], [0.0, 0.0, 1.0], (-2.0, 2.0)),
    ];
    let gripper = GripperSpec {
        attach_link: 7,
        finger_x: 0.06,
        tcp_offset: [0.0, 0.0, -0.06],
        finger_half: [0.030, 0.006, 0.045],
        gap_open: 0.075,
        gap_closed: 0.046,
        friction: 1.4,
        effort: 20.0,
        gap_rate: 0.004,
        kind: GripperKind::ParallelPlates,
        approach_axis: Some([0.0, 0.0, -1.0]),
        gap_axis: Some([0.0, 1.0, 0.0]),
        contact_mesh: false,
        holding_criterion: criterion,
    };
    RobotSpec {
        name: "tabletop-7dof-down-gripper-m2".into(),
        base: [sx, 0.5, 0.0],
        links,
        joints,
        gripper: Some(gripper),
        home: vec![0.0; 7],
        mass_scale: 1.0,
        action_scale: vec![2.0; 7],
        self_collision: SelfCollision::Off,
    }
}

fn ctx(step: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(step),
        dt: DT,
        now: Timestamp(0),
        grants: Grants::EMPTY,
    }
}

fn pose_targets(a_deg: f32, roll: f32) -> Vec<f32> {
    let a = a_deg.to_radians();
    vec![0.0, a, -a, 0.0, roll, 0.0, 0.0]
}

fn home_targets() -> Vec<f32> {
    vec![0.0, 0.0, 0.0, 0.0, -std::f32::consts::FRAC_PI_2, 0.0, 0.0]
}

fn lift_targets() -> Vec<f32> {
    pose_targets(30.0, -std::f32::consts::FRAC_PI_2)
}

/// The pick episode: approach high → roll at height → descend → close →
/// lift → release. Returns the per-tick `(holding, demand>0.5, cube_y)`
/// timeline plus the phase boundaries (start index of close / lift /
/// release).
fn run_episode(criterion: HoldingCriterion) -> (Vec<(bool, bool, f32)>, [usize; 3]) {
    let (mut world, mut sys, arm, obj) = {
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
                pos: [CX, CY, 0.0],
                ..Default::default()
            },
        );
        world.insert(obj, SimObject);
        let sys = MultibodySystem::from_robot(seven_axis_down_gripper_spec(criterion), {
            oriera_subsys::Scene3D::default()
        })
        .expect("down-gripper spec builds");
        (world, sys, arm, obj)
    };
    let phases: Vec<(Vec<f32>, u64, f32, bool)> = vec![
        (pose_targets(60.0, 0.0), 80, 0.0, false),
        (pose_targets(60.0, -std::f32::consts::FRAC_PI_2), 80, 0.0, false),
        (home_targets(), 120, 0.0, false),
        (home_targets(), 80, 1.0, true),   // close
        (lift_targets(), 150, 1.0, true),  // lift
        (lift_targets(), 100, 0.0, false), // release
    ];
    let mut timeline = Vec::new();
    let mut bounds = [0usize; 3];
    let mut i = 0u64;
    for (pi, (targets, ticks, grip, is_hold_phase)) in phases.iter().enumerate() {
        if (3..=5).contains(&pi) {
            bounds[pi - 3] = timeline.len();
        }
        for _ in 0..*ticks {
            if let Some(t) = world.get_mut::<JointTargets>(arm) {
                t.angles = targets.clone();
                t.gripper = *grip;
            }
            sys.run(&ctx(i), &mut world);
            i += 1;
            let holding = world
                .get::<JointTelemetry>(arm)
                .map(|t| t.holding)
                .unwrap_or(false);
            let cube_y = world.get::<Pose3>(obj).map(|p| p.pos[1]).unwrap_or(f32::NAN);
            timeline.push((holding, *is_hold_phase, cube_y));
        }
        let _ = is_hold_phase;
    }
    (timeline, bounds)
}

#[test]
#[allow(clippy::needless_range_loop)]
fn contact_criterion_labels_the_grasp_by_real_contact() {
    let (tl, b) = run_episode(HoldingCriterion::Contact {
        min_normal_force: MIN_NORMAL_FORCE,
        max_slip: MAX_SLIP,
    });
    let (tl_v1, _) = run_episode(HoldingCriterion::TipRadius);

    // 1. Pre-touch window: the demand is 1.0 from the close phase's
    //    first tick, but the plates need ~1 tick/tick·0.004 m to reach
    //    the cube. TipRadius labels "held" immediately (cube centre is
    //    0 mm from the TCP — it sat under the tip all along); Contact
    //    must stay false until real pressure exists.
    let close_start = b[0];
    let v1_true_early = tl_v1[close_start..close_start + 10]
        .iter()
        .filter(|(h, d, _)| *h && *d)
        .count();
    let contact_true_early = tl[close_start..close_start + 10]
        .iter()
        .filter(|(h, _, _)| *h)
        .count();
    eprintln!(
        "U-6 discriminator, first 10 close ticks: TipRadius holding in {v1_true_early}/10, \
         Contact holding in {contact_true_early}/10"
    );
    assert!(
        v1_true_early > contact_true_early,
        "the pre-touch window is the whole point: TipRadius must label sooner than Contact \
         (v1 {v1_true_early} vs contact {contact_true_early})"
    );

    // 2. Squeeze established: over the last 30 close ticks the contact
    //    criterion must read held.
    let close_end = b[1];
    let squeeze = tl[close_end - 30..close_end].iter().filter(|(h, _, _)| *h).count();
    assert!(
        squeeze >= 25,
        "contact holding must be stable once the squeeze is established, got {squeeze}/30"
    );

    // 3. The cube rides the gripper (friction carry is untouched by the
    //    label change — same physics, different telemetry).
    let lift_end = b[2];
    let carried = tl[lift_end - 1].2;
    assert!(
        carried > CY + 0.04,
        "cube must ride the gripper through the lift, got y={carried:.3} (rest {CY})"
    );
    // 4. Release: holding false again (demand gate + plates open).
    let release_true = tl[lift_end..].iter().filter(|(h, _, _)| *h).count();
    assert_eq!(release_true, 0, "release must clear the contact label");

    // 5. Agreement report vs the v1 heuristic (成功集上一致率 — a
    //    migration-table number, not a gate; the two criteria disagree
    //    by design only in the pre-touch window).
    let hold_len = lift_end - close_start;
    let mut both = 0usize;
    let mut v1_only = 0usize;
    let mut contact_only = 0usize;
    for i in close_start..lift_end {
        match (tl[i].0, tl_v1[i].0) {
            (true, true) => both += 1,
            (false, true) => v1_only += 1,
            (true, false) => contact_only += 1,
            _ => {}
        }
    }
    eprintln!(
        "agreement over close+lift ({hold_len} ticks): both={both} tip_radius_only={v1_only} \
         contact_only={contact_only} — v1 share of contact labels: {:.1}%",
        100.0 * both as f32 / (both + contact_only).max(1) as f32
    );

    // 6. Determinism: the same criterion replays bit-identically.
    let (tl2, _) = run_episode(HoldingCriterion::Contact {
        min_normal_force: MIN_NORMAL_FORCE,
        max_slip: MAX_SLIP,
    });
    assert_eq!(
        tl, tl2,
        "same spec+seed ⇒ bit-identical holding timeline"
    );
}

/// The spec-level gate: validation refuses a Contact criterion with a
/// negative threshold, and the identity hash moves only when the
/// criterion is non-default (golden-stability discipline).
#[test]
fn contact_criterion_is_opt_in_and_validated() {
    let ok = seven_axis_down_gripper_spec(HoldingCriterion::default());
    assert!(ok.validate().is_ok());
    let h0 = oriera_subsys::rigid3d::robot_identity_hash(&ok);
    let mut bad = ok.clone();
    bad.gripper.as_mut().unwrap().holding_criterion =
        HoldingCriterion::Contact { min_normal_force: -1.0, max_slip: 0.0 };
    assert!(bad.validate().is_err(), "negative force threshold refused");

    let mut contact = ok.clone();
    contact.gripper.as_mut().unwrap().holding_criterion =
        HoldingCriterion::Contact { min_normal_force: 1.0, max_slip: 0.05 };
    contact.validate().expect("valid contact criterion");
    assert_ne!(
        oriera_subsys::rigid3d::robot_identity_hash(&contact),
        h0,
        "Contact criterion is v2 identity"
    );
    // TipRadius spelled out == omitted (default-omission discipline).
    let mut explicit = ok.clone();
    explicit.gripper.as_mut().unwrap().holding_criterion = HoldingCriterion::TipRadius;
    assert_eq!(oriera_subsys::rigid3d::robot_identity_hash(&explicit), h0);
}
