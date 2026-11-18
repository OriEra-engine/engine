//! 双端共读金样(#153 M1):demo 05/11 的 JS 蒙皮镜像(`skin.js`)与本测试
//! 同读 `contracts/golden/skin_golden.json`。
//!
//! 两个覆盖面:
//! - **合成 GLB**(内嵌 base64):双关节(root→child)、两三角形(各绑一关节)、
//!   LINEAR 平移+旋转通道——覆盖采样/短弧 slerp/图遍历/面板乘法的主要分支;
//! - **CesiumMan 真模型**(复用 oriera-asset fixtures,不重复入库):19 关节/
//!   57 通道/accessor 全 byteOffset——真实导出器布局下面板逐项对拍。
//!
//! 容差域(写进金样 meta):JS 镜像 f64 计算,Rust 混合 f32/f64(局部矩阵
//! 合成处逐元 cast f32),palette 容差 1e-5 吸收超越函数边界;蒙皮顶点 1e-4。
//!
//! 重锚:`ORIERA_SKIN_BLESS=1 cargo test -p oriera-asset --test cross_language_skin`

use oriera_asset::glb::{decode_glb_rig, joint_matrices_at, skin_vertices};
use serde_json::{json, Value};
use std::path::PathBuf;

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/golden/skin_golden.json")
}

