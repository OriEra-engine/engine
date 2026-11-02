//! U4 全链验收（PLAN-embodied-general M0a 工作包 D；docs/URDF-IMPORT.md）：
//!
//! - `tests/fixtures/panda7.urdf`（menagerie rev b065505 换算件 + mesh
//!   引用 + datasheet velocity）→ [`UrdfReport::to_robot_spec`] → 7 轴
//!   [`RobotSpec`] → [`MultibodySystem::from_robot`] 构建并跟踪限位内目标；
//! - `tests/fixtures/gripper2f.urdf`（手写外部两指夹爪，mimic 耦合）→
//!   [`GripperSpec`] 检测，并**挂到 panda7** 的工具法兰上构建、按 spec 落板；
//! - 不退化：4 轴 fixture 的 `to_robot_spec` 关节数与
//!   `RobotSpec::from(&first_revolute_chain(4))` 一致。
//!
//! 整文件用 `#![cfg]` 门控：不带 `rigid3d,urdf` 特性时是空测试二进制
//! （cargo 自动发现 tests/*.rs，无 [[test]] 登记可加）。

#![cfg(all(feature = "rigid3d", feature = "urdf"))]

use oriera_agent::urdf::{RobotSpecOpts, UrdfReport};
use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use oriera_subsys::{
    JointTargets, JointTelemetry, LinkShape, LinkSpec, MultibodySystem, RobotSpec, Scene3D, TimeScale,
};

const DT: f32 = 1.0 / 60.0;
const PANDA: &str = "tests/fixtures/panda7.urdf";
const GRIPPER: &str = "tests/fixtures/gripper2f.urdf";

fn fixture(name: &str) -> UrdfReport {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    let xml = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读 {} 失败：{e}", path.display()));
    UrdfReport::parse(&xml).unwrap_or_else(|e| panic!("{} 解析失败：{e}", path.display()))
}

fn ctx(step: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(step),
        dt: DT,
        now: Timestamp(0),
        grants: Grants::EMPTY,
    }
}

/// Panda 专用 opts：默认 60/4 增益是 4 轴轻臂标定，11 kg 级 7 轴臂在重力下
/// 稳态误差 τ_g/kp ≈ 0.2 rad 会越线——这正是 opts 存在的意义（menagerie
/// 本身给 armature=0.1）。验收命令照抄即可复现。
fn panda_opts() -> RobotSpecOpts {
    RobotSpecOpts {
        default_kp: 300.0,
        default_kd: 30.0,
        default_armature: 0.05,
        ..RobotSpecOpts::default()
    }
}

/// Panda spec 的形状体积——质量等价断言用（与 urdf.rs 的包络推导同一公式）。
fn spec_volume(l: &LinkSpec) -> f32 {
    match l.shape {
        LinkShape::Box { half } => 8.0 * half[0] * half[1] * half[2],
        LinkShape::Capsule { half_len, radius } => {
            std::f32::consts::PI * radius * radius * (2.0 * half_len)
                + 4.0 / 3.0 * std::f32::consts::PI * radius.powi(3)
        }
    }
}

/// 7 轴限位/effort/velocity/damping 对照表（URDF 逐字面值）。验收报告附表。
const PANDA_JOINT_TABLE: [(&str, f32, f32, f32, f32, f32); 7] = [
    // (name, lower, upper, effort, velocity, damping)
    ("joint1", -2.8973, 2.8973, 87.0, 2.1750, 1.0),
    ("joint2", -1.7628, 1.7628, 87.0, 2.1750, 1.0),
    ("joint3", -2.8973, 2.8973, 87.0, 2.1750, 1.0),
    ("joint4", -3.0718, -0.0698, 87.0, 2.1750, 1.0),
    ("joint5", -2.8973, 2.8973, 12.0, 2.6100, 1.0),
    ("joint6", -0.0175, 3.7525, 12.0, 2.6100, 1.0),
    ("joint7", -2.8973, 2.8973, 12.0, 2.6100, 1.0),
];

const PANDA_MASS: [f32; 8] = [
    0.629769, 4.970684, 0.646926, 3.228604, 3.587895, 1.225946, 1.666555, 0.735522,
];

