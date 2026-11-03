//! 接触抓取机制守卫（round 4 定案，2026-11-01 转正）：运动学夹板以
//! 2mm 挤压量 + 摩擦能否拖动动力学方块？这是 rigid3d 双指夹板抓取
//! 的物理底座——夹板几何/摩擦系数变动时，此测试先于 12 集验收报警。

use rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;

#[test]
fn kinematic_plates_drag_cube_via_friction() {
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
    let rise = final_y - 0.5;
    println!("VERDICT: cube rose {:.4} m", rise);
    // 摩擦承载成立的最小证据：夹板 2s 走 0.161 m，方块必须被拖走大半程
    //（实测 ≈0.160 全程贴合）；带上界——JS 侧实测过「接触炮」（方块被
    // 挤压弹射数米）也能过纯下限闸，上下界把弹射挡在门外（审查 P2）。
    assert!(
        (0.10..0.25).contains(&rise),
        "friction drag out of band: cube rose {rise:.4} m (expect 0.10..0.25)"
    );
}
