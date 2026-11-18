//! 双端共读金样（PLAN-splat v2 M2，#188 评审 04/09 号门槛）：
//! Rust 与 demo 16 的 JS 镜像（`examples/16-gaussian-hall/engine/splat.js`）同读
//! `contracts/golden/splat_golden.json`，CI 双腿对拍。
//!
//! 容差域（=「Math.fround 边界约定」的落地形式，写进金样 meta）：
//! - **原始透传字段**（positions/sh_dc）按 u32 位型**精确**对拍——解码不碰数值；
//! - **激活值**（exp/sigmoid）容差 1e-6——Rust `f32::exp` 与 JS `Math.exp` 允许 1ulp；
//! - **投影量** means/conics 容差 5e-3、extents 容差 1.0（ceil 离散值）——f32 vs f64 累计；
//! - **排序**精确对拍——金样深度两两分离 0.5，任何 1ulp 级差异都不会翻转次序。
//!
//! 重锚（仅在有意改变解码/投影语义时）：
//! `ORIERA_SPLAT_BLESS=1 cargo test -p oriera-splat --test cross_language`

use oriera_splat::{decode_splat_ply, depth_order, project, SplatCamera};
use std::path::PathBuf;

const CAMERA: [f32; 3] = [0.0, 0.0, -4.0];

/// (位置, DC, w 在前四元数, logit 不透明度, log 尺度)
type SplatRow = ([f64; 3], [f64; 3], [f64; 4], f64, [f64; 3]);

/// 8 个手搓 splat：深度 = 0.4 + 0.5k，两两分离 0.5（排序对 1ulp 级差异免疫）；
/// 含倾斜四元数、非线性透明度、三通道各异的 DC——覆盖激活与投影的主要分支。
fn splat_table() -> Vec<SplatRow> {
    (0..8)
        .map(|k| {
            let z = -3.6 + 0.5 * k as f64;
            (
                [
                    0.15 * (1.7 * k as f64).sin(),
                    0.12 * (2.3 * k as f64).cos(),
                    z,
                ],
                [0.04 + 0.008 * k as f64, 0.02 + 0.004 * k as f64, 0.03],
                [0.0, 0.0, (0.2 * k as f64).sin(), (0.2 * k as f64).cos()],
                -1.0 + 0.25 * k as f64,
                [
                    ((k % 3) as f64) * 0.4 - 0.3,
                    (((k + 1) % 3) as f64) * 0.4 - 0.3,
                    (((k + 2) % 3) as f64) * 0.4 - 0.3,
                ],
            )
        })
        .collect()
}

