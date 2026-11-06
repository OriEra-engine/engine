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
//!
//! ── 慢测试分层约定（PLAN-embodied-v3 P1 #20）────────────────────────
//! 本文件分两层，CI 只跑快层：
//! - 快层（默认 `cargo test` 可见）：`panda7_maps_to_a_full_robot_spec`
//!   （全表导入契约）、`four_axis_fixture_joint_count_matches_the_legacy_
//!   projection`、`panda7_inertials_reach_the_rigid_bodies`（真惯量进
//!   multibody，10 tick settle）、`gripper2f_is_detected_and_mounts_on_
//!   the_panda`（夹爪挂载几何契约）、`prismatic_rail_urdf_imports_into_
//!   the_multibody_and_tracks`（M3 滑轨导入）、`mobile_arm_preset_json_
//!   loads_validates_and_drives`（移动底盘预设端到端）、
//!   `six_axis_gripper_imports_full_chain_and_hull_degrades_honestly`（
//!   P1-B1：6 轴+mimic+mesh 全链 + 凸包/包络双路 + 坑位警告）、
//!   `gap_touch_twin_trips_the_gap_warning`（PUGC-2 坑位警告正反例）。
//! - 慢层（`#[ignore = "slow: …"]`）：`panda7_enters_the_multibody_and_
//!   tracks_in_limit_targets`——800 tick 7 轴跟踪长跑（本文件 ~87% 时长
//!   的主体，实测 27s/31s）。本地/夜间跑：`cargo test -p oriera-agent
//!   --features rigid3d,urdf --test urdf_robot_spec -- --ignored`（CI 的
//!   `--ignored` 计数会因此变化，属于分层设计而非回归）。

#![cfg(all(feature = "rigid3d", feature = "urdf"))]

use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use oriera_embodied::urdf::{RobotSpecOpts, UrdfReport};
use oriera_subsys::{JointTargets, JointTelemetry, MultibodySystem, RobotSpec, Scene3D, TimeScale};

const DT: f32 = 1.0 / 60.0;
const PANDA: &str = "tests/fixtures/panda7.urdf";
const GRIPPER: &str = "tests/fixtures/gripper2f.urdf";
const SIX_AXIS: &str = "tests/fixtures/six_axis_gripper.urdf";
const SIX_AXIS_GAP_TOUCH: &str = "tests/fixtures/six_axis_gap_touch.urdf";

