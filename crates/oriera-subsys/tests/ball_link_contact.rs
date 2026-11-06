//! P2 sphere → Ball 直通 — contact-behaviour guards (Ball semantics:
//! 球心 / 半径 / 接触行为) and the old-vs-new probe.
//!
//! Before P2 a URDF `<sphere radius=r>` link imported as the envelope
//! `LinkShape::Box { half: [r; 3] }`; P2 adds `LinkShape::Ball { radius }`
//! (rapier `ball`) and the importer maps spheres straight through. The
//! geometry the two share — the top point `centre + r·ŷ` — makes a
//! straight-down drop indistinguishable, so the guard probes both:
//!
//! 1. **centre + radius**: a sphere prop dropped straight onto the
//!    ball's top rests at `centre + (r + r_prop)·ŷ` (contact distance
//!    = r + r_prop) — true for Ball and envelope alike;
//! 2. **curvature (接触行为)**: the same prop dropped `0.5 r` off-centre
//!    rolls OFF a Ball (tilted contact normal) but sits still on the
//!    envelope's flat top. `cargo test … -- --nocapture` prints both
//!    final poses (the old-vs-new probe numbers).
//!
//! Fast layer (≈2×240 physics ticks on a 2-link robot): CI-visible.
#![cfg(feature = "rigid3d")]

use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use oriera_subsys::rigid3d::{JointTargets, JointTelemetry, MultibodySystem, Pose3, PropShape};
use oriera_subsys::{
    Gravity, JointKind, JointSpec, LinkShape, LinkSpec, PropPhys, RobotSpec, Scene3D,
    SelfCollision, SimObject, TimeScale,
};

const DT: f32 = 1.0 / 60.0;
/// Ball radius under test and the dropped sphere prop's radius (m).
const R: f32 = 0.04;
const R_PROP: f32 = 0.02;
/// Root body frame and the collider offset: the ball centre sits at
/// `ORIGIN + OFFSET` = (0, 0.30, 0) — a non-zero offset pins 球心.
const ORIGIN: [f32; 3] = [0.0, 0.25, 0.0];
const OFFSET: [f32; 3] = [0.0, 0.05, 0.0];
const CENTRE: [f32; 3] = [0.0, 0.30, 0.0];

fn ctx(step: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(step),
        dt: DT,
        now: Timestamp(0),
        grants: Grants::EMPTY,
    }
}

/// A 2-link probe robot: the fixed root carries the shape under test;
/// one revolute stub (vertical axis — gravity exerts no torque on it)
/// parks 0.5 m away so nothing but the root meets the prop.
fn probe_robot(root_shape: LinkShape) -> RobotSpec {
    let link = |name: &str, shape: LinkShape, origin: [f32; 3], off: [f32; 3]| LinkSpec {
        name: name.into(),
        shape,
        origin,
        collider_offset: off,
        density: 1500.0,
        friction: 0.7,
        added_mass: 0.0,
        inertial: None,
        mesh_uri: None,
    };
    RobotSpec {
        name: "ball-probe".into(),
        base: ORIGIN,
        links: vec![
            link("root", root_shape, ORIGIN, OFFSET),
            link(
                "stub",
                LinkShape::Box { half: [0.02; 3] },
                [0.5, ORIGIN[1], 0.0],
                [0.0; 3],
            ),
        ],
        joints: vec![JointSpec {
            name: "stub_yaw".into(),
            kind: JointKind::Revolute,
            parent_link: 0,
            child_link: 1,
            anchor: [0.5, 0.0, 0.0],
            axis: [0.0, 1.0, 0.0],
            limits: (-1.0, 1.0),
            effort: None,
            velocity_limit: None,
            kp: 60.0,
            kd: 4.0,
            armature: 0.02,
            damping: 0.05,
        }],
        gripper: None,
        home: vec![0.0],
        mass_scale: 1.0,
        action_scale: vec![2.0],
        self_collision: SelfCollision::Off,
    }
}

