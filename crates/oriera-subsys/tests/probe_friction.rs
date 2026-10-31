//! TEMP probe (round 4): do kinematic plates drag a dynamic cube via
//! friction in rapier 3D? Delete after the contact-grasp decision.

use rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;

#[test]
fn probe_kinematic_friction_drag() {
    let mut pipeline = PhysicsPipeline::new();
    let mut islands = IslandManager::new();
    let mut bp = DefaultBroadPhase::new();
    let mut np = NarrowPhase::new();
    let mut bodies = RigidBodySet::new();
    let mut colliders = ColliderSet::new();
    let mut ij = ImpulseJointSet::new();
    let mut mj = MultibodyJointSet::new();
    let mut sb = SoftBodySet::new();
    let mut ccd = CCDSolver::new();

    // cube: 5cm, density 400 (the bench prop), resting in mid-air? No:
    // give it a fixed floor just below so it doesn't fall during the
    // squeeze — we're testing HORIZONTAL drag along +Y by two vertical
    // plates pressing on ±Z faces and sliding along +Y.
    let cube = bodies.insert(
        RigidBodyBuilder::dynamic()
            .additional_mass(0.0)
            .translation(Vec3::new(0.0, 0.5, 0.0))
            .build(),
    );
    colliders.insert_with_parent(
        ColliderBuilder::cuboid(0.025, 0.025, 0.025)
            .density(400.0)
            .friction(0.8)
            .build(),
        cube,
        &mut bodies,
    );
    let floor = bodies.insert(
        RigidBodyBuilder::fixed()
            .translation(Vec3::new(0.0, 0.475 - 0.05, 0.0))
            .build(),
    );
    colliders.insert_with_parent(
        ColliderBuilder::cuboid(0.5, 0.025, 0.5)
            .friction(0.9)
            .build(),
        floor,
        &mut bodies,
    );

    // plates: kinematic, inner faces at z=±0.023 (2mm squeeze), sliding +Y
    let mk = |bodies: &mut RigidBodySet, colliders: &mut ColliderSet, z: f32| {
        let b = bodies.insert(
            RigidBodyBuilder::kinematic_position_based()
                .translation(Vec3::new(0.0, 0.5, z))
                .build(),
        );
        colliders.insert_with_parent(
            ColliderBuilder::cuboid(0.030, 0.075, 0.006)
                .friction(1.4)
                .build(),
            b,
            bodies,
        );
        b
    };
    let pl = mk(&mut bodies, &mut colliders, 0.023);
    let pr = mk(&mut bodies, &mut colliders, -0.023);

    let speed = 0.08_f32; // m/s upward
    for i in 0..121 {
        let y = 0.5 + speed * (i as f32 * DT);
        for b in [pl, pr] {
            let z = bodies.get(b).unwrap().translation().z;
            bodies
                .get_mut(b)
                .unwrap()
                .set_next_kinematic_translation(Vec3::new(0.0, y, z));
        }
        let params = IntegrationParameters {
            dt: DT,
            ..Default::default()
        };
        pipeline.step(
            Vec3::new(0.0, -9.8, 0.0),
            &params,
            &mut islands,
            &mut bp,
            &mut np,
            &mut bodies,
            &mut colliders,
            &mut ij,
            &mut mj,
            &mut sb,
            &mut ccd,
            &(),
            &(),
        );
        if i % 30 == 0 {
            let c = bodies.get(cube).unwrap().translation();
            let mut n_contacts = 0;
            for ch in bodies.get(cube).unwrap().colliders() {
                n_contacts += np.contact_pairs_with(*ch).count();
            }
            println!("i={i:3} cube_y={:.4} contacts={n_contacts}", c.y);
        }
    }
    let final_y = bodies.get(cube).unwrap().translation().y;
    println!("VERDICT: cube rose {:.4} m", final_y - 0.5);
}