fn fixture(name: &str) -> UrdfReport {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    let xml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读 {} 失败：{e}", path.display()));
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
    spec.validate()
        .expect("spec 结构合法（mesh_uri 保持 None）");

    // 7 关节 + 8 链体（attachment 被 fixed 吸收进 link7）
    assert_eq!(spec.joints.len(), 7, "{:?}", spec.joint_names());
    assert_eq!(spec.links.len(), 8);
    assert_eq!(spec.root_link(), Some(0));
    assert_eq!(spec.dof(), 7);

    // 逐关节对照表：limits / effort / velocity / damping 全接
    for (row, j) in PANDA_JOINT_TABLE.iter().zip(&spec.joints) {
        assert_eq!(j.name, row.0, "关节顺序 = URDF 链序");
        let (lo, hi) = j.limits;
        assert!(
            (lo - row.1).abs() < 1e-5 && (hi - row.2).abs() < 1e-5,
            "{}: {:?}",
            j.name,
            j.limits
        );
        assert_eq!(j.effort, Some(row.3), "{} effort", j.name);
        assert_eq!(
            j.velocity_limit,
            Some(row.4),
            "{} velocity（URDF 此前被丢弃）",
            j.name
        );
        assert_eq!(j.damping, row.5, "{} damping", j.name);
        assert_eq!(
            (j.kp, j.kd, j.armature),
            (300.0, 30.0, 0.05),
            "{} 用 opts 增益",
            j.name
        );
    }
    // Panda 特色：joint4 限位不含零 → home 钳到 −0.0698，其余零位
    assert!((spec.home[3] - (-0.0698)).abs() < 1e-5, "{:?}", spec.home);
    assert!(spec
        .home
        .iter()
        .enumerate()
        .all(|(i, h)| i == 3 || h.abs() < 1e-6));

    // 轴：Z-up→Y-up 后 joint1/3/5 竖直（OriEra +Y，yaw 式），joint2/4/6/7 横向
    // （f32 三角函数有 ~1e-8 级噪声，全部按容差断言）
    let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
    let ax = |i: usize| spec.joints[i].axis;
    assert!(
        near(ax(1), [0.0, 0.0, -1.0]),
        "joint2 轴（URDF +Y → OriEra −Z）：{:?}",
        ax(1)
    );
    assert!(near(ax(3), [0.0, 0.0, 1.0]), "joint4 轴：{:?}", ax(3));
    assert!(near(ax(5), [0.0, 0.0, 1.0]), "joint6 轴：{:?}", ax(5));
    assert!(
        near(ax(6), [0.0, -1.0, 0.0]),
        "joint7 轴（法兰零位朝下，URDF −Z → OriEra −Y）：{:?}",
        ax(6)
    );
    for i in [0, 2, 4, 6] {
        assert!(
            (ax(i)[1].abs() - 1.0).abs() < 1e-5,
            "joint{} 轴 {:?} 应竖直",
            i + 1,
            ax(i)
        );
    }
    for i in [1, 3, 5] {
        assert!(
            ax(i)[1].abs() < 1e-5,
            "joint{} 轴 {:?} 应横向",
            i + 1,
            ax(i)
        );
    }

    // mesh 诚实降级：8/8 链体降级（.obj/.stl 无 mesh_base_dir 可解析）、
    // mesh_uri 全 None、URI 进 skipped（M2 起给 mesh_base_dir 的 STL 走凸包）
    let mesh_rows: Vec<&String> = opts.skipped.iter().filter(|s| s.contains("mesh")).collect();
    assert_eq!(
        mesh_rows.len(),
        spec.links.len(),
        "mesh 降级计数 = 链节数：{:?}",
        opts.skipped
    );
    assert!(mesh_rows
        .iter()
        .any(|s| s.contains("link5_collision_0.obj")));
    assert!(spec.links.iter().all(|l| l.mesh_uri.is_none()));

    // <inertial> 全张量直进 LinkSpec.inertial（M2 U-1）：质量逐项 == URDF，
    // 张量迹（旋转不变量）逐项 == URDF 对角和；密度退为簿记缺省
    let preset = RobotSpec::from(&oriera_subsys::ArmDescription::default());
    const PANDA_TRACE: [f32; 8] = [
        0.00315 + 0.00388 + 0.004285,
        0.70337 + 0.70661 + 0.0091170,
        0.0079620 + 2.8110e-2 + 2.5995e-2,
        3.7242e-2 + 3.6155e-2 + 1.083e-2,
        2.5853e-2 + 1.9552e-2 + 2.8323e-2,
        3.5549e-2 + 2.9474e-2 + 8.627e-3,
        1.964e-3 + 4.354e-3 + 5.433e-3,
        1.2516e-2 + 1.0027e-2 + 4.815e-3,
    ];
    for (k, (l, m)) in spec.links.iter().zip(PANDA_MASS).enumerate() {
        let i = l
            .inertial
            .as_ref()
            .unwrap_or_else(|| panic!("link {} 缺 inertial", l.name));
        assert_eq!(i.mass, m, "link {} 质量 == URDF 逐项", l.name);
        let tr = i.ixx + i.iyy + i.izz;
        assert!(
            (tr - PANDA_TRACE[k]).abs() < 1e-5,
            "link {} 张量迹不变量：{tr} vs URDF {}",
            l.name,
            PANDA_TRACE[k]
        );
        assert_eq!(l.added_mass, 0.0, "有 <inertial> 不再加质量");
        assert_eq!(
            l.density, preset.links[1].density,
            "inertial 直给 → 密度簿记缺省"
        );
    }
    // FK：link1 = mount + (0,0.333,0)（Z-up 旋成 OriEra +Y）
    assert!((spec.links[0].origin[1] - 0.0).abs() < 1e-6);
    assert!(
        (spec.links[1].origin[1] - 0.333).abs() < 1e-5,
        "{:?}",
        spec.links[1].origin
    );
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
#[ignore = "slow: 800 tick 7 轴跟踪物理长跑——本地/夜间跑 cargo test -- --ignored"]
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
        assert!(
            err < 0.15,
            "joint{} 跟踪误差 {err} rad ≥ 0.15（got {got}, want {want}）",
            i + 1
        );
    }
    assert!(
        tel.velocities.iter().all(|v| v.is_finite()) && tel.ee_pos.iter().all(|v| v.is_finite()),
        "遥测有限"
    );
    eprintln!(
        "panda7 跟踪最差关节：{}（误差 {:.4} rad）",
        worst.0 + 1,
        worst.1
    );
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
    let grip_only = grip_rep
        .to_robot_spec(&mut gopts)
        .expect("gripper-only descriptor");
    assert!(grip_only.joints.is_empty());
    let g0 = grip_only.gripper.as_ref().expect("GripperSpec 检出");
    assert_eq!(g0.attach_link, 0);
    assert!(
        (g0.gap_closed - 0.008).abs() < 1e-4,
        "闭位 = 指板半厚 ×2：{}",
        g0.gap_closed
    );
    assert!(
        (g0.gap_open - g0.gap_closed - 0.08).abs() < 1e-4,
        "行程 = 限位 0.04 × (1+multiplier 1)：{} − {}",
        g0.gap_open,
        g0.gap_closed
    );
    assert_eq!(
        g0.finger_half,
        [0.0085, 0.0085, 0.004],
        "板半尺寸 = 挂载系 AABB（r_up 置换 y↔z，指垫对称 → 数值同旧重排）"
    );
    // M2 朝向接线：接近/开合方向经 r_up·mount.r 旋到挂载体系——
    // 手系接近 +Z → OriEra +Y（上轴换算）、开合 +Y → OriEra −Z。
    let near =
        |a: [f32; 3], b: [f32; 3], tol: f32| a.iter().zip(b).all(|(x, y)| (x - y).abs() < tol);
    assert!(
        near(g0.approach_axis.unwrap(), [0.0, 1.0, 0.0], 1e-6),
        "approach_axis {:?}",
        g0.approach_axis
    );
    assert!(
        near(g0.gap_axis.unwrap(), [0.0, 0.0, -1.0], 1e-6),
        "gap_axis {:?}",
        g0.gap_axis
    );
    assert_eq!(g0.effort, 20.0, "手指 <limit effort>");
    assert!(
        (g0.gap_rate - 0.2 * 2.0 / 60.0).abs() < 1e-6,
        "<limit velocity>→gap_rate"
    );
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
    assert!(
        (g.finger_x - 0.1029).abs() < 1e-3,
        "接近轴距离：{}",
        g.finger_x
    );
    // TCP = link7 法兰下 0.107 + 手指 0.1029 → (0, −0.2099, 0)（OriEra −Y 即下）
    assert!(
        (g.tcp_offset[1] - (-0.2099)).abs() < 2e-3
            && g.tcp_offset[0].abs() < 2e-3
            && g.tcp_offset[2].abs() < 2e-3,
        "tcp_offset {:?}",
        g.tcp_offset
    );
    assert!((g.gap_open - 0.088).abs() < 1e-4 && (g.gap_closed - 0.008).abs() < 1e-4);
    // M2 朝向接线（挂载）：法兰 attachment = Rx(180°)·Rz(135°)——手系
    // 接近 +Z → OriEra −Y（垂直向下 ✓），开合 +Y → 水平 (−√2/2, 0, −√2/2)；
    // finger_half = 手系 AABB 经同一旋转 → 135° 非整倍角 → 保守包络
    // （0.0085/0.004 → 0.70711·(0.0085+0.004) = 0.0088388 行）。
    let near =
        |a: [f32; 3], b: [f32; 3], tol: f32| a.iter().zip(b).all(|(x, y)| (x - y).abs() < tol);
    assert!(
        near(g.approach_axis.unwrap(), [0.0, -1.0, 0.0], 1e-5),
        "approach_axis {:?}",
        g.approach_axis
    );
    let s = std::f32::consts::FRAC_1_SQRT_2;
    assert!(
        near(g.gap_axis.unwrap(), [-s, 0.0, -s], 1e-5),
        "gap_axis {:?}",
        g.gap_axis
    );
    let half_want = [s * 0.0125, 0.0085, s * 0.0125];
    assert!(
        g.finger_half
            .iter()
            .zip(half_want)
            .all(|(a, b)| (a - b).abs() < 1e-5),
        "finger_half {:?} vs 保守 AABB {half_want:?}",
        g.finger_half
    );
    assert!(
        !opts.skipped.iter().any(|s| s.contains("覆盖")),
        "panda7 无自带夹爪"
    );

    // from_robot 构建 + 指板按 spec（板系朝向数据）落位
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
    let gap = ((plates[0][0] - plates[1][0]).powi(2)
        + (plates[0][1] - plates[1][1]).powi(2)
        + (plates[0][2] - plates[1][2]).powi(2))
    .sqrt();
    let g = sys.robot.gripper.as_ref().unwrap();
    assert!(
        (gap - g.gap_open).abs() < 2e-3,
        "开位指板间距（沿 gap 轴的欧氏距离）= spec.gap_open：{gap} vs {}",
        g.gap_open
    );
    // 板中心 = 末体原点 + finger_x·approach_axis（M0b 板系朝向字段——
    // Panda 法兰朝下构型，接近轴 = 世界 −Y）
    let origin7 = sys.chain_points()[5];
    let mid = [
        (plates[0][0] + plates[1][0]) * 0.5,
        (plates[0][1] + plates[1][1]) * 0.5,
        (plates[0][2] + plates[1][2]) * 0.5,
    ];
    let ap = g.approach_axis_or_default();
    let want_mid = [
        origin7[0] + g.finger_x * ap[0],
        origin7[1] + g.finger_x * ap[1],
        origin7[2] + g.finger_x * ap[2],
    ];
    let d = ((mid[0] - want_mid[0]).powi(2)
        + (mid[1] - want_mid[1]).powi(2)
        + (mid[2] - want_mid[2]).powi(2))
    .sqrt();
    // 容差 = 板跟随后挂体的 kinematic 位姿（settle 残余 sag 的小角度摆动）
    assert!(
        d < 0.02,
        "板中心 {mid:?} vs 末体+finger_x·approach {want_mid:?}（差 {d}）"
    );
    // 板间距方向 ≈ gap_axis（sag 后小角度容差 → 点积 > 0.99）
    let sep = [
        plates[0][0] - plates[1][0],
        plates[0][1] - plates[1][1],
        plates[0][2] - plates[1][2],
    ];
    let gp = g.gap_axis_or_default();
    let dot = (sep[0] * gp[0] + sep[1] * gp[1] + sep[2] * gp[2]) / gap;
    assert!(dot > 0.99, "板间距方向应沿 gap_axis：dot={dot}");
    // TCP 遥测 = 末体 + tcp_offset（sag 后小角度容差）
    let tip = sys.ee_pose().0;
    let tcp_want = [
        origin7[0] + g.tcp_offset[0],
        origin7[1] + g.tcp_offset[1],
        origin7[2] + g.tcp_offset[2],
    ];
    let dtip = ((tip[0] - tcp_want[0]).powi(2)
        + (tip[1] - tcp_want[1]).powi(2)
        + (tip[2] - tcp_want[2]).powi(2))
    .sqrt();
    assert!(
        dtip < 0.05,
        "TCP {tip:?} vs {tcp_want:?}（差 {dtip}，sag 容差内）"
    );
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
    let n3 = rep3
        .to_robot_spec(&mut RobotSpecOpts::default())
        .unwrap()
        .joints
        .len();
    assert_eq!(n3, 3, "全链导入按实得轴数，不再 4 轴投影");
}