/// 7 个限位内目标（joint4 上限 −0.0698、joint6 下限 −0.0175 都避开）。
const PANDA_TARGETS: [f32; 7] = [0.35, -0.45, 0.55, -1.55, 0.5, 1.55, -0.55];

#[test]
fn panda7_maps_to_a_full_robot_spec() {
    let rep = fixture(PANDA);
    // 链校验（既有 API）：7 轴零 issue
    let issues = rep.validate_chain(7);
    assert!(issues.is_empty(), "validate_chain(7): {issues:?}");

    let mut opts = panda_opts();
    let spec = rep.to_robot_spec(&mut opts).expect("to_robot_spec");
    assert_eq!(spec.name, "panda7");
    spec.validate().expect("spec 结构合法（mesh_uri 保持 None）");

    // 7 关节 + 8 链体（attachment 被 fixed 吸收进 link7）
    assert_eq!(spec.joints.len(), 7, "{:?}", spec.joint_names());
    assert_eq!(spec.links.len(), 8);
    assert_eq!(spec.root_link(), Some(0));
    assert_eq!(spec.dof(), 7);

    // 逐关节对照表：limits / effort / velocity / damping 全接
    for (row, j) in PANDA_JOINT_TABLE.iter().zip(&spec.joints) {
        assert_eq!(j.name, row.0, "关节顺序 = URDF 链序");
        let (lo, hi) = j.limits;
        assert!((lo - row.1).abs() < 1e-5 && (hi - row.2).abs() < 1e-5, "{}: {:?}", j.name, j.limits);
        assert_eq!(j.effort, Some(row.3), "{} effort", j.name);
        assert_eq!(j.velocity_limit, Some(row.4), "{} velocity（URDF 此前被丢弃）", j.name);
        assert_eq!(j.damping, row.5, "{} damping", j.name);
        assert_eq!((j.kp, j.kd, j.armature), (300.0, 30.0, 0.05), "{} 用 opts 增益", j.name);
    }
    // Panda 特色：joint4 限位不含零 → home 钳到 −0.0698，其余零位
    assert!((spec.home[3] - (-0.0698)).abs() < 1e-5, "{:?}", spec.home);
    assert!(spec.home.iter().enumerate().all(|(i, h)| i == 3 || h.abs() < 1e-6));

    // 轴：Z-up→Y-up 后 joint1/3/5 竖直（OriEra +Y，yaw 式），joint2/4/6/7 横向
    // （f32 三角函数有 ~1e-8 级噪声，全部按容差断言）
    let near = |a: [f32; 3], b: [f32; 3]| {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
    };
    let ax = |i: usize| spec.joints[i].axis;
    assert!(near(ax(1), [0.0, 0.0, -1.0]), "joint2 轴（URDF +Y → OriEra −Z）：{:?}", ax(1));
    assert!(near(ax(3), [0.0, 0.0, 1.0]), "joint4 轴：{:?}", ax(3));
    assert!(near(ax(5), [0.0, 0.0, 1.0]), "joint6 轴：{:?}", ax(5));
    assert!(near(ax(6), [0.0, -1.0, 0.0]), "joint7 轴（法兰零位朝下，URDF −Z → OriEra −Y）：{:?}", ax(6));
    for i in [0, 2, 4, 6] {
        assert!((ax(i)[1].abs() - 1.0).abs() < 1e-5, "joint{} 轴 {:?} 应竖直", i + 1, ax(i));
    }
    for i in [1, 3, 5] {
        assert!(ax(i)[1].abs() < 1e-5, "joint{} 轴 {:?} 应横向", i + 1, ax(i));
    }

    // mesh 诚实降级：8/8 链体降级、mesh_uri 全 None、URI 进 skipped（M2 消费）
    let mesh_rows: Vec<&String> = opts.skipped.iter().filter(|s| s.contains("mesh")).collect();
    assert_eq!(mesh_rows.len(), spec.links.len(), "mesh 降级计数 = 链节数：{:?}", opts.skipped);
    assert!(mesh_rows.iter().any(|s| s.contains("link5_collision_0.obj")));
    assert!(spec.links.iter().all(|l| l.mesh_uri.is_none()));

    // <inertial> → 密度反推质量等价（WP-B/M2 前的诚实近似）
    for (l, m) in spec.links.iter().zip(PANDA_MASS) {
        let mass = l.density * spec_volume(l);
        assert!(
            (mass - m).abs() / m < 1e-3,
            "link {} 质量等价：{} vs URDF {m}（density {}）",
            l.name,
            mass,
            l.density
        );
        assert_eq!(l.added_mass, 0.0, "有 <inertial> 不再加质量");
        assert!(l.density.is_finite() && l.density > 0.0);
    }
    // FK：link1 = mount + (0,0.333,0)（Z-up 旋成 OriEra +Y）
    assert!((spec.links[0].origin[1] - 0.0).abs() < 1e-6);
    assert!((spec.links[1].origin[1] - 0.333).abs() < 1e-5, "{:?}", spec.links[1].origin);
    assert!(spec.links[1].origin[0].abs() < 1e-6 && spec.links[1].origin[2].abs() < 1e-6);
    // 链一致性（robot.rs 契约）：origin[child] == origin[parent] + anchor
    for j in &spec.joints {
        let want = [
            spec.links[j.parent_link].origin[0] + j.anchor[0],
            spec.links[j.parent_link].origin[1] + j.anchor[1],
            spec.links[j.parent_link].origin[2] + j.anchor[2],
        ];
        assert_eq!(spec.links[j.child_link].origin, want, "joint {}", j.name);
    }
}

