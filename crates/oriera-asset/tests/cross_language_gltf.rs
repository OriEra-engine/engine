//! 双端共读金样(PLAN-splat v2 → #152 第三刀):demo 05 的 JS glTF 镜像
//! (`examples/05-gltf-model/engine/gltf.js`)与本测试同读
//! `contracts/golden/gltf_golden.json`。
//!
//! 手搓 GLB:两 mesh×材质(其一带 PNG 纹理字节摊位)、父子 TRS、accessor
//! byteOffset、交错 POSITION——覆盖场景解码的主要分支。
//! 容差域(同 M2 约定,写进金样 meta):原始透传(索引/材质因子/纹理字节)精确;
//! 变换后顶点/法线 f32-vs-f64 累计容差 1e-4。
//!
//! 重锚:`ORIERA_GLTF_BLESS=1 cargo test -p oriera-asset --test cross_language_gltf`

use oriera_asset::glb::decode_glb_scene;
use std::path::PathBuf;

/// 两 mesh 场景文档:parent(T+S) → child(rot, mesh0, 材质0),root(mesh1, 材质1)。
/// POSITION 交错布局 + accessor byteOffset;mesh1 材质带 PNG 纹理字节摊位。
fn fixture_scene() -> (Value, Vec<u8>) {
    use serde_json::json;

    // bin 布局:交错 POSITION(mesh0, stride16, acc byteOffset 4)@0;
    // NORMAL(mesh0)@偏移另计;UV(mesh1)@..;索引@..;纹理假 PNG @..
    let mut bin: Vec<u8> = Vec::new();
    // 交错 POSITION + 垫片:3 顶点 × (12B pos + 4B pad) = 48B,前 4B 是 acc byteOffset 的跳跃区
    bin.extend_from_slice(&[0xEE; 4]);
    for p in [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]] {
        for v in p {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        bin.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    }
    let pos_view_len = bin.len() as i64; // 52
                                         // NORMAL(mesh0):3×12 = 36B,含 acc byteOffset 0
    let norm_at = bin.len();
    for _ in 0..3 {
        for v in [0.0f32, 0.0, 1.0] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
    }
    // POSITION(mesh1):3×12(独立顶点,验证多 mesh 各自取数)
    let pos1_at = bin.len();
    for p in [[0.5f32, 0.5, 0.0], [-0.5, 0.5, 0.0], [0.0, -0.5, 0.0]] {
        for v in p {
            bin.extend_from_slice(&v.to_le_bytes());
        }
    }
    // UV(mesh1):3×8
    let uv_at = bin.len();
    for uv in [[0.0f32, 0.0], [1.0, 0.0], [0.5, 1.0]] {
        bin.extend_from_slice(&uv[0].to_le_bytes());
        bin.extend_from_slice(&uv[1].to_le_bytes());
    }
    // 索引 mesh0 u16×3
    let idx0_at = bin.len();
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    // 索引 mesh1 u16×3
    let idx1_at = bin.len();
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    // 纹理假 PNG(15B→4 对齐垫到 16)
    let tex = b"\x89PNG-golden-bytes!";
    let tex_at = bin.len();
    bin.extend_from_slice(tex);
    if !bin.len().is_multiple_of(4) {
        bin.extend(std::iter::repeat_n(0, 4 - bin.len() % 4));
    }

    let doc = json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1]}],
        "nodes": [
            {"translation": [1.0, 0.0, 0.0], "scale": [2.0, 2.0, 2.0], "children": [2]},
            {"mesh": 1},
            {"rotation": [0.0, 0.0, std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2], "mesh": 0},
        ],
        "meshes": [
            {"primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1},
                "indices": 2, "material": 0
            }]},
            {"primitives": [{
                "attributes": {"POSITION": 3, "TEXCOORD_0": 4},
                "indices": 5, "material": 1
            }]}
        ],
        "materials": [
            {"pbrMetallicRoughness": {"baseColorFactor": [0.8, 0.2, 0.1, 1.0],
                                      "metallicFactor": 0.25, "roughnessFactor": 0.75}},
            {"pbrMetallicRoughness": {"baseColorTexture": {"index": 0},
                                      "metallicRoughnessTexture": {"index": 0}}}
        ],
        "textures": [{"source": 0}],
        "images": [{"mimeType": "image/png", "bufferView": 6}],
        "accessors": [
            {"componentType": 5126, "count": 3, "bufferView": 0, "byteOffset": 4, "type": "VEC3"},
            {"componentType": 5126, "count": 3, "bufferView": 1, "byteOffset": 0, "type": "VEC3"},
            {"componentType": 5123, "count": 3, "bufferView": 2, "byteOffset": 0, "type": "SCALAR"},
            {"componentType": 5126, "count": 3, "bufferView": 3, "byteOffset": 0, "type": "VEC3"},
            {"componentType": 5126, "count": 3, "bufferView": 4, "byteOffset": 0, "type": "VEC2"},
            {"componentType": 5123, "count": 3, "bufferView": 5, "byteOffset": 0, "type": "SCALAR"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteLength": pos_view_len, "byteStride": 16},
            {"buffer": 0, "byteOffset": norm_at, "byteLength": 36},
            {"buffer": 0, "byteOffset": idx0_at, "byteLength": 6},
            {"buffer": 0, "byteOffset": pos1_at, "byteLength": 36},
            {"buffer": 0, "byteOffset": uv_at, "byteLength": 24},
            {"buffer": 0, "byteOffset": idx1_at, "byteLength": 6},
            {"buffer": 0, "byteOffset": tex_at, "byteLength": tex.len()}
        ],
        "buffers": [{"byteLength": bin.len()}]
    });
    (doc, bin)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/golden/gltf_golden.json")
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