/// M2 U-1 验收：Panda 夹具链体的**真惯量**进刚体构建——`body
/// mass_properties()` 读回质量逐项 == URDF，张量旋转不变量（迹 /
/// 行列式 / Frobenius）逐项 == URDF 全张量，质心模长 == URDF inertial
/// origin 模长（同旋系不变量）；per-joint armature 真进 multibody。
#[test]
fn panda7_inertials_reach_the_rigid_bodies() {
    let rep = fixture(PANDA);
    let mut opts = panda_opts();
    let spec = rep.to_robot_spec(&mut opts).expect("to_robot_spec");
    let scene = Scene3D {
        ground: ([0.0, -2.0, 0.0], [4.0, 0.05, 4.0]),
        table: None,
    };
    let mut sys = MultibodySystem::from_robot(spec.clone(), scene).expect("构建");
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
    // rapier 的质量属性重算随首个 pipeline step 落地——先 settle。
    for i in 0..10u64 {
        sys.run(&ctx(i), &mut world);
    }

    // URDF 原始张量（fixture 逐字面值）：(ixx, iyy, izz, ixy, ixz, iyz)
    const PANDA_TENSOR: [[f32; 6]; 8] = [
        [0.00315, 0.00388, 0.004285, 8.2904e-7, 0.00015, 8.2299e-6],
        [
            0.70337,
            0.70661,
            0.0091170,
            -0.00013900,
            0.0067720,
            0.019169,
        ],
        [
            0.0079620, 2.8110e-2, 2.5995e-2, -3.925e-3, 1.0254e-2, 7.04e-4,
        ],
        [
            3.7242e-2, 3.6155e-2, 1.083e-2, -4.761e-3, -1.1396e-2, -1.2805e-2,
        ],
        [
            2.5853e-2, 1.9552e-2, 2.8323e-2, 7.796e-3, -1.332e-3, 8.641e-3,
        ],
        [
            3.5549e-2, 2.9474e-2, 8.627e-3, -2.117e-3, -4.037e-3, 2.29e-4,
        ],
        [1.964e-3, 4.354e-3, 5.433e-3, 1.09e-4, -1.158e-3, 3.41e-4],
        [
            1.2516e-2, 1.0027e-2, 4.815e-3, -4.28e-4, -1.196e-3, -7.41e-4,
        ],
    ];
    // URDF inertial origin（link 系）模长——旋系不变量（fixture 逐项实算，
    // f32 三角噪声容差 2e-3）
    const PANDA_COM_NORM: [f32; 8] = [
        0.064652, 0.047823, 0.029101, 0.081978, 0.120343, 0.057503, 0.062672, 0.062633,
    ];
    // 惯性张量 (ixx,iyy,izz,ixy,ixz,iyz) → 对称矩阵
    let sym = |t: [f32; 6]| [[t[0], t[3], t[4]], [t[3], t[1], t[5]], [t[4], t[5], t[2]]];
    let det3 = |m: [[f32; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let frob2 = |m: [[f32; 3]; 3]| {
        (0..3)
            .map(|a| (0..3).map(|b| m[a][b] * m[a][b]).sum::<f32>())
            .sum::<f32>()
    };
    for (k, l) in spec.links.iter().enumerate().skip(1) {
        // link0 = 固定根：rapier 把 fixed 体质量归零（求解器不消费），
        // 其 URDF 张量仍逐项在 spec.links[0].inertial（上一测试已钉）。
        let mp = sys
            .link_mass_properties(k)
            .unwrap_or_else(|| panic!("link {k} 读回"));
        // 质量：读回 == URDF 逐项（f32 直通，1e-6 相对）
        assert!(
            (mp.mass - PANDA_MASS[k]).abs() / PANDA_MASS[k] < 1e-5,
            "link {} 质量 {} vs URDF {}",
            l.name,
            mp.mass,
            PANDA_MASS[k]
        );
        // 张量：主轴读回值 = URDF 全张量的特征值 → 比三个旋转不变量
        // （迹 / det / Frobenius²——正交共轭守恒，绕开测试侧重做特征分解）
        let lam = mp.principal_inertia;
        let tr = lam[0] + lam[1] + lam[2];
        let det = lam[0] * lam[1] * lam[2];
        let f2 = lam[0] * lam[0] + lam[1] * lam[1] + lam[2] * lam[2];
        let u = sym(PANDA_TENSOR[k]);
        let tr_u = u[0][0] + u[1][1] + u[2][2];
        let det_u = det3(u);
        let f2_u = frob2(u);
        let rel = |a: f32, b: f32| (a - b).abs() / b.abs().max(1e-9);
        assert!(rel(tr, tr_u) < 1e-4, "link {} 迹 {tr} vs {tr_u}", l.name);
        assert!(
            rel(det, det_u) < 2e-3,
            "link {} 行列式 {det} vs {det_u}",
            l.name
        );
        assert!(
            rel(f2, f2_u) < 1e-4,
            "link {} Frobenius² {f2} vs {f2_u}",
            l.name
        );
        // 质心：模长旋系不变（方向由 r_up∘R_link 决定，urdf.rs 的
        // rotate_inertia 单测钉死；这里钉模长与有限性）
        let com_norm =
            (mp.com[0] * mp.com[0] + mp.com[1] * mp.com[1] + mp.com[2] * mp.com[2]).sqrt();
        assert!(
            (com_norm - PANDA_COM_NORM[k]).abs() < 2e-3,
            "link {} |com| {com_norm} vs URDF ≈{}",
            l.name,
            PANDA_COM_NORM[k]
        );
        assert!(
            mp.com.iter().all(|v| v.is_finite()) && mp.principal_inertia.iter().all(|v| *v >= 0.0)
        );
    }
    // armature：opts.default_armature 真进 multibody 的 per-DoF 槽位
    for (k, j) in spec.joints.iter().enumerate() {
        let a = sys.joint_armature(k);
        assert!(
            (a - j.armature).abs() < 1e-6,
            "joint {} armature 读回 {a} vs spec {}",
            j.name,
            j.armature
        );
    }
}

// ── M3（PLAN-embodied-general §四 M3 移动底盘）：prismatic 链关节真导入 ──

/// 手写滑轨臂 URDF（REP-103 Z-up）：固定 base → `rail_x`（prismatic，沿
/// URDF +X，限位米）→ 滑台 → `yaw`（revolute 绕 Z）→ `pitch`（revolute 绕
/// Y）→ fixed 工具。公开 Panda 没有 prismatic 链关节——这就是任务书说的
/// 「合成滑轨臂 fixture」的 URDF 版。
const SLIDE_ARM_URDF: &str = r#"<?xml version="1.0"?>
<robot name="slide_arm">
  <link name="base"><collision><geometry><box size="0.6 0.1 0.04"/></geometry></collision></link>
  <link name="carriage">
    <inertial><mass value="2.0"/><inertia ixx="0.004" iyy="0.004" izz="0.006"/></inertial>
    <collision><geometry><box size="0.12 0.12 0.04"/></geometry></collision>
  </link>
  <link name="l1">
    <inertial><origin xyz="0 0 0.05"/><mass value="1.0"/><inertia ixx="0.002" iyy="0.002" izz="0.001"/></inertial>
    <collision><origin xyz="0 0 0.05"/><geometry><box size="0.05 0.05 0.1"/></geometry></collision>
  </link>
  <link name="l2">
    <inertial><origin xyz="0.1 0 0"/><mass value="0.5"/><inertia ixx="0.0005" iyy="0.002" izz="0.002"/></inertial>
    <collision><origin xyz="0.1 0 0"/><geometry><box size="0.2 0.04 0.04"/></geometry></collision>
  </link>
  <link name="tool"/>
  <joint name="rail_x" type="prismatic">
    <parent link="base"/><child link="carriage"/>
    <origin xyz="0 0 0.05"/><axis xyz="1 0 0"/>
    <limit lower="-0.1" upper="0.3" effort="200" velocity="0.5"/>
    <dynamics damping="2.0"/>
  </joint>
  <joint name="yaw" type="revolute">
    <parent link="carriage"/><child link="l1"/>
    <origin xyz="0 0 0.02"/><axis xyz="0 0 1"/>
    <limit lower="-3.14" upper="3.14" effort="40" velocity="2"/>
  </joint>
  <joint name="pitch" type="revolute">
    <parent link="l1"/><child link="l2"/>
    <origin xyz="0 0 0.1"/><axis xyz="0 1 0"/>
    <limit lower="-1.5" upper="1.5" effort="40" velocity="2"/>
  </joint>
  <joint name="tool_fix" type="fixed">
    <parent link="l2"/><child link="tool"/><origin xyz="0.2 0 0"/>
  </joint>
</robot>"#;

#[test]
fn prismatic_rail_urdf_imports_into_the_multibody_and_tracks() {
    let rep = UrdfReport::parse(SLIDE_ARM_URDF).expect("滑轨臂 URDF 解析");
    let mut opts = RobotSpecOpts::default();
    let spec = rep
        .to_robot_spec(&mut opts)
        .expect("prismatic 链真导入（不再断链报错）");
    assert_eq!(spec.joint_names(), ["rail_x", "yaw", "pitch"]);
    let rail = &spec.joints[0];
    assert_eq!(rail.kind, oriera_subsys::JointKind::Prismatic);
    assert_eq!(rail.limits, (-0.1, 0.3), "米制限位直接接");
    assert_eq!(
        (rail.effort, rail.velocity_limit, rail.damping),
        (Some(200.0), Some(0.5), 2.0)
    );
    assert_eq!(rail.axis, [1.0, 0.0, 0.0], "URDF +X 滑向 → OriEra +X");
    assert_eq!(
        (rail.kp, rail.kd),
        (opts.default_kp_prismatic, opts.default_kd_prismatic)
    );
    assert!(spec.joints[1..]
        .iter()
        .all(|j| j.kind == oriera_subsys::JointKind::Revolute));
    assert_eq!(
        spec.joints[1].axis,
        [0.0, 1.0, 0.0],
        "URDF +Z 转轴 → OriEra +Y"
    );
    spec.validate().expect("滑轨臂 RobotSpec 合法");

    // 无家具世界（M3 from_robot_fixtures 空列表 = 纯铰接动力学）
    let mut sys =
        MultibodySystem::from_robot_fixtures(spec.clone(), &[]).expect("prismatic 链进 multibody");
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
    let mut hold = |targets: [f32; 3], t0: u64, n: u64, world: &mut World| {
        for i in t0..t0 + n {
            if let Some(t) = world.get_mut::<JointTargets>(arm) {
                t.angles = targets.to_vec();
            }
            sys.run(&ctx(i), world);
        }
        world.get::<JointTelemetry>(arm).unwrap().clone()
    };
    let a = hold([0.0, 0.4, 0.3], 0, 300, &mut world);
    let b = hold([0.2, 0.4, 0.3], 300, 300, &mut world);
    for (k, (got, want)) in b.angles.iter().zip([0.2f32, 0.4, 0.3]).enumerate() {
        let tol = if k == 0 { 5e-3 } else { 0.06 };
        assert!(
            (got - want).abs() < tol,
            "{}: got {got}, want {want}",
            spec.joints[k].name
        );
    }
    let dx = b.ee_pos[0] - a.ee_pos[0];
    eprintln!(
        "slide-arm URDF: rail 0→0.2 m, TCP Δx {dx:.4} m, rail readout {:.4} m",
        b.angles[0]
    );
    assert!((dx - 0.2).abs() < 0.01, "TCP rides the rail: Δx {dx}");
}

/// 官方移动底盘预设 JSON 端到端：经 SceneSpec 真加载（extends/schema/未知键
/// 审计/validate 全链）→ prismatic 关节类型与米制字段 → `kind` 只在
/// prismatic 关节上序列化（revolute 省略 = 旧 JSON 语义）→ env reset 后
/// 方块落在 −Z 带的地面上 → 绝对目标动作把底盘开到位（观测里 slide
/// 关节是米）。
#[cfg(all(feature = "spec", feature = "json"))]
#[test]
fn mobile_arm_preset_json_loads_validates_and_drives() {
    use oriera_embodied::embodied_env::EmbodiedEnv;
    use oriera_embodied::spec::SceneSpec;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("specs/official/mobile-arm-4dof.json");
    let loaded = SceneSpec::load_file(&path).expect("移动底盘预设加载");
    assert!(!loaded.migrated, "v2 原生 spec");
    let spec = loaded.spec;
    spec.validate().expect("spec 合法");
    assert_eq!(spec.robot.dof(), 7);
    let kinds: Vec<_> = spec.robot.joints.iter().map(|j| j.kind).collect();
    assert_eq!(
        &kinds[..2],
        &[
            oriera_subsys::JointKind::Prismatic,
            oriera_subsys::JointKind::Prismatic
        ]
    );
    assert!(kinds[2..]
        .iter()
        .all(|k| *k == oriera_subsys::JointKind::Revolute));
    assert_eq!(spec.scene.fixtures.len(), 1, "只有地面");
    assert_eq!(spec.task.id, "pick_place@v1");
    let v = serde_json::to_value(&spec.robot).expect("robot 序列化");
    let joints = v["joints"].as_array().expect("joints");
    assert_eq!(joints[0]["kind"], "Prismatic");
    assert!(
        joints[2].get("kind").is_none(),
        "revolute 的 kind 省略（旧 JSON 语义）"
    );

    let mut env = spec.to_env();
    let obs = env.reset(1000);
    assert_eq!(obs.len(), env.spec().obs_dim);
    let names = env.obs_names();
    let at = |n: &str| {
        names
            .iter()
            .position(|s| s == n)
            .unwrap_or_else(|| panic!("缺 {n}"))
    };
    let (sx, sz, ee, rel) = (
        at("slide_x.pos"),
        at("slide_z.pos"),
        at("ee.x"),
        at("obj_rel.x"),
    );
    let cube = [
        obs[ee] + obs[rel],
        obs[ee + 1] + obs[rel + 1],
        obs[ee + 2] + obs[rel + 2],
    ];
    assert!(
        (0.14 - 0.03..=0.22 + 0.03).contains(&cube[0])
            && (-0.52 - 0.03..=-0.46 + 0.03).contains(&cube[2])
            && (cube[1] - 0.025).abs() < 0.01,
        "方块落在 −Z 带地面上：{cube:?}"
    );
    // 动作 = 绝对关节目标（slide 为米）+ 夹爪
    let mut action = vec![0.0f32; env.spec().action_dim];
    action[0] = 0.1;
    action[1] = -0.3;
    let mut last = obs;
    for _ in 0..240 {
        last = env.step(&action).obs;
    }
    assert!(
        (last[sx] - 0.1).abs() < 5e-3,
        "slide_x 到位（米）：{}",
        last[sx]
    );
    assert!(
        (last[sz] + 0.3).abs() < 5e-3,
        "slide_z 到位（米）：{}",
        last[sz]
    );
}

// ── P1-B1（PLAN-embodied-v3 §二 #12）：--urdf-robot 全链 CLI 验收 ──────
// 合成 6 轴 + mimic 夹爪 + mesh 链体（tests/fixtures/six_axis_gripper.urdf
// 及其 gap-touch 孪生）——CLI 侧跑 `--urdf-robot --dump-robot-spec`，这里
// 钉导入链本体：全链关节表、凸包/包络双路、坑位警告正反例。

/// six_axis fixture 的 opts：mesh 根 = fixtures/meshes，
/// `package://sixaxis_meshes/…` 经包根表解析到同一目录。
fn six_axis_opts_with_meshes() -> RobotSpecOpts {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/meshes");
    RobotSpecOpts {
        mesh_base_dir: Some(root.clone()),
        mesh_packages: std::collections::BTreeMap::from([("sixaxis_meshes".to_string(), root)]),
        ..RobotSpecOpts::default()
    }
}

#[test]
// fixture 的限位字面值是 URDF 原文 3.14159（REP 风格缩写 π）——回环断言
// 就是要逐字面值相等，epsilon 反而弱化契约。
#[allow(clippy::approx_constant)]
fn six_axis_gripper_imports_full_chain_and_hull_degrades_honestly() {
    let rep = fixture(SIX_AXIS);
    // mimic 两指对检出
    assert_eq!(
        rep.gripper_finger_joints(),
        Some(("finger_1".into(), "finger_2".into()))
    );

    // ① 给了 mesh 根：link5 的 STL → ConvexHull（12 三角立方去重后 8 顶点）
    let mut opts = six_axis_opts_with_meshes();
    let spec = rep.to_robot_spec(&mut opts).expect("6 轴全链导入");
    assert_eq!(spec.name, "sixaxis_lab_arm");
    assert_eq!(spec.dof(), 6, "全链按实得轴数：{:?}", spec.joint_names());
    assert_eq!(
        spec.joint_names(),
        ["joint1", "joint2", "joint3", "joint4", "joint5", "joint6"]
    );
    assert_eq!(
        spec.links.len(),
        7,
        "base_link + link1..6（tool0/hand 被 fixed 吸收，手指不入链）"
    );
    spec.validate().expect("导入 spec 结构合法");

    // 轴映射：URDF +Z（joint1 竖直 yaw）→ OriEra +Y；URDF +Y（pitch）→ −Z
    assert_eq!(spec.joints[0].axis, [0.0, 1.0, 0.0]);
    assert_eq!(spec.joints[1].axis, [0.0, 0.0, -1.0]);
    // 主轴朝向规则（文档化）：肩柱两关节锚竖直（OriEra +Y），
    // 水平伸展段（joint3..6）锚全沿 OriEra +X——URDF +X 主轴原样保向
    assert_eq!(spec.joints[0].anchor, [0.0, 0.10, 0.0]);
    assert_eq!(spec.joints[1].anchor, [0.0, 0.10, 0.0]);
    for j in &spec.joints[2..] {
        assert!(
            j.anchor[0] > 0.0 && j.anchor[1].abs() < 1e-6 && j.anchor[2].abs() < 1e-6,
            "{} 锚沿 OriEra +X：{:?}",
            j.name,
            j.anchor
        );
    }
    // 限位/effort/velocity/damping 逐字面值直进
    assert_eq!(spec.joints[0].limits, (-3.14159, 3.14159));
    assert_eq!(
        (
            spec.joints[2].effort,
            spec.joints[2].velocity_limit,
            spec.joints[2].damping
        ),
        (Some(120.0), Some(2.0), 0.5)
    );

    // mesh 凸包：AABB 0.06 m、mesh_uri 留 provenance、skipped 诚实登记
    let l5 = &spec.links[5];
    let oriera_subsys::LinkShape::ConvexHull { points } = &l5.shape else {
        panic!("给了 mesh 根：link5 应为 ConvexHull：{:?}", l5.shape)
    };
    assert_eq!(points.len(), 8, "立方 12 三角去重后 8 顶点：{points:?}");
    let mut mn = [f32::MAX; 3];
    let mut mx = [f32::MIN; 3];
    for p in points {
        for k in 0..3 {
            mn[k] = mn[k].min(p[k]);
            mx[k] = mx[k].max(p[k]);
        }
    }
    for k in 0..3 {
        assert!(
            ((mx[k] - mn[k]) - 0.06).abs() < 1e-4,
            "AABB[{k}] = {:?}（半 0.03 立方）",
            (mn[k], mx[k])
        );
    }
    assert_eq!(
        l5.mesh_uri.as_deref(),
        Some("package://sixaxis_meshes/link5.stl")
    );
    assert!(
        opts.skipped
            .iter()
            .any(|s| s.contains("link5") && s.contains("ConvexHull")),
        "{:?}",
        opts.skipped
    );

    // 集成夹爪：gap_closed = 0.046（桌面 preset 值——刻意不触警）、行程 0.126
    let g = spec.gripper.as_ref().expect("集成夹爪检出");
    assert!((g.gap_closed - 0.046).abs() < 1e-4, "{}", g.gap_closed);
    assert!((g.gap_open - 0.126).abs() < 1e-4, "{}", g.gap_open);
    assert!(
        (g.gap_rate - 0.15 * 2.0 / 60.0).abs() < 1e-6,
        "手指 <limit velocity> → gap_rate：{}",
        g.gap_rate
    );
    assert_eq!(g.attach_link, 6, "末体 link6（法兰+手被 fixed 吸收）");
    let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
    assert!(
        near(g.approach_axis.unwrap(), [1.0, 0.0, 0.0]),
        "接近轴 = 手系 +X → OriEra +X：{:?}",
        g.approach_axis
    );
    assert!(
        near(g.gap_axis.unwrap(), [0.0, 0.0, -1.0]),
        "开合轴 = 手系 +Y → OriEra −Z：{:?}",
        g.gap_axis
    );

    // 坑位治理：home 全零（URDF 零位）→ 警告；gap_closed = 0.046 → 无 gap 警告
    let warns = oriera_embodied::urdf::pitfall_warnings(&spec);
    assert!(warns.iter().any(|w| w.contains("URDF 零位")), "{warns:?}");
    assert!(
        !warns.iter().any(|w| w.contains("指板闭合位相触")),
        "{warns:?}"
    );

    // 构建过闸：6 轴 + 凸包链体 + 双板夹爪进 multibody
    assert!(MultibodySystem::from_robot(spec, Scene3D::default()).is_ok());

    // ② 不给 mesh 根：同一 link → 链段包络降级（段沿 +X → Capsule）+ 登记
    let mut opts2 = RobotSpecOpts::default();
    let spec2 = rep.to_robot_spec(&mut opts2).expect("无 mesh 根也导入");
    let l5b = &spec2.links[5];
    assert!(
        matches!(l5b.shape, oriera_subsys::LinkShape::Capsule { .. }),
        "段沿 +X → Capsule：{:?}",
        l5b.shape
    );
    assert!(l5b.mesh_uri.is_none());
    assert!(
        opts2
            .skipped
            .iter()
            .any(|s| s.contains("link5") && s.contains("包络降级") && s.contains("link5.stl")),
        "{:?}",
        opts2.skipped
    );
}

/// PUGC-2 正例：指板闭合位相触（gap_closed = 0）→ 人话警告点名 preset
/// 参考值（0.046）与 `--set` 出口；警告非致命——spec 仍合法可构建。
/// 反例：六轴主件（gap 0.046）与 panda7（无夹爪、home 非全零）都不触警。
#[test]
fn gap_touch_twin_trips_the_gap_warning() {
    let rep = fixture(SIX_AXIS_GAP_TOUCH);
    let mut opts = six_axis_opts_with_meshes();
    let spec = rep.to_robot_spec(&mut opts).expect("gap-touch 导入");
    let g = spec.gripper.as_ref().expect("夹爪");
    assert!(
        g.gap_closed.abs() < 1e-6,
        "指板闭合位相触：{}",
        g.gap_closed
    );
    let warns = oriera_embodied::urdf::pitfall_warnings(&spec);
    let gap = warns
        .iter()
        .find(|w| w.contains("指板闭合位相触"))
        .expect("gap_closed=0 必须触发人话警告");
    assert!(
        gap.contains("0.046") && gap.contains("robot.gripper.gap_closed"),
        "警告须带参考值与覆盖出口：{gap}"
    );
    // home 全零警告同发（URDF 零位）
    assert!(warns.iter().any(|w| w.contains("URDF 零位")), "{warns:?}");
    // 非致命：警告 ≠ 拒绝
    spec.validate().expect("gap-touch 仍合法（警告不拦）");
    assert!(MultibodySystem::from_robot(spec, Scene3D::default()).is_ok());

    // 反例一：六轴主件（gap 0.046）——无 gap 警告（home 警告仍在）
    let ok = fixture(SIX_AXIS);
    let mut o = six_axis_opts_with_meshes();
    let ok_spec = ok.to_robot_spec(&mut o).expect("主件导入");
    assert!(!oriera_embodied::urdf::pitfall_warnings(&ok_spec)
        .iter()
        .any(|w| w.contains("指板闭合位相触")));
    // 反例二：panda7——无夹爪、joint4 钳出非零 → 两类警告都不发
    let panda = fixture(PANDA);
    let mut po = panda_opts();
    let pspec = panda.to_robot_spec(&mut po).expect("panda 导入");
    assert!(
        oriera_embodied::urdf::pitfall_warnings(&pspec).is_empty(),
        "panda7 不应触警"
    );
}

// ── P2：base 安装朝向（RobotSpecOpts::mount_rpy）FK 一致性 ──────────────

/// URDF `rpy`（外旋 XYZ：Rz·Ry·Rx）与 Z-up→Y-up 的测试侧独立实现——不借
/// urdf.rs 的私有函数，避免「拿被测代码验被测代码」。
fn rpy_mat(rpy: [f32; 3]) -> [[f32; 3]; 3] {
    let (sr, cr) = rpy[0].sin_cos();
    let (sp, cp) = rpy[1].sin_cos();
    let (sy, cy) = rpy[2].sin_cos();
    [
        [cy * cp, cy * sp * sr - sy * cr, cy * sp * cr + sy * sr],
        [sy * cp, sy * sp * sr + cy * cr, sy * sp * cr - cy * sr],
        [-sp, cp * sr, cp * cr],
    ]
}

fn mul3(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let c = |i: usize, j: usize| (0..3).map(|k| a[i][k] * b[k][j]).sum::<f32>();
    [
        [c(0, 0), c(0, 1), c(0, 2)],
        [c(1, 0), c(1, 1), c(1, 2)],
        [c(2, 0), c(2, 1), c(2, 2)],
    ]
}

fn apply3(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// OriEra 世界里的 base 旋转：`r_up · R(rpy) · r_upᵀ`（r_up = Rx(−90°)）。
fn oriera_base_rot(rpy: [f32; 3]) -> [[f32; 3]; 3] {
    let up = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]];
    let up_t = [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]];
    mul3(&mul3(&up, &rpy_mat(rpy)), &up_t)
}