#[test]
fn panda7_enters_the_multibody_and_tracks_in_limit_targets() {
    let rep = fixture(PANDA);
    let mut opts = panda_opts();
    let spec = rep.to_robot_spec(&mut opts).expect("to_robot_spec");
    // 夹爪外挂不挡构建：panda7 无夹爪
    assert!(spec.gripper.is_none());

    // 空桌远地：7 轴臂悬空跟踪，不与家具打架
    let scene = Scene3D {
        ground: ([0.0, -2.0, 0.0], [4.0, 0.05, 4.0]),
        table: None,
    };
    let mut sys = MultibodySystem::from_robot(spec.clone(), scene).expect("from_robot 构建");

    let mut world = World::new();
    world.insert_resource(oriera_subsys::Gravity(9.8));
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

    // settle：home 位姿稳定下来（joint4 从 0 钳到 −0.0698 的微动也完成）
    for i in 0..300u64 {
        sys.run(&ctx(i), &mut world);
    }
    let tel = world.get::<JointTelemetry>(arm).unwrap().clone();
    assert_eq!(tel.angles.len(), 7);
    for (i, a) in tel.angles.iter().enumerate() {
        assert!(
            (a - spec.home[i]).abs() < 0.05,
            "settle 后 joint{} = {a}，home {}",
            i + 1,
            spec.home[i]
        );
    }
    assert_eq!(sys.chain_points().len(), 7, "骨架随 N 缩放");

    // 限位内目标跟踪：<0.15 rad 各关节
    for i in 300..800u64 {
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.angles = PANDA_TARGETS.to_vec();
        }
        sys.run(&ctx(i), &mut world);
    }
    let tel = world.get::<JointTelemetry>(arm).unwrap().clone();
    let mut worst = (0usize, 0.0f32);
    for (i, (got, want)) in tel.angles.iter().zip(PANDA_TARGETS).enumerate() {
        let err = (got - want).abs();
        if err > worst.1 {
            worst = (i, err);
        }
        assert!(err < 0.15, "joint{} 跟踪误差 {err} rad ≥ 0.15（got {got}, want {want}）", i + 1);
    }
    assert!(
        tel.velocities.iter().all(|v| v.is_finite()) && tel.ee_pos.iter().all(|v| v.is_finite()),
        "遥测有限"
    );
    eprintln!("panda7 跟踪最差关节：{}（误差 {:.4} rad）", worst.0 + 1, worst.1);
}

