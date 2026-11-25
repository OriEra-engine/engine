//! L4 GLB 解码 + L8 闸口微基准 — `decode_glb_mesh` 与 `validate_asset`。
//!
//! 三组场景，对应 L8 闸 1/闸 2 的真实成本面：
//! - `glb_decode/v{vertices}`：单 mesh 网格顶点数梯度（1k / 10k / 50k / 100k），
//!   索引三角、法线齐全——解码器的纯解析成本；
//! - `l8_gate/valid_v{vertices}`：同一批网格走 `validate_asset` 全路径
//!   （容器 sanity → 解码 → 顶点/三角/半长三道核对），默认限额；
//! - `l8_gate/reject_v{vertices}`：超过 50k 顶点默认闸的拒绝路径——
//!   **必须先解码再拒绝**，所以拒绝不比通过便宜，这条曲线决定闸口
//!   是否需要前置快速判定。
//!
//! 夹具在 bench 内按 glTF 2.0 规范逐字节构造（JSON chunk 四字节对齐 +
//! BIN chunk），与 `oriera-asset` 单测的 `build_glb` 同构——解码器只认
//! 字节，不认测试 helper。
//!
//! 对 L8 阈值回调的意义：默认闸 50k 顶点 / 100k 三角是宽松起步值，
//! 真实 Tripo/Rodin 样本接入后按这条耗时曲线决定是否收紧或放宽。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_asset::glb::decode_glb_mesh;
use oriera_genai::{validate_asset, AssetFormat, GeneratedAsset, ValidationLimits};

/// 单 mesh、单 primitive、三角拓扑：`vertices` 个顶点排成三角带，
/// 索引 `(n-2)` 个三角，法线齐全（+Z）。
fn indexed_grid_glb(vertices: usize) -> Vec<u8> {
    assert!(vertices >= 3, "a triangle strip needs at least 3 vertices");
    let triangles = vertices - 2;
    let indices: Vec<u32> = (0..triangles)
        .flat_map(|t| {
            let base = t as u32;
            [base, base + 1, base + 2]
        })
        .collect();

    // 坐标铺在 [-1, 1]² 平面内，半长 1.0 < 默认闸 2.0 m。
    let side = (vertices as f32).sqrt().ceil();
    let positions: Vec<[f32; 3]> = (0..vertices)
        .map(|i| {
            let x = (i as f32 % side) / side * 2.0 - 1.0;
            let y = (i as f32 / side) / side * 2.0 - 1.0;
            [x, y, 0.0]
        })
        .collect();
    let normals = vec![[0.0f32, 0.0, 1.0]; vertices];

    let pos_bytes = positions.len() * 12;
    let nrm_bytes = normals.len() * 12;
    let idx_bytes = indices.len() * 4;

    let doc = serde_json::json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [{"mesh": 0}]}],
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": 0, "NORMAL": 1},
            "indices": 2
        }]}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": vertices, "type": "VEC3"},
            {"bufferView": 1, "componentType": 5126, "count": vertices, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5125, "count": indices.len(), "type": "SCALAR"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": pos_bytes},
            {"buffer": 0, "byteOffset": pos_bytes, "byteLength": nrm_bytes},
            {"buffer": 0, "byteOffset": pos_bytes + nrm_bytes, "byteLength": idx_bytes}
        ],
        "buffers": [{"byteLength": pos_bytes + nrm_bytes + idx_bytes}]
    });

    // BIN chunk：POSITION → NORMAL → 索引。
    let mut bin = Vec::with_capacity(pos_bytes + nrm_bytes + idx_bytes);
    for p in &positions {
        for c in p {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    for n in &normals {
        for c in n {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    for i in &indices {
        bin.extend_from_slice(&i.to_le_bytes());
    }

    // GLB 容器：12 字节头 + JSON chunk（四字节对齐补空格）+ BIN chunk（补零）。
    let json_raw = serde_json::to_vec(&doc).expect("fixture JSON serializes");
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
    out.extend_from_slice(&bin);
    out.extend(std::iter::repeat_n(0u8, bin_pad - bin.len()));
    out
}

fn asset_of(bytes: &[u8]) -> GeneratedAsset {
    GeneratedAsset {
        format: AssetFormat::Glb,
        payload: bytes.to_vec(),
        provider_id: "bench".into(),
        model: "bench-mesh".into(),
        usage: None,
    }
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("glb_decode");
    for v in [1_000usize, 10_000, 50_000, 100_000] {
        let bytes = indexed_grid_glb(v);
        group.bench_with_input(BenchmarkId::new("v", v), &bytes, |b, bytes| {
            b.iter(|| black_box(decode_glb_mesh(black_box(bytes)).expect("fixture decodes")))
        });
    }
    group.finish();
}

fn bench_gate(c: &mut Criterion) {
    let mut group = c.benchmark_group("l8_gate");
    let limits = ValidationLimits::default();

    // ── 通过路径：默认闸（50k 顶点 / 100k 三角 / 2.0 m）以内 ──
    for v in [1_000usize, 10_000, 50_000] {
        let asset = asset_of(&indexed_grid_glb(v));
        group.bench_with_input(BenchmarkId::new("valid", v), &asset, |b, asset| {
            b.iter(|| black_box(validate_asset(black_box(asset), &limits)).expect("under ceiling"))
        });
    }

    // ── 拒绝路径：超 50k 顶点闸，仍须完整解码后才拒绝 ──
    for v in [60_000usize, 100_000] {
        let asset = asset_of(&indexed_grid_glb(v));
        group.bench_with_input(BenchmarkId::new("reject", v), &asset, |b, asset| {
            b.iter(|| {
                let r = validate_asset(black_box(asset), &limits);
                debug_assert!(r.is_err(), "over the default vertex ceiling");
                black_box(r)
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_decode, bench_gate);
criterion_main!(benches);