/// P2 base 朝向 FK 一致性（six_axis：6 轴 + mimic 夹爪 + STL 凸包链体）。
/// 非单位安装朝向 R 下：① 运动学 FK（`ik::fk_pose`）对 8 组限位内关节角，
/// TCP − mount = R·(平移-only 导入的 TCP − mount)、接近/开合轴 = R·原轴；
/// ② rapier 仿真（零重力、无桌面，跟踪同一组目标）实测 TCP 与带朝向
/// spec 的 FK 一致，且等于平移-only 仿真的 TCP 绕 mount 旋转 R。
#[cfg(feature = "spec")]
#[test]
fn base_mount_rpy_keeps_fk_and_sim_consistent() {
    use oriera_embodied::ik::fk_pose;
    let rep = fixture(SIX_AXIS);
    let mount = [0.0f32, 1.2, 0.0];
    let rpy = [0.4f32, -0.3, 2.2];
    let import = |rpy: [f32; 3]| {
        let mut o = RobotSpecOpts {
            mount,
            mount_rpy: rpy,
            ..six_axis_opts_with_meshes()
        };
        rep.to_robot_spec(&mut o).expect("six_axis 导入")
    };
    let plain = import([0.0; 3]);
    let rot = import(rpy);
    rot.validate().expect("带朝向导入合法");
    let r = oriera_base_rot(rpy);
    let rel = |p: [f32; 3]| [p[0] - mount[0], p[1] - mount[1], p[2] - mount[2]];
    let near =
        |a: [f32; 3], b: [f32; 3], tol: f32| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol);

    // ① 运动学 FK：确定性伪随机关节角（限位内）
    let mut s = 0x5eed_u64;
    let mut worst = 0.0f32;
    for _ in 0..8 {
        let q: Vec<f32> = plain
            .joints
            .iter()
            .map(|j| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let f = (s >> 33) as f32 / (u32::MAX >> 1) as f32;
                let (lo, hi) = (j.limits.0.max(-2.5), j.limits.1.min(2.5));
                lo + f * (hi - lo)
            })
            .collect();
        let a = fk_pose(&plain, &q);
        let b = fk_pose(&rot, &q);
        let want = apply3(&r, rel(a.tip));
        worst = worst.max(
            rel(b.tip)
                .iter()
                .zip(want)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0, f32::max),
        );
        assert!(
            near(rel(b.tip), want, 1e-4),
            "q {q:?}: TCP {:?} vs R·plain {want:?}",
            b.tip
        );
        assert!(
            near(b.approach, apply3(&r, a.approach), 1e-4),
            "approach 轴随朝向转"
        );
        assert!(near(b.gap, apply3(&r, a.gap), 1e-4), "gap 轴随朝向转");
    }
    eprintln!("[base-rpy] FK 8 组关节角：TCP 最大偏差 {worst:.2e} m");

    // ② 仿真：零重力（去掉随朝向变化的重力矩，只留刚体旋转对称）、远地无桌
    let sim = |spec: RobotSpec, q: &[f32]| -> ([f32; 3], Vec<f32>) {
        let scene = Scene3D {
            ground: ([0.0, -5.0, 0.0], [4.0, 0.05, 4.0]),
            table: None,
        };
        let mut sys = MultibodySystem::from_robot(spec, scene).expect("from_robot");
        let mut world = World::new();
        world.insert_resource(oriera_subsys::Gravity(0.0));
        world.insert_resource(TimeScale(1.0));
        let arm = world.spawn();
        world.insert(
            arm,
            JointTargets {
                angles: q.to_vec(),
                gripper: 0.0,
            },
        );
        world.insert(arm, JointTelemetry::default());
        for i in 0..240u64 {
            sys.run(&ctx(i), &mut world);
        }
        let t = world.get::<JointTelemetry>(arm).unwrap().clone();
        (t.ee_pos, t.angles)
    };
    let q = [0.3f32, -0.4, 0.6, 0.2, -0.5, 0.4];
    let (ee_plain, _) = sim(plain.clone(), &q);
    let (ee_rot, ang_rot) = sim(rot.clone(), &q);
    let fk_rot = fk_pose(&rot, &ang_rot).tip;
    eprintln!(
        "[base-rpy] sim TCP rot {ee_rot:?} · FK(rot, 实测角) {fk_rot:?} · R·(plain−mount)+mount {:?}",
        {
            let w = apply3(&r, rel(ee_plain));
            [w[0] + mount[0], w[1] + mount[1], w[2] + mount[2]]
        }
    );
    assert!(
        near(ee_rot, fk_rot, 2e-3),
        "仿真 TCP 与带朝向 spec 的 FK 一致：{ee_rot:?} vs {fk_rot:?}"
    );
    assert!(
        near(rel(ee_rot), apply3(&r, rel(ee_plain)), 2e-3),
        "仿真 TCP = 平移-only 仿真绕 mount 旋转 R"
    );
}