#[test]
fn gripper2f_is_detected_and_mounts_on_the_panda() {
    let grip_rep = fixture(GRIPPER);
    assert_eq!(
        grip_rep.gripper_finger_joints(),
        Some(("finger_1".into(), "finger_2".into()))
    );
    // 单独导入：夹爪本体 descriptor（0 臂关节 + GripperSpec，validate 会拒绝
    // 构建——这正是「移植前」形态）
    let mut gopts = RobotSpecOpts::default();
    let grip_only = grip_rep.to_robot_spec(&mut gopts).expect("gripper-only descriptor");
    assert!(grip_only.joints.is_empty());
    let g0 = grip_only.gripper.as_ref().expect("GripperSpec 检出");
    assert_eq!(g0.attach_link, 0);
    assert!((g0.gap_closed - 0.008).abs() < 1e-4, "闭位 = 指板半厚 ×2：{}", g0.gap_closed);
    assert!(
        (g0.gap_open - g0.gap_closed - 0.08).abs() < 1e-4,
        "行程 = 限位 0.04 × (1+multiplier 1)：{} − {}",
        g0.gap_open,
        g0.gap_closed
    );
    assert_eq!(g0.finger_half, [0.0085, 0.0085, 0.004], "接近轴 Z→板 X、开合轴 Y→板 Z");
    assert_eq!(g0.effort, 20.0, "手指 <limit effort>");
    assert!((g0.gap_rate - 0.2 * 2.0 / 60.0).abs() < 1e-6, "<limit velocity>→gap_rate");
    assert_eq!(g0.kind, oriera_subsys::GripperKind::ParallelPlates);

    // 挂到 panda7：arm + 外部夹爪 → 一份 spec
    let rep = fixture(PANDA);
    let mut opts = panda_opts();
    let spec = rep
        .to_robot_spec_with_gripper(&grip_rep, &mut opts)
        .expect("panda7 + gripper2f");
    spec.validate().expect("挂载后仍合法");
    assert_eq!(spec.joints.len(), 7, "夹爪不进关节链");
    let g = spec.gripper.as_ref().expect("夹爪挂上");
    assert_eq!(g.attach_link, 7, "末体（attachment 吸收进 link7）");
    assert!((g.finger_x - 0.1029).abs() < 1e-3, "接近轴距离：{}", g.finger_x);
    // TCP = link7 法兰下 0.107 + 手指 0.1029 → (0, −0.2099, 0)（OriEra −Y 即下）
    assert!(
        (g.tcp_offset[1] - (-0.2099)).abs() < 2e-3
            && g.tcp_offset[0].abs() < 2e-3
            && g.tcp_offset[2].abs() < 2e-3,
        "tcp_offset {:?}",
        g.tcp_offset
    );
    assert!((g.gap_open - 0.088).abs() < 1e-4 && (g.gap_closed - 0.008).abs() < 1e-4);
    assert!(!opts.skipped.iter().any(|s| s.contains("覆盖")), "panda7 无自带夹爪");

    // from_robot 构建 + 指板按 spec 落位
    let scene = Scene3D {
        ground: ([0.0, -2.0, 0.0], [4.0, 0.05, 4.0]),
        table: None,
    };
    let mut sys = MultibodySystem::from_robot(spec, scene).expect("带夹爪构建");
    let mut world = World::new();
    world.insert_resource(oriera_subsys::Gravity(9.8));
    world.insert_resource(TimeScale(1.0));
    let arm = world.spawn();
    world.insert(
        arm,
        JointTargets {
            angles: vec![0.0; 7],
            gripper: 0.0,
        },
    );
    world.insert(arm, JointTelemetry::default());
    for i in 0..300u64 {
        sys.run(&ctx(i), &mut world);
    }
    let (plates, _counts) = sys.debug_contact_state();
    let gap = (plates[0][2] - plates[1][2]).abs();
    let g = sys.robot.gripper.as_ref().unwrap();
    assert!(
        (gap - g.gap_open).abs() < 1e-3,
        "开位指板间距 = spec.gap_open：{gap} vs {}",
        g.gap_open
    );
    // 板中心 = 末体原点 + finger_x·X̂（builder 板系约定；M0b 增板系朝向字段）
    let origin7 = sys.chain_points()[5];
    let mid = [
        (plates[0][0] + plates[1][0]) * 0.5,
        (plates[0][1] + plates[1][1]) * 0.5,
        (plates[0][2] + plates[1][2]) * 0.5,
    ];
    let want_mid = [origin7[0] + g.finger_x, origin7[1], origin7[2]];
    let d = ((mid[0] - want_mid[0]).powi(2) + (mid[1] - want_mid[1]).powi(2) + (mid[2] - want_mid[2]).powi(2)).sqrt();
    // 容差 = 板跟随后挂体的 kinematic 位姿（settle 残余 sag 的小角度摆动）
    assert!(d < 0.02, "板中心 {mid:?} vs 末体+finger_x·X̂ {want_mid:?}（差 {d}）");
    // TCP 遥测 = 末体 + tcp_offset（sag 后小角度容差）
    let tip = sys.ee_pose().0;
    let tcp_want = [origin7[0] + g.tcp_offset[0], origin7[1] + g.tcp_offset[1], origin7[2] + g.tcp_offset[2]];
    let dtip = ((tip[0] - tcp_want[0]).powi(2) + (tip[1] - tcp_want[1]).powi(2) + (tip[2] - tcp_want[2]).powi(2)).sqrt();
    assert!(dtip < 0.05, "TCP {tip:?} vs {tcp_want:?}（差 {dtip}，sag 容差内）");
}

