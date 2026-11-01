//! URDF chain dump — MJCF 全量交叉验证（out/mjcf-crosscheck）的 Rust 侧探针。
//!
//! 读取一份 URDF，把 [`UrdfReport`] 解析结果（逐关节/逐 link +
//! [`UrdfReport::validate_chain`] 报告 + [`UrdfReport::first_revolute_chain`]
//! 到 [`ArmDescription`] 的映射结果）序列化为 JSON 打到 stdout，供
//! `scripts/mjcf_crosscheck.py` 与 MuJoCo MJCF 参照逐参数对比。
//!
//! 只读不改：不触碰 urdf.rs/meshres.rs 本体，不引新依赖（serde_json 由
//! feature `urdf` 自带，见 Cargo.toml 的 `urdf = [... dep:serde_json]`）。
//!
//! 用法：
//! ```text
//! cargo run --example urdf_chain_dump -p oriera-agent --features urdf -- \
//!     <path/to/robot.urdf> [--want N]
//! ```
//! stdout 上只有这一份 JSON（cargo 的编译信息走 stderr）。

use oriera_agent::urdf::UrdfReport;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: urdf_chain_dump <robot.urdf> [--want N]");
        std::process::exit(2);
    };
    let mut want = 7usize;
    while let Some(a) = args.next() {
        if a == "--want" {
            want = args
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(want);
        }
    }

    let xml = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("读 URDF 失败：{e}");
        std::process::exit(1);
    });
    let rep = UrdfReport::parse(&xml).unwrap_or_else(|e| {
        eprintln!("URDF 解析失败：{e}");
        std::process::exit(1);
    });

    // 链序（first_revolute_chain 同款走法：root 向下、fixed 透明吸收），
    // 让 Python 侧按链序而不是文档序对比。
    let chain: Vec<String> = {
        let mut chain = Vec::new();
        let mut link = rep.root.clone();
        while let Some(j) = rep
            .joints
            .iter()
            .find(|j| j.parent == link && matches!(j.kind.as_str(), "revolute" | "continuous"))
        {
            chain.push(j.name.clone());
            link = j.child.clone();
            if chain.len() >= want {
                break;
            }
        }
        chain
    };

    // validate_chain 报告原文（人话列表，逐条入 JSON）。
    let issues = rep.validate_chain(want);

    // ArmDescription 映射结果（OriEra 实际落进 TableTopEnv 的量）。
    let arm = rep.first_revolute_chain(4).ok();

    let joints = serde_json::json!(rep
        .joints
        .iter()
        .map(|j| serde_json::json!({
            "name": j.name,
            "kind": j.kind,
            "parent": j.parent,
            "child": j.child,
            "origin_xyz": j.origin_xyz,
            "origin_rpy": j.origin_rpy,
            "axis": j.axis,
            "limit_lower": j.limit_lower,
            "limit_upper": j.limit_upper,
            "damping": j.damping,
            "effort": j.effort,
        }))
        .collect::<Vec<_>>());

    let links = serde_json::json!(rep
        .links
        .iter()
        .map(|l| serde_json::json!({
            "name": l.name,
            "geometry": match &l.geometry {
                Some(oriera_agent::urdf::UrdfGeom::Box(s)) => {
                    serde_json::json!({"kind": "box", "size": s})
                }
                Some(oriera_agent::urdf::UrdfGeom::Cylinder { radius, length }) => {
                    serde_json::json!({"kind": "cylinder", "radius": radius, "length": length})
                }
                Some(oriera_agent::urdf::UrdfGeom::Sphere { radius }) => {
                    serde_json::json!({"kind": "sphere", "radius": radius})
                }
                Some(oriera_agent::urdf::UrdfGeom::Mesh { uri }) => {
                    serde_json::json!({"kind": "mesh", "uri": uri})
                }
                None => serde_json::Value::Null,
            },
            "inertial": l.inertial.as_ref().map(|i| serde_json::json!({
                "mass": i.mass,
                "ixx": i.ixx,
                "iyy": i.iyy,
                "izz": i.izz,
                "ixy": i.ixy,
                "ixz": i.ixz,
                "iyz": i.iyz,
                "origin_xyz": i.origin_xyz,
            })),
        }))
        .collect::<Vec<_>>());

    let arm_json = arm.as_ref().map(|(desc, skipped)| {
        serde_json::json!({
            "base": desc.base,
            "shoulder_height": desc.shoulder_height,
            "links": desc.links,
            "link_radius": desc.link_radius,
            "home": desc.home,
            "limits": desc.limits,
            "kp": desc.kp,
            "kd": desc.kd,
            "mass_scale": desc.mass_scale,
            "passive_damping": desc.passive_damping,
            "skipped": skipped,
        })
    });

    let out = serde_json::json!({
        "tool": "urdf_chain_dump",
        "urdf": path,
        "want": want,
        "root": rep.root,
        "skipped": rep.skipped,
        "chain_joints": chain,
        "validate_chain": issues,
        "joints": joints,
        "links": links,
        "arm_description": arm_json,
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("JSON 序列化")
    );
}