#[cfg(test)]
mod bin_fixture_gate {
    // P2 真 fixture 料箱：spec.rs:227 门改为「≥5 prop 或 ≥7 fixture」。
    use oriera_embodied::spec::SceneSpec;

    #[test]
    fn fixture_bin_passes_the_gate() {
        // extends 由 load() 解析（from_value 只吃 resolved 全量 spec）
        let spec = SceneSpec::load(include_str!("../specs/official/bin-pick-4dof.json"))
            .expect("fixture bin must load+validate");
        assert!(spec.spec.scene.fixtures.len() >= 7);
        assert!(spec.spec.scene.props.len() < 5);
    }

    #[test]
    fn legacy_five_prop_bin_still_passes() {
        // 回归：5-prop 近似（14fc9bf8 旧路径）仍过闸
        let spec = SceneSpec::load(include_str!("fixtures/bin-pick-4dof-legacy-5prop.json"))
            .expect("legacy 5-prop bin must load+validate");
        assert!(spec.spec.scene.props.len() >= 5);
    }

    #[test]
    fn bare_bin_without_bin_furniture_is_rejected() {
        // 守门不松：bin_pick 无料箱家具（1 prop + 2 fixture）仍拒
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../specs/official/bin-pick-4dof.json")).unwrap();
        let v = oriera_embodied::spec::SceneSpec::load(&serde_json::to_string(&v).unwrap())
            .expect("resolved loads")
            .spec;
        let mut resolved = serde_json::to_value(&v).unwrap();
        let fixtures = resolved["scene"]["fixtures"].as_array_mut().unwrap();
        fixtures.retain(|f| f["name"].as_str().unwrap_or("") == "ground");
        let err = SceneSpec::from_value(resolved).unwrap_err();
        assert!(err.contains("bin_pick"), "{err}");
    }
}
