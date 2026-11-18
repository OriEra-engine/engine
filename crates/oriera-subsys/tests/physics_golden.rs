//! P0-5「确定性验证扩面」物理金样（战略路线图 02-STATE §2 W4~W6 判据）。
//!
//! 渲染侧金样（splat/skin，1e-5）早已入 CI；本测试把同一纪律推广到
//! **物理轨迹**：固定程序化场景（桌面 4 轴预设臂 + 地面/桌面板台）+
//! 两阶段关节指令（保持 home 60 拍 → 阶跃目标保持 180 拍），在检查点
//! 采集 `sim_fingerprint` 轨迹与末端位姿，与入库金样比对。
//!
//! - 同构建位确定性：同场景连跑两次，指纹轨迹必须逐位相等（rapier
//!   same-build deterministic 契约的持续回归哨兵）；
//! - 金样比对：指纹逐位相等；末端位姿/关节角浮点逐项 ≤ 1e-5
//!   （吸收跨平台 libm 末位差，与 splat 金样 1/255 同思路）。
//!
//! 重锚（仅在有意改变物理语义时）：
//! `ORIERA_SUBSYS_BLESS=1 cargo test -p oriera-subsys --features rigid3d --test physics_golden`
#![cfg(feature = "rigid3d")]

use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Entity, World};
use oriera_subsys::{
    sim_fingerprint, Gravity, JointTargets, JointTelemetry, MultibodySystem, RobotSpec, Scene3D,
    TimeScale,
};

const DT: f32 = 1.0 / 60.0;
const HOLD_TICKS: u64 = 60;
const MOVE_TICKS: u64 = 180;
const TOTAL: u64 = HOLD_TICKS + MOVE_TICKS;
/// 指纹检查点（拍号，含首拍与末拍）。
const CHECKPOINTS: [u64; 5] = [1, 60, 120, 180, 240];

fn ctx(step: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(step),
        dt: DT,
        now: Timestamp(0),
        grants: Grants::EMPTY,
    }
}

/// 预设 4 轴桌面臂 + 默认地/桌板台；指令轨迹 = 保持 home → 每关节
/// +0.25 rad 的阶跃目标。全部量由 preset 派生，不引入自由参数。
fn scenario() -> (World, MultibodySystem, Entity, Vec<f32>, Vec<f32>) {
    let spec = RobotSpec::from(&oriera_subsys::ArmDescription::default());
    let home = spec.home.clone();
    let target: Vec<f32> = home.iter().map(|a| a + 0.25).collect();

    let mut world = World::new();
    world.insert_resource(Gravity(9.8));
    world.insert_resource(TimeScale(1.0));
    let arm = world.spawn();
    world.insert(
        arm,
        JointTargets {
            angles: home.clone(),
            gripper: 0.0,
        },
    );
    world.insert(arm, JointTelemetry::default());
    let sys = MultibodySystem::from_robot_fixtures(spec, &Scene3D::default().fixture_slabs())
        .expect("preset spec builds");
    (world, sys, arm, home, target)
}

/// 跑完整两阶段轨迹，返回检查点指纹与末端终态快照。
fn run_track() -> (Vec<u64>, [f32; 3], [f32; 4], Vec<f32>) {
    let (mut world, mut sys, arm, home, target) = scenario();
    let mut fps = Vec::new();
    let mut next_cp = 0usize;
    for tick in 1..=TOTAL {
        let angles: &[f32] = if tick <= HOLD_TICKS { &home } else { &target };
        if let Some(t) = world.get_mut::<JointTargets>(arm) {
            t.angles = angles.to_vec();
        }
        sys.run(&ctx(tick), &mut world);
        if next_cp < CHECKPOINTS.len() && CHECKPOINTS[next_cp] == tick {
            fps.push(sim_fingerprint(&world));
            next_cp += 1;
        }
    }
    let (px, pq) = sys.ee_pose();
    let angles = sys.angles();
    (fps, px, pq, angles)
}

