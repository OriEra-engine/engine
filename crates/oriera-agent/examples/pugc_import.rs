//! PUGC review scratch example (NOT part of the shipped examples):
//! hand-written 6-axis arm + integrated two-finger gripper URDF →
//! `UrdfReport::to_robot_spec` full-chain import → lab SceneSpec JSON.
//! Mirrors what an external robotics-company engineer must do today,
//! because `--urdf` in embodied_sim_collect only runs the old 4-axis
//! `first_revolute_chain(4)` projection.

use oriera_agent::spec::SceneSpec;

fn main() {
    let urdf_path = std::env::args()
        .nth(1)
        .expect("usage: pugc_import <URDF> <out.json> [mesh_root]");
    let out_path = std::env::args()
        .nth(2)
        .expect("usage: pugc_import <URDF> <out.json> [mesh_root]");
    let mesh_root = std::env::args().nth(3);

    let xml = std::fs::read_to_string(&urdf_path).expect("read urdf");
    let rep = oriera_agent::urdf::UrdfReport::parse(&xml).expect("parse urdf");

    let mut opts = oriera_agent::urdf::RobotSpecOpts {
        default_kp: 60.0,
        default_kd: 4.0,
        default_armature: 0.02,
        mount: [0.02, 0.5, 0.0],
        ..Default::default()
    };
    if let Ok(mount) = std::env::var("ORIERA_MOUNT") {
        let m: Vec<f32> = mount
            .split(',')
            .map(|v| v.trim().parse::<f32>().expect("mount float"))
            .collect();
        opts.mount = [m[0], m[1], m[2]];
    }
    if let Some(root) = &mesh_root {
        opts.mesh_base_dir = Some(std::path::PathBuf::from(root));
        opts.mesh_packages.insert(
            "acme_xr6s_meshes".to_string(),
            std::path::PathBuf::from(root),
        );
    }

    let mut robot = rep
        .to_robot_spec(&mut opts)
        .unwrap_or_else(|e| panic!("to_robot_spec failed: {e}"));
    robot
        .validate()
        .unwrap_or_else(|e| panic!("robot invalid: {e}"));

    // tuning overrides for the review iteration loop
    if let Ok(home) = std::env::var("ORIERA_HOME") {
        robot.home = home
            .split(',')
            .map(|v| v.trim().parse::<f32>().expect("home float"))
            .collect();
        if robot.home.len() != robot.dof() {
            panic!("home len {} != dof {}", robot.home.len(), robot.dof());
        }
    }
    if let Ok(gc) = std::env::var("ORIERA_GAP_CLOSED") {
        if let Some(g) = robot.gripper.as_mut() {
            g.gap_closed = gc.parse::<f32>().expect("gap_closed float");
        }
    }
    if let Ok(go_) = std::env::var("ORIERA_GAP_OPEN") {
        if let Some(g) = robot.gripper.as_mut() {
            g.gap_open = go_.parse::<f32>().expect("gap_open float");
        }
    }

    println!("imported robot: {} / {} DoF", robot.name, robot.dof());
    println!("home = {:?}", robot.home);
    if let Some(g) = &robot.gripper {
        println!(
            "gripper: gap_open {} gap_closed {} finger_x {} approach {:?} gap {:?}",
            g.gap_open, g.gap_closed, g.finger_x, g.approach_axis, g.gap_axis
        );
    } else {
        println!("gripper: NONE (mimic/prismatic pair NOT detected)");
    }
    println!("--- skipped report ({} entries) ---", opts.skipped.len());
    for s in &opts.skipped {
        println!("  · {s}");
    }

    let golden = SceneSpec::load_file(std::path::Path::new(
        "crates/oriera-agent/specs/official/tabletop-4dof-pick.json",
    ))
    .expect("golden spec load")
    .spec;
    let spec = SceneSpec {
        schema: golden.schema.clone(),
        meta: oriera_agent::spec::SpecMeta {
            name: "acme-xr6s-pick".into(),
            author: Some("pugc-review".into()),
            notes: Some("6-axis URDF full-chain import review artifact".into()),
            difficulty: Some("进阶".into()),
        },
        robot,
        scene: golden.scene.clone(),
        task: golden.task.clone(),
        control: golden.control.clone(),
        dt: golden.dt,
        max_steps: golden.max_steps,
        randomization: golden.randomization.clone(),
        physics: golden.physics,
    };
    spec.validate()
        .unwrap_or_else(|e| panic!("spec invalid: {e}"));
    std::fs::write(&out_path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    println!("spec → {out_path} (spec_hash {})", spec.spec_hash_hex());
}