/// INRIA 布局写出器（含 nx/ny/nz 与 f_rest_0..8 垃圾值——锻炼按名映射与跳过）。
fn build_ply(table: &[SplatRow]) -> Vec<u8> {
    let mut head = String::from(
        "ply\nformat binary_little_endian 1.0\ncomment oriera cross-language golden fixture\n\
         obj_info depth_separation=0.5\nelement vertex 8\n",
    );
    for name in ["x", "y", "z", "nx", "ny", "nz"] {
        head.push_str(&format!("property float {name}\n"));
    }
    for k in 0..3 {
        head.push_str(&format!("property float f_dc_{k}\n"));
    }
    for j in 0..9 {
        head.push_str(&format!("property float f_rest_{j}\n"));
    }
    head.push_str("property float opacity\n");
    for name in ["scale_0", "scale_1", "scale_2"] {
        head.push_str(&format!("property float {name}\n"));
    }
    for name in ["rot_0", "rot_1", "rot_2", "rot_3"] {
        head.push_str(&format!("property float {name}\n"));
    }
    head.push_str("end_header\n");
    let mut out = head.into_bytes();
    for &(pos, dc, rot, logit, log_scale) in table {
        let mut row: Vec<f32> = pos.iter().map(|v| *v as f32).collect();
        row.extend([0.0f32, 0.0, 1.0]);
        row.extend(dc.iter().map(|v| *v as f32));
        row.extend((0..9).map(|j| 0.5 * j as f32 - 2.0));
        row.push(logit as f32);
        row.extend(log_scale.iter().map(|v| *v as f32));
        // PLY rot_0..3 是 w 在前
        row.extend([rot[3] as f32, rot[0] as f32, rot[1] as f32, rot[2] as f32]);
        for v in row {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

fn golden_path() -> PathBuf {
    // crate 在 engine/crates/oriera-splat:两级上到 engine 根,再进 contracts/
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/golden/splat_golden.json")
}

#[test]
fn cross_language_golden_matches_js_mirror_contract() {
    let table = splat_table();
    let ply = build_ply(&table);
    let cloud = decode_splat_ply(&ply).expect("fixture PLY must decode");

    let camera = SplatCamera::look_at(CAMERA, [0.0; 3], [0.0, 1.0, 0.0], 60.0, 64, 48);
    let projected = project(&cloud, &camera);
    let order = depth_order(&projected);
    // 槽位 → 原始下标（JS 侧直接拿原始下标）
    let order_indices: Vec<u32> = order.iter().map(|&s| projected[s as usize].index).collect();

    let fixture = serde_json::json!({
        "meta": {
            "name": "splat_golden",
            "generated_by": "oriera-splat tests/cross_language.rs (ORIERA_SPLAT_BLESS=1)",
            "consumed_by": [
                "crates/oriera-splat/tests/cross_language.rs",
                "examples/16-gaussian-hall/engine/splat.test.mjs"
            ],
            "conventions": {
                "ply": "standard 3DGS binary_little_endian, mapped by property name",
                "fields": "SplatCloud stores KHR-activated values (linear scale/opacity, xyzw unit quaternion)",
                "order": "front-to-back; ties broken by original index",
                "note": "JS mirror computes in f64; raw passthrough fields must match bit-exactly, exp/sigmoid allow 1 ulp, projection accumulates f32-vs-f64 drift — hence the tolerance domains below"
            },
            "tolerances": {
                "activated_abs": 1e-6,
                "means_abs": 5e-3,
                "conics_abs": 5e-3,
                "extents_abs": 1.0
            },
            "camera": {
                "eye": CAMERA,
                "target": [0.0, 0.0, 0.0],
                "up": [0.0, 1.0, 0.0],
                "fov_y_deg": 60.0,
                "width": 64,
                "height": 48
            }
        },
        "ply_base64": base64_encode(&ply),
        "expect": {
            "count": cloud.len(),
            "sh_degree": cloud.sh_degree,
            "comments": cloud.comments,
            "positions_bits": bits3(&cloud.positions),
            "sh_dc_bits": bits3(&cloud.sh_dc),
            "scales": cloud.scales.clone(),
            "rotations": cloud.rotations.clone(),
            "opacities": cloud.opacities.clone(),
            "project": {
                "surviving": projected.iter().map(|p| p.index).collect::<Vec<_>>(),
                "depth_order": order_indices,
                "means": projected.iter().map(|p| p.mean.to_vec()).collect::<Vec<_>>(),
                "extents": projected.iter().map(|p| p.extent.to_vec()).collect::<Vec<_>>(),
                "conics": projected.iter().map(|p| p.conic.to_vec()).collect::<Vec<_>>()
            }
        }
    });

    let path = golden_path();
    if std::env::var_os("ORIERA_SPLAT_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&fixture).unwrap() + "\n",
        )
        .unwrap();
    }

    let on_disk = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {} ({e}); bless with ORIERA_SPLAT_BLESS=1",
            path.display()
        )
    });
    let stored: serde_json::Value = serde_json::from_slice(&on_disk).unwrap();
    let fresh = fixture["expect"].clone();
    let disk = stored["expect"].clone();

    assert_eq!(disk["count"], fresh["count"]);
    assert_eq!(disk["sh_degree"], fresh["sh_degree"]);
    assert_eq!(disk["comments"], fresh["comments"]);
    assert_eq!(
        disk["positions_bits"], fresh["positions_bits"],
        "positions are raw passthrough: must be bit-exact"
    );
    assert_eq!(
        disk["sh_dc_bits"], fresh["sh_dc_bits"],
        "sh_dc is raw passthrough: must be bit-exact"
    );
    assert_eq!(
        disk["project"]["depth_order"], fresh["project"]["depth_order"],
        "depth order must be exact (depths separated by 0.5)"
    );

    let t = stored["meta"]["tolerances"].clone();
    let near = |a: serde_json::Value, b: serde_json::Value, tol: f64| {
        let (a, b): (Vec<f64>, Vec<f64>) = (
            serde_json::from_value(a).unwrap(),
            serde_json::from_value(b).unwrap(),
        );
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
    };
    for key in ["scales", "rotations"] {
        let (xs, ys): (Vec<Vec<f64>>, Vec<Vec<f64>>) = (
            serde_json::from_value(disk[key].clone()).unwrap(),
            serde_json::from_value(fresh[key].clone()).unwrap(),
        );
        for (a, b) in xs.iter().zip(&ys) {
            assert!(
                near(
                    serde_json::to_value(a).unwrap(),
                    serde_json::to_value(b).unwrap(),
                    t["activated_abs"].as_f64().unwrap()
                ),
                "{key} beyond activated tolerance"
            );
        }
    }
    let (os, oo): (Vec<f64>, Vec<f64>) = (
        serde_json::from_value(disk["opacities"].clone()).unwrap(),
        serde_json::from_value(fresh["opacities"].clone()).unwrap(),
    );
    for (a, b) in os.iter().zip(&oo) {
        assert!((a - b).abs() <= t["activated_abs"].as_f64().unwrap());
    }
    for (key, tol_key) in [
        ("means", "means_abs"),
        ("conics", "conics_abs"),
        ("extents", "extents_abs"),
    ] {
        let (xs, ys): (Vec<Vec<f64>>, Vec<Vec<f64>>) = (
            serde_json::from_value(disk["project"][key].clone()).unwrap(),
            serde_json::from_value(fresh["project"][key].clone()).unwrap(),
        );
        for (a, b) in xs.iter().zip(&ys) {
            assert!(
                near(
                    serde_json::to_value(a).unwrap(),
                    serde_json::to_value(b).unwrap(),
                    t[tol_key].as_f64().unwrap()
                ),
                "{key} beyond projected tolerance"
            );
        }
    }
}

fn bits3(v: &[[f32; 3]]) -> Vec<Vec<u32>> {
    v.iter()
        .map(|p| p.iter().map(|c| c.to_bits()).collect())
        .collect()
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