/// 构造合法 GLB:JSON chunk(4 字节对齐补空格)+ BIN chunk(与 glb.rs 单测同式)。
fn build_glb(json: &Value, bin: &[u8]) -> Vec<u8> {
    let json_raw = serde_json::to_vec(json).unwrap();
    let json_pad = json_raw.len().div_ceil(4) * 4;
    let bin_pad = bin.len().div_ceil(4) * 4;
    let total = 12 + 8 + json_pad + 8 + bin_pad;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_pad as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_raw);
    out.extend(std::iter::repeat_n(b' ', json_pad - json_raw.len()));
    out.extend_from_slice(&(bin_pad as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(bin);
    out.extend(std::iter::repeat_n(0, bin_pad - bin.len()));
    out
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

/// 合成双关节 rig:root(T 动画)→ child(R 动画,绕 Z 摆动 0→0.6rad→0);
/// 三角 A 绑 root、三角 B 绑 child。IBM:root=单位、child=T(0,-1,0)。
fn synthetic_rig() -> (Value, Vec<u8>) {
    let mut bin: Vec<u8> = Vec::new();
    let mut view = |bytes: &[u8]| -> (i64, i64) {
        let off = bin.len() as i64;
        bin.extend_from_slice(bytes);
        (off, bytes.len() as i64)
    };

    // 0: 动画时间(两通道共用,SCALAR f32 ×3)
    let times: [f32; 3] = [0.0, 0.5, 1.0];
    let mut t_bytes = Vec::new();
    for t in times {
        t_bytes.extend_from_slice(&t.to_le_bytes());
    }
    let (t_off, t_len) = view(&t_bytes);
    // 1: root 平移值(VEC3 ×3)
    let mut tr_bytes = Vec::new();
    for v in [[0.0f32, 0.0, 0.0], [0.0, 0.2, 0.0], [0.0, 0.0, 0.0]] {
        for c in v {
            tr_bytes.extend_from_slice(&c.to_le_bytes());
        }
    }
    let (tr_off, tr_len) = view(&tr_bytes);
    // 2: child 旋转值(VEC4 ×3,绕 Z:0 → 0.6rad → 0)
    let mut rot_bytes = Vec::new();
    let (s, c) = (0.3f32.sin(), 0.3f32.cos());
    for q in [
        [0.0f32, 0.0, 0.0, 1.0],
        [0.0, 0.0, s, c],
        [0.0, 0.0, 0.0, 1.0],
    ] {
        for v in q {
            rot_bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    let (rot_off, rot_len) = view(&rot_bytes);
    // 3: IBM(MAT4 ×2:root=单位;child=T(0,-1,0))
    let mut ibm_bytes = Vec::new();
    for m in [
        [1.0f32, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ] {
        for v in m {
            ibm_bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    // glTF MAT4 按列主序写字节:data[c*4+r] = m[r][c](与修复后的 Rust 读取
    // 同一约定)。child 的逆绑定 = 逆(T(0,1,0)) = T(0,-1,0)(平移在第 4 列)。
    let child_ibm: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, -1.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    // 列主序写:外层列、内层行(data[c*4+r] = m[r][c])
    for c in 0..4 {
        for row in &child_ibm {
            ibm_bytes.extend_from_slice(&row[c].to_le_bytes());
        }
    }
    let (ibm_off, ibm_len) = view(&ibm_bytes);
    // 4: POSITION(VEC3 ×6:三角 A 绑 root,三角 B 绑 child)
    let pos: [[f32; 3]; 6] = [
        [-0.5, 1.5, 0.0],
        [0.5, 1.5, 0.0],
        [0.0, 2.0, 0.0],
        [-0.5, 0.5, 0.0],
        [0.5, 0.5, 0.0],
        [0.0, 1.0, 0.0],
    ];
    let mut pos_bytes = Vec::new();
    for v in pos {
        for c in v {
            pos_bytes.extend_from_slice(&c.to_le_bytes());
        }
    }
    let (pos_off, pos_len) = view(&pos_bytes);
    // 5: JOINTS_0(VEC4 u16 ×6)
    let mut j_bytes = Vec::new();
    for j in [
        [0u16, 0, 0, 0],
        [0, 0, 0, 0],
        [0, 0, 0, 0],
        [1, 0, 0, 0],
        [1, 0, 0, 0],
        [1, 0, 0, 0],
    ] {
        for v in j {
            j_bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    let (j_off, j_len) = view(&j_bytes);
    // 6: WEIGHTS_0(VEC4 f32 ×6)
    let mut w_bytes = Vec::new();
    for w in [
        [1.0f32, 0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, 0.0],
    ] {
        for v in w {
            w_bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    let (w_off, w_len) = view(&w_bytes);
    // 7: 索引(u32 ×6)
    let mut i_bytes = Vec::new();
    for i in [0u32, 1, 2, 3, 4, 5] {
        i_bytes.extend_from_slice(&i.to_le_bytes());
    }
    let (i_off, i_len) = view(&i_bytes);

    let bv = |off: i64, len: i64| json!({"buffer": 0, "byteOffset": off, "byteLength": len});
    let doc = json!({
        "scene": 0,
        "scenes": [{"nodes": [0, 2]}],
        "nodes": [
            {"name": "root", "translation": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0], "children": [1]},
            {"name": "child", "translation": [0.0, 1.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0]},
            {"name": "mesh-node", "mesh": 0, "skin": 0}
        ],
        "skins": [{"joints": [0, 1], "inverseBindMatrices": 3}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 4, "JOINTS_0": 5, "WEIGHTS_0": 6}, "indices": 7, "material": 0}]}],
        "materials": [{"pbrMetallicRoughness": {"baseColorFactor": [0.8, 0.6, 0.4, 1.0], "metallicFactor": 0.0, "roughnessFactor": 0.9}}],
        "animations": [{
            "name": "flap",
            "channels": [
                {"target": {"node": 0, "path": "translation"}, "sampler": 0},
                {"target": {"node": 1, "path": "rotation"}, "sampler": 1}
            ],
            "samplers": [
                {"input": 0, "output": 1, "interpolation": "LINEAR"},
                {"input": 0, "output": 2, "interpolation": "LINEAR"}
            ]
        }],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "SCALAR"},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4"},
            {"bufferView": 3, "componentType": 5126, "count": 2, "type": "MAT4"},
            {"bufferView": 4, "componentType": 5126, "count": 6, "type": "VEC3"},
            {"bufferView": 5, "componentType": 5123, "count": 6, "type": "VEC4"},
            {"bufferView": 6, "componentType": 5126, "count": 6, "type": "VEC4"},
            {"bufferView": 7, "componentType": 5125, "count": 6, "type": "SCALAR"}
        ],
        "bufferViews": [
            bv(t_off, t_len), bv(tr_off, tr_len), bv(rot_off, rot_len), bv(ibm_off, ibm_len),
            bv(pos_off, pos_len), bv(j_off, j_len), bv(w_off, w_len), bv(i_off, i_len)
        ],
        "buffers": [{"byteLength": bin.len() as i64}]
    });
    (doc, bin)
}

fn palette_json(palettes: &[Vec<Vec<[[f32; 4]; 4]>>]) -> Value {
    json!(palettes
        .iter()
        .map(|times| times
            .iter()
            .map(|skin| skin
                .iter()
                .map(|m| {
                    let mut flat = [0f32; 16];
                    for (r, row) in m.iter().enumerate() {
                        for (c, v) in row.iter().enumerate() {
                            flat[r * 4 + c] = *v;
                        }
                    }
                    flat
                })
                .collect::<Vec<_>>())
            .collect::<Vec<_>>())
        .collect::<Vec<_>>())
}

#[test]
fn cross_language_skin_golden_matches_js_mirror_contract() {
    // ---- 合成 rig:算面板 + 蒙皮顶点 ----
    let (doc, bin) = synthetic_rig();
    let glb = build_glb(&doc, &bin);
    let rig = decode_glb_rig(&glb).expect("synthetic rig must decode");
    assert_eq!(rig.skins.len(), 1);
    assert_eq!(rig.skins[0].joints.len(), 2);
    assert_eq!(rig.animations.len(), 1);
    let duration = rig.animations[0].duration;
    assert!((duration - 1.0).abs() < 1e-6);

    let times = [0.0f32, 0.25, 0.5, 1.0];
    let synth_palettes: Vec<Vec<Vec<[[f32; 4]; 4]>>> = times
        .iter()
        .map(|&t| joint_matrices_at(&rig, Some(0), t))
        .collect();
    let skinned_half = skin_vertices(&rig.primitives[0], &synth_palettes[2][0]);

    // ---- CesiumMan 真模型面板 ----
    let fix = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/CesiumMan.glb");
    let cm = decode_glb_rig(&std::fs::read(&fix).expect("CesiumMan fixture")).expect("decodes");
    let cm_dur = cm.animations[0].duration;
    let cm_times = [0.0f32, cm_dur * 0.5];
    let cm_palettes: Vec<Vec<Vec<[[f32; 4]; 4]>>> = cm_times
        .iter()
        .map(|&t| joint_matrices_at(&cm, Some(0), t))
        .collect();

    let fixture = json!({
        "meta": {
            "name": "skin_golden",
            "generated_by": "oriera-asset tests/cross_language_skin.rs (ORIERA_SKIN_BLESS=1)",
            "consumed_by": [
                "crates/oriera-asset/tests/cross_language_skin.rs",
                "examples/05-gltf-model/engine/skin.test.mjs",
                "examples/11-ai-cafe/engine/skin.test.mjs"
            ],
            "conventions": {
                "palette": "per skin: joint_world · inverseBind(row-major 16);joint_world 由 FIFO 图遍历合成(动画 TRS 覆盖 + 静态回退)",
                "slerp": "点积为负先翻短弧;LINEAR 对 T/S lerp;STEP 保持前值;越界钳制两端",
                "ibm_layout": "glTF MAT4 column-major;Rust/JS 双侧同样按列主序读入后转行主序 Mat4",
                "note": "JS 镜像 f64 计算;Rust 混合 f32/f64(局部矩阵合成处逐元 cast f32)——palette 1e-5 吸收超越函数(acos/sin)f32-vs-f64 边界"
            },
            "tolerances": {"palette_abs": 1e-5, "skinned_pos_abs": 1e-4}
        },
        "synthetic": {
            "glb_base64": base64_encode(&glb),
            "joint_count": rig.skins[0].joints.len(),
            "duration": duration,
            "sample_times": times,
            "palettes": palette_json(&synth_palettes),
            "skinned_positions_at_half": skinned_half
        },
        "cesiumman": {
            "fixture": "crates/oriera-asset/tests/fixtures/CesiumMan.glb",
            "joint_count": cm.skins[0].joints.len(),
            "duration": cm_dur,
            "sample_times": cm_times,
            "palettes": palette_json(&cm_palettes)
        }
    });

    let path = golden_path();
    if std::env::var_os("ORIERA_SKIN_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&fixture).unwrap() + "\n",
        )
        .unwrap();
    }
    let on_disk = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {} ({e}); bless with ORIERA_SKIN_BLESS=1",
            path.display()
        )
    });
    let stored: Value = serde_json::from_slice(&on_disk).unwrap();

    // 结构面:数量/时长/时间点精确
    assert_eq!(
        stored["synthetic"]["joint_count"],
        fixture["synthetic"]["joint_count"]
    );
    assert_eq!(
        stored["cesiumman"]["joint_count"],
        fixture["cesiumman"]["joint_count"]
    );
    assert!((stored["synthetic"]["duration"].as_f64().unwrap() - 1.0).abs() < 1e-6);

    // 数值面:palette 容差(Rust 自比应为逐位一致,取 1e-7 保守)
    // 递归展平对拍(palette 行 flat16 / 蒙皮顶点 [[x,y,z]×6] 两种形状通吃)
    let near = |a: &Value, b: &Value, tol: f64, what: &str| {
        fn walk(a: &Value, b: &Value, tol: f64, what: &str) {
            match (a, b) {
                (Value::Array(xs), Value::Array(ys)) => {
                    assert_eq!(xs.len(), ys.len(), "{what}: length mismatch");
                    for (x, y) in xs.iter().zip(ys) {
                        walk(x, y, tol, what);
                    }
                }
                (Value::Number(x), Value::Number(y)) => {
                    let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
                    assert!((x - y).abs() <= tol, "{what}: {x} vs {y} beyond {tol}");
                }
                _ => panic!("{what}: shape mismatch {a} vs {b}"),
            }
        }
        walk(a, b, tol, what);
    };
    let pal = |v: &Value| -> Vec<Vec<Vec<Vec<f64>>>> { serde_json::from_value(v.clone()).unwrap() };
    for (s, f) in pal(&stored["synthetic"]["palettes"])
        .iter()
        .zip(pal(&fixture["synthetic"]["palettes"]))
    {
        for (sm, fm) in s.iter().zip(&f) {
            for (m_s, m_f) in sm.iter().zip(fm) {
                near(&json!(m_s), &json!(m_f), 1e-7, "synthetic palette");
            }
        }
    }
    for (s, f) in pal(&stored["cesiumman"]["palettes"])
        .iter()
        .zip(pal(&fixture["cesiumman"]["palettes"]))
    {
        for (sm, fm) in s.iter().zip(&f) {
            for (m_s, m_f) in sm.iter().zip(fm) {
                near(&json!(m_s), &json!(m_f), 1e-7, "cesiumman palette");
            }
        }
    }
    near(
        &stored["synthetic"]["skinned_positions_at_half"],
        &fixture["synthetic"]["skinned_positions_at_half"],
        1e-7,
        "skinned positions",
    );
}