fn parse_and_compare(golden: &str, fps: &[u64], px: &[f32; 3], pq: &[f32; 4], angles: &[f32]) {
    // 平台标签：指纹逐位只在 bless 平台比对——rapier 同构建内逐位确定，
    // 但 f32 积分的 libm/simd 末位差跨 OS 必然漂移（CI Ubuntu vs 本机
    // Windows 首跑即实证）。跨平台只比姿态/关节角（容差 1e-3，物理收敛态）；
    // 同构建位确定性由测试内的双跑断言另行锁定（splat 金样同款双层设计）。
    let this_platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let mut golden_platform: Option<String> = None;
    let mut cross_platform = false;
    let mut got_fps = fps.iter();
    let mut got_floats = px.iter().chain(pq.iter()).chain(angles.iter());
    for line in golden
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let (tag, rest) = line
            .split_once(' ')
            .unwrap_or_else(|| panic!("bad golden line: {line}"));
        match tag {
            "platform" => golden_platform = Some(rest.to_string()),
            "fp" => {
                let want: u64 = u64::from_str_radix(rest, 16)
                    .unwrap_or_else(|e| panic!("bad golden fingerprint {rest}: {e}"));
                let got = got_fps
                    .next()
                    .unwrap_or_else(|| panic!("golden has more fingerprints than run produced"));
                if golden_platform.as_deref() == Some(this_platform.as_str()) {
                    assert_eq!(
                        *got, want,
                        "sim fingerprint drift at checkpoint (同平台逐位漂移——rapier 版本或积分语义变了，若有意请重锚)"
                    );
                } else {
                    cross_platform = true;
                }
            }
            "f" => {
                let want: f32 = rest
                    .parse()
                    .unwrap_or_else(|e| panic!("bad golden float {rest}: {e}"));
                let got = got_floats
                    .next()
                    .unwrap_or_else(|| panic!("golden has more floats than run produced"));
                let d = (want - got).abs();
                let tol = if cross_platform { 1e-3 } else { 1e-5 };
                assert!(
                    d <= tol,
                    "float drift {want} vs {got} (Δ={d} > {tol}, cross_platform={cross_platform})"
                );
            }
            other => panic!("unknown golden tag {other}"),
        }
    }
}

#[test]
fn physics_trajectory_is_deterministic_and_matches_golden() {
    let (fps, px, pq, angles) = run_track();

    // 覆盖率哨兵：检查点采满、终态有限且关节角齐——金样必须真有内容。
    assert_eq!(
        fps.len(),
        CHECKPOINTS.len(),
        "checkpoint fingerprints missing"
    );
    assert!(
        px.iter().chain(pq.iter()).all(|v| v.is_finite()),
        "ee pose not finite"
    );
    assert!(!angles.is_empty(), "no joint angles readback");

    // 同构建位确定性：第二次独立运行必须与首次逐位一致。
    let (fps2, px2, pq2, angles2) = run_track();
    assert_eq!(
        fps, fps2,
        "two identical runs produced different fingerprints"
    );
    assert_eq!(px, px2, "ee position differs across identical runs");
    assert_eq!(pq, pq2, "ee rotation differs across identical runs");
    assert_eq!(angles, angles2, "joint angles differ across identical runs");

    // 金样比对。
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/physics_track_4axis.txt");
    if std::env::var_os("ORIERA_SUBSYS_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut buf = String::from(
            "# oriera-subsys 物理金样：预设 4 轴臂两阶段轨迹（指纹逐位限 bless 平台 / 姿态 1e-5，跨平台 1e-3）\n",
        );
        buf.push_str(&format!(
            "platform {}-{}\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        buf.push_str(&format!("# 检查点拍序: {:?}\n", CHECKPOINTS));
        for fp in &fps {
            buf.push_str(&format!("fp {fp:016x}\n"));
        }
        for v in px.iter().chain(pq.iter()).chain(angles.iter()) {
            buf.push_str(&format!("f {v:.6}\n"));
        }
        std::fs::write(&path, buf).unwrap();
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {} ({e}); bless with ORIERA_SUBSYS_BLESS=1",
            path.display()
        )
    });
    parse_and_compare(&golden, &fps, &px, &pq, &angles);
}