// serde_json::Value 别名(测试体顶部 use 与下方 json! 宏共用)
use serde_json::Value;

#[test]
fn cross_language_gltf_golden_matches_js_mirror_contract() {
    let (doc, bin) = fixture_scene();
    let scene = decode_glb_scene(&build_glb(&doc, &bin)).expect("fixture GLB must decode");
    assert_eq!(scene.primitives.len(), 2);

    let fixture = serde_json::json!({
        "meta": {
            "name": "gltf_golden",
            "generated_by": "oriera-asset tests/cross_language_gltf.rs (ORIERA_GLTF_BLESS=1)",
            "consumed_by": [
                "crates/oriera-asset/tests/cross_language_gltf.rs",
                "examples/05-gltf-model/engine/gltf.test.mjs"
            ],
            "conventions": {
                "walk": "scene graph DFS, world = parent_world · node_local(T·R·S)",
                "normals": "inverse-transpose of world upper-3×3, renormalized",
                "note": "JS mirror computes in f64; indices/material stall exact; transformed positions/normals tolerate f32-vs-f64 drift"
            },
            "tolerances": {"transformed_abs": 1e-4}
        },
        "glb_base64": base64_encode(&build_glb(&doc, &bin)),
        "expect": {
            "primitive_count": scene.primitives.len(),
            "primitives": scene.primitives.iter().map(|p| serde_json::json!({
                "indices": p.indices,
                "base_color": p.base_color,
                "metallic": p.metallic,
                "roughness": p.roughness,
                "positions": p.positions,
                "normals": p.normals,
                "uvs": p.uvs,
                "texture_mime": p.base_color_texture.as_ref().map(|t| t.mime.clone()),
                "texture_len": p.base_color_texture.as_ref().map(|t| t.bytes.len()),
                "mr_texture_len": p.metallic_roughness_texture.as_ref().map(|t| t.bytes.len()),
            })).collect::<Vec<_>>()
        }
    });

    let path = golden_path();
    if std::env::var_os("ORIERA_GLTF_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&fixture).unwrap() + "\n",
        )
        .unwrap();
    }
    let on_disk = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {} ({e}); bless with ORIERA_GLTF_BLESS=1",
            path.display()
        )
    });
    let stored: serde_json::Value = serde_json::from_slice(&on_disk).unwrap();
    assert_eq!(
        stored["expect"]["primitive_count"],
        fixture["expect"]["primitive_count"]
    );
    let stored_ps = stored["expect"]["primitives"].as_array().unwrap();
    let fresh_ps = fixture["expect"]["primitives"].as_array().unwrap();
    for (d, f) in stored_ps.iter().zip(fresh_ps) {
        assert_eq!(d["indices"], f["indices"], "indices bit-exact");
        assert_eq!(d["base_color"], f["base_color"], "base_color exact");
        assert_eq!(d["metallic"], f["metallic"]);
        assert_eq!(d["roughness"], f["roughness"]);
        assert_eq!(d["texture_mime"], f["texture_mime"]);
        assert_eq!(d["texture_len"], f["texture_len"]);
        // 容差域:变换后顶点/法线/UV
        near_rows(&d["positions"], &f["positions"], 1e-4, "positions");
        if f["normals"].is_null() {
            assert!(d["normals"].is_null());
        } else {
            near_rows(&d["normals"], &f["normals"], 1e-4, "normals");
        }
        if f["uvs"].is_null() {
            assert!(d["uvs"].is_null());
        } else {
            near_rows(&d["uvs"], &f["uvs"], 1e-6, "uvs");
        }
    }
}

fn near_rows(a: &serde_json::Value, b: &serde_json::Value, tol: f64, what: &str) {
    let (a, b): (Vec<Vec<f64>>, Vec<Vec<f64>>) = (
        serde_json::from_value(a.clone()).unwrap(),
        serde_json::from_value(b.clone()).unwrap(),
    );
    for (ra, rb) in a.iter().zip(&b) {
        for (x, y) in ra.iter().zip(rb) {
            assert!((x - y).abs() <= tol, "{what}: {x} vs {y} beyond {tol}");
        }
    }
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