/// Drop a sphere prop from 5 mm above contact height at horizontal
/// offset `dx` from the ball centre; return its pose after `ticks`.
fn drop_prop(root_shape: LinkShape, dx: f32, ticks: u64) -> [f32; 3] {
    let mut world = World::new();
    world.insert_resource(Gravity(9.8));
    world.insert_resource(TimeScale(1.0));
    let arm = world.spawn();
    world.insert(
        arm,
        JointTargets {
            angles: vec![0.0],
            gripper: 0.0,
        },
    );
    world.insert(arm, JointTelemetry::default());
    let prop = world.spawn();
    world.insert(
        prop,
        Pose3 {
            pos: [CENTRE[0] + dx, CENTRE[1] + R + R_PROP + 0.005, CENTRE[2]],
            ..Default::default()
        },
    );
    world.insert(prop, SimObject);
    world.insert(prop, PropShape::Sphere { r: R_PROP });
    world.insert(prop, PropPhys::default());
    let scene = Scene3D {
        ground: ([0.0, -0.05, 0.0], [2.0, 0.05, 1.5]),
        table: None,
    };
    let robot = probe_robot(root_shape);
    robot.validate().expect("probe robot validates");
    let mut sys = MultibodySystem::from_robot(robot, scene).expect("probe robot builds");
    for i in 0..ticks {
        sys.run(&ctx(i), &mut world);
    }
    world.get::<Pose3>(prop).expect("prop pose").pos
}

fn horiz(p: [f32; 3]) -> f32 {
    ((p[0] - CENTRE[0]).powi(2) + (p[2] - CENTRE[2]).powi(2)).sqrt()
}

/// 球心 + 半径: a straight-down drop rests at contact distance
/// `R + R_PROP` above the ball centre (collider_offset honoured).
#[test]
fn ball_link_contact_distance_is_centre_plus_radii() {
    let p = drop_prop(LinkShape::Ball { radius: R }, 0.0, 90);
    let rest_y = CENTRE[1] + R + R_PROP;
    eprintln!(
        "[ball-probe] straight drop on Ball{{r={R}}}: prop at {p:?} (want y {rest_y:.4}, centre axis)"
    );
    assert!(
        (p[1] - rest_y).abs() < 2e-3,
        "prop must rest at centre + R + r_prop = {rest_y}: {p:?}"
    );
    assert!(horiz(p) < 2e-3, "stays on the vertical axis: {p:?}");
}

/// 接触行为 (old vs new): 0.5 R off-centre, the prop rolls off the Ball
/// (curved contact) but sits on the old `Box{[R;3]}` envelope's flat top.
#[test]
fn ball_link_curvature_rolls_off_where_the_box_envelope_held() {
    let dx = 0.5 * R;
    let ticks = 240;
    let rest_y = CENTRE[1] + R + R_PROP;
    let new = drop_prop(LinkShape::Ball { radius: R }, dx, ticks);
    let old = drop_prop(LinkShape::Box { half: [R; 3] }, dx, ticks);
    eprintln!(
        "[ball-probe] off-centre dx={dx:.3} after {ticks} ticks — \
         NEW Ball: prop {new:?} (horiz {:.4} m, Δy {:+.4} m) | \
         OLD Box{{[r;3]}} envelope: prop {old:?} (horiz {:.4} m, Δy {:+.4} m)",
        horiz(new),
        new[1] - rest_y,
        horiz(old),
        old[1] - rest_y
    );
    // old envelope: flat top holds the prop where it landed
    assert!(
        (old[1] - rest_y).abs() < 2e-3 && (horiz(old) - dx).abs() < 5e-3,
        "Box envelope: prop sits on the flat top at dx: {old:?}"
    );
    // new Ball: the tilted normal rolls it off the top and down
    assert!(
        new[1] < rest_y - 0.02 && horiz(new) > R,
        "Ball: prop must roll off the sphere: {new:?}"
    );
}

/// Same-build determinism holds on the new collider path too.
#[test]
fn ball_link_drop_is_bit_deterministic() {
    let a = drop_prop(LinkShape::Ball { radius: R }, 0.5 * R, 120);
    let b = drop_prop(LinkShape::Ball { radius: R }, 0.5 * R, 120);
    assert_eq!(
        a.map(f32::to_bits),
        b.map(f32::to_bits),
        "two runs must be bit-identical"
    );
}