#[test]
fn four_axis_fixture_joint_count_matches_the_legacy_projection() {
    // 与 urdf.rs 内嵌 FIXTURE 同源，但 j4 提升为 revolute（真 4 轴）——
    // 两条路都必须给出 4 关节。
    let xml = r#"<?xml version="1.0"?>
    <robot name="oriera_test_arm4">
      <link name="base"><collision><geometry><box size="0.08 0.12 0.12"/></geometry></collision></link>
      <link name="l1"><collision><geometry><cylinder radius="0.022" length="0.20"/></geometry></collision></link>
      <link name="l2"><collision><geometry><cylinder radius="0.022" length="0.18"/></geometry></collision></link>
      <link name="l3"><collision><geometry><cylinder radius="0.022" length="0.09"/></geometry></collision></link>
      <link name="tool"><collision><geometry><sphere radius="0.01"/></geometry></collision></link>
      <joint name="j1" type="revolute">
        <parent link="base"/><child link="l1"/>
        <origin xyz="0 0 0.12" rpy="0 0 0"/><axis xyz="0 1 0"/>
        <limit lower="-3.14" upper="3.14" effort="80" velocity="2"/>
        <dynamics damping="0.05"/>
      </joint>
      <joint name="j2" type="revolute">
        <parent link="l1"/><child link="l2"/>
        <origin xyz="0.20 0 0" rpy="0 0 0"/><axis xyz="0 0 1"/>
        <limit lower="-0.35" upper="1.45" effort="80" velocity="2"/>
      </joint>
      <joint name="j3" type="revolute">
        <parent link="l2"/><child link="l3"/>
        <origin xyz="0.18 0 0" rpy="0 0 0"/><axis xyz="0 0 1"/>
        <limit lower="-2.5" upper="0.35" effort="80" velocity="2"/>
      </joint>
      <joint name="j4" type="revolute">
        <parent link="l3"/><child link="tool"/>
        <origin xyz="0.09 0 0" rpy="0 0 0"/><axis xyz="0 0 1"/>
        <limit lower="-2.0" upper="2.0" effort="80" velocity="2"/>
      </joint>
    </robot>"#;
    let rep = UrdfReport::parse(xml).expect("fixture parses");
    let n_spec = rep
        .to_robot_spec(&mut RobotSpecOpts::default())
        .expect("4 轴全链导入")
        .joints
        .len();
    let (desc, _skipped) = rep.first_revolute_chain(4).expect("4 轴链提取");
    let n_projection = RobotSpec::from(&desc).joints.len();
    assert_eq!(n_spec, 4);
    assert_eq!(n_projection, 4);
    assert_eq!(n_spec, n_projection, "to_robot_spec 与旧投影关节数一致");

    // 3 轴版（j4 回到 fixed）：全链按实得 3 关节，不再投影补齐到 4
    let xml3 = xml.replace(
        r#"<joint name="j4" type="revolute">"#,
        r#"<joint name="j4" type="fixed">"#,
    );
    let rep3 = UrdfReport::parse(&xml3).unwrap();
    let n3 = rep3.to_robot_spec(&mut RobotSpecOpts::default()).unwrap().joints.len();
    assert_eq!(n3, 3, "全链导入按实得轴数，不再 4 轴投影");
}
