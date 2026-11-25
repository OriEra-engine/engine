//! L4 GLB 解码器**属性测试 + 确定性 fuzz**。
//!
//! 与 `glb.rs` 内部单测的分工：那处按已知坏样本逐个钉错误分支；本文件
//! 钉的是**对任意输入都成立的不变量**——解码器是引擎面向外部生成资产
//! （Tripo / Rodin / Hyper3D）的第一道字节边界，它必须做到两件事：
//!
//! 1. **绝不 panic**。坏文件只能变成 `Err(GlbError)`，不能变成崩溃或
//!    越界读。这条对 fuzz 语料全域成立。
//! 2. **凡 `Ok`，输出必然自洽**。索引不越顶点界、坐标全有限、包围盒
//!    与坐标实算一致、法线数量与顶点数对齐。下游渲染与物理直接吃这些
//!    不变量，解码期不保就得处处设防。
//!
//! 另加两条工程性质：**同字节双跑逐位一致**（金样与缓存的前提）、
//! **任意前缀截断必拒**（半下载文件不得被当作完整资产接受）。
//!
//! ## 为什么不引 `proptest` / `rand`
//!
//! 语料由内置 xorshift64* 生成，种子写死在源码里：失败可精确复现，
//! 且不给仓库添依赖。这是确定性 fuzz，不是随机 fuzz —— CI 上每次跑
//! 同一批语料，红了就是真回归，不会是"这次运气不好"。

use oriera_asset::glb::{decode_glb_mesh, GlbError, GlbMesh};
use serde_json::{json, Value};

// ── 确定性语料源 ───────────────────────────────────────────────────

/// xorshift64*：种子固定，跨平台逐位一致（纯整数运算，无浮点）。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // 0 是 xorshift 的不动点，避开。
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    fn byte(&mut self) -> u8 {
        (self.next_u64() >> 33) as u8
    }
}

// ── 合法夹具（与 glb.rs 单测的 build_glb 同构） ─────────────────────

fn u32le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// 按 glTF 2.0 规范逐字节封装：12 字节头 + JSON chunk（补空格对齐）
/// + BIN chunk（补零对齐）。解码器只认字节，不认测试 helper。
fn build_glb(doc: &Value, bin: &[u8]) -> Vec<u8> {
    let json_raw = serde_json::to_vec(doc).expect("fixture JSON serializes");
    let json_pad = json_raw.len().div_ceil(4) * 4;
    let bin_pad = bin.len().div_ceil(4) * 4;
    let total = 12 + 8 + json_pad + 8 + bin_pad;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&u32le(2));
    out.extend_from_slice(&u32le(total as u32));
    out.extend_from_slice(&u32le(json_pad as u32));
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_raw);
    out.extend(std::iter::repeat_n(b' ', json_pad - json_raw.len()));
    out.extend_from_slice(&u32le(bin_pad as u32));
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(bin);
    out.extend(std::iter::repeat_n(0, bin_pad - bin.len()));
    out
}

fn f32s(bin: &mut Vec<u8>, v: &[f32]) {
    for c in v {
        bin.extend_from_slice(&c.to_le_bytes());
    }
}

/// 索引三角网格：`positions` 给定坐标，法线 +Z，索引为三角带。
fn mesh_glb(positions: &[[f32; 3]]) -> Vec<u8> {
    let n = positions.len();
    let triangles = n.saturating_sub(2);
    let indices: Vec<u32> = (0..triangles)
        .flat_map(|t| {
            let b = t as u32;
            [b, b + 1, b + 2]
        })
        .collect();

    let pos_bytes = n * 12;
    let nrm_bytes = n * 12;
    let idx_bytes = indices.len() * 4;

    let doc = json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [{"mesh": 0}]}],
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": 0, "NORMAL": 1},
            "indices": 2
        }]}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": n, "type": "VEC3"},
            {"bufferView": 1, "componentType": 5126, "count": n, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5125, "count": indices.len(), "type": "SCALAR"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": pos_bytes},
            {"buffer": 0, "byteOffset": pos_bytes, "byteLength": nrm_bytes},
            {"buffer": 0, "byteOffset": pos_bytes + nrm_bytes, "byteLength": idx_bytes}
        ],
        "buffers": [{"byteLength": pos_bytes + nrm_bytes + idx_bytes}]
    });

    let mut bin = Vec::new();
    for p in positions {
        f32s(&mut bin, p);
    }
    for _ in 0..n {
        f32s(&mut bin, &[0.0, 0.0, 1.0]);
    }
    for i in &indices {
        bin.extend_from_slice(&u32le(*i));
    }
    build_glb(&doc, &bin)
}

fn unit_triangle() -> Vec<u8> {
    mesh_glb(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]])
}

// ── 不变量核对 ─────────────────────────────────────────────────────

/// 凡 `Ok`，这些必须全部成立。下游渲染 / 物理直接依赖它们。
fn assert_mesh_invariants(m: &GlbMesh) {
    assert!(
        !m.positions.is_empty(),
        "a decoded mesh must carry vertices"
    );

    for (i, p) in m.positions.iter().enumerate() {
        for c in p {
            assert!(
                c.is_finite(),
                "position {i} has a non-finite component: {p:?}"
            );
        }
    }

    for &idx in &m.indices {
        assert!(
            (idx as usize) < m.positions.len(),
            "index {idx} escapes {} vertices",
            m.positions.len()
        );
    }

    if let Some(normals) = &m.normals {
        assert_eq!(
            normals.len(),
            m.positions.len(),
            "normal count must align with vertex count"
        );
        for n in normals {
            for c in n {
                assert!(c.is_finite(), "non-finite normal component: {n:?}");
            }
        }
    }

    // 包围盒必须与坐标实算一致（渲染端归一化直接用它）。
    for axis in 0..3 {
        let lo = m
            .positions
            .iter()
            .map(|p| p[axis])
            .fold(f32::INFINITY, f32::min);
        let hi = m
            .positions
            .iter()
            .map(|p| p[axis])
            .fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(m.min[axis], lo, "min[{axis}] disagrees with positions");
        assert_eq!(m.max[axis], hi, "max[{axis}] disagrees with positions");
        assert!(m.min[axis] <= m.max[axis]);
    }

    if let Some(c) = &m.base_color {
        for v in c {
            assert!(v.is_finite(), "non-finite base color component");
        }
    }
}

// ── 1. 绝不 panic：随机字节全域 ────────────────────────────────────

#[test]
fn arbitrary_bytes_never_panic() {
    let mut rng = Rng::new(0x0A1E_0001);
    for case in 0..2000 {
        let len = rng.below(512);
        let bytes: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        // 只要求"不 panic"；Ok / Err 都可接受，但 Ok 必须自洽。
        if let Ok(m) = decode_glb_mesh(&bytes) {
            assert_mesh_invariants(&m);
        }
        let _ = case;
    }
}

/// 带合法魔数的随机字节：逼解码器走进容器解析之后的分支，
/// 而不是在第一道魔数检查就被挡回。
#[test]
fn gltf_prefixed_random_bytes_never_panic() {
    let mut rng = Rng::new(0x0A1E_0002);
    for _ in 0..2000 {
        let len = 12 + rng.below(512);
        let mut bytes: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        bytes[..4].copy_from_slice(b"glTF");
        bytes[4..8].copy_from_slice(&u32le(2));
        let total = bytes.len() as u32;
        bytes[8..12].copy_from_slice(&u32le(total));
        if let Ok(m) = decode_glb_mesh(&bytes) {
            assert_mesh_invariants(&m);
        }
    }
}

// ── 2. 变异 fuzz：合法文件逐字节扰动 ───────────────────────────────

#[test]
fn single_byte_mutations_never_panic_and_stay_consistent() {
    let base = unit_triangle();
    let mut rng = Rng::new(0x0A1E_0003);
    let mut accepted = 0usize;
    for _ in 0..4000 {
        let mut mutated = base.clone();
        let at = rng.below(mutated.len());
        mutated[at] = rng.byte();
        if let Ok(m) = decode_glb_mesh(&mutated) {
            assert_mesh_invariants(&m);
            accepted += 1;
        }
    }
    // 防空转：若变异后全被拒，上面的不变量断言一次都没跑过，
    // 这条测试就是绿的假象。BIN 区的浮点字节变异应当大量存活。
    assert!(
        accepted > 100,
        "mutation fuzz accepted only {accepted} inputs; the invariant check is effectively dead"
    );
}

/// 每个字节位置都翻一次最高位：覆盖容器头、JSON 文本、BIN 浮点字节
/// 三类区域，比纯随机变异更系统地扫过结构边界。
#[test]
fn bitflip_at_every_offset_never_panics() {
    let base = unit_triangle();
    let mut accepted = 0usize;
    for at in 0..base.len() {
        let mut mutated = base.clone();
        mutated[at] ^= 0x80;
        if let Ok(m) = decode_glb_mesh(&mutated) {
            assert_mesh_invariants(&m);
            accepted += 1;
        }
    }
    assert!(
        accepted > 0,
        "no bitflip survived decoding; the invariant check never ran"
    );
}

// ── 3. 截断必拒 ────────────────────────────────────────────────────

#[test]
fn every_strict_prefix_is_rejected() {
    let base = unit_triangle();
    for cut in 0..base.len() {
        let err = decode_glb_mesh(&base[..cut])
            .expect_err("a truncated GLB must never decode as a complete asset");
        // 半下载文件在容器层就该被挡住，不该走到几何解析。
        assert!(
            matches!(err, GlbError::NotGlb | GlbError::Truncated(_)),
            "cut {cut}: unexpected error kind {err:?}"
        );
    }
    // 完整长度本身必须通过。
    let m = decode_glb_mesh(&base).expect("the full fixture is valid");
    assert_mesh_invariants(&m);
}

#[test]
fn trailing_garbage_is_rejected_by_total_length() {
    let mut base = unit_triangle();
    base.extend_from_slice(&[0xAA; 16]);
    // 头部声明的总长与实际字节不符 → NotGlb，不静默忽略尾巴。
    assert_eq!(decode_glb_mesh(&base), Err(GlbError::NotGlb));
}

// ── 4. 双跑逐位一致 ────────────────────────────────────────────────

#[test]
fn same_bytes_decode_identically_twice() {
    let base = mesh_glb(&[
        [-0.5, -0.25, 0.0],
        [0.5, -0.25, 0.125],
        [0.0, 0.75, -0.5],
        [0.25, 0.25, 0.5],
    ]);
    let a = decode_glb_mesh(&base).expect("valid fixture");
    let b = decode_glb_mesh(&base).expect("valid fixture");
    assert_eq!(a.positions, b.positions);
    assert_eq!(a.normals, b.normals);
    assert_eq!(a.indices, b.indices);
    assert_eq!(a.min, b.min);
    assert_eq!(a.max, b.max);
    assert_eq!(a.base_color, b.base_color);
}

/// 随机合法网格的双跑：坐标由确定性语料生成，覆盖负数、非对称包围盒。
#[test]
fn random_valid_meshes_roundtrip_and_replay() {
    let mut rng = Rng::new(0x0A1E_0004);
    for _ in 0..200 {
        let n = 3 + rng.below(40);
        let positions: Vec<[f32; 3]> = (0..n)
            .map(|_| {
                [
                    (rng.below(2000) as f32 - 1000.0) / 100.0,
                    (rng.below(2000) as f32 - 1000.0) / 100.0,
                    (rng.below(2000) as f32 - 1000.0) / 100.0,
                ]
            })
            .collect();
        let bytes = mesh_glb(&positions);
        let m = decode_glb_mesh(&bytes).expect("generated fixture is valid");
        assert_mesh_invariants(&m);
        assert_eq!(
            m.positions, positions,
            "positions must roundtrip bit-exactly"
        );
        assert_eq!(decode_glb_mesh(&bytes).unwrap().positions, m.positions);
    }
}

// ── 5. 显式拒绝的坏样本（不静默降级） ──────────────────────────────

#[test]
fn non_finite_positions_are_refused() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let bytes = mesh_glb(&[[0.0, 0.0, 0.0], [bad, 0.0, 0.0], [0.0, 1.0, 0.0]]);
        assert_eq!(
            decode_glb_mesh(&bytes),
            Err(GlbError::NonFinitePosition),
            "non-finite coordinate {bad} must be refused at decode time"
        );
    }
}

#[test]
fn index_beyond_vertex_count_is_refused() {
    // 手工造一份索引越界的文档：3 个顶点，索引里出现 3。
    let positions = [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let indices = [0u32, 1, 3];
    let pos_bytes = positions.len() * 12;
    let nrm_bytes = positions.len() * 12;
    let idx_bytes = indices.len() * 4;
    let doc = json!({
        "asset": {"version": "2.0"},
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": 0, "NORMAL": 1}, "indices": 2
        }]}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5125, "count": 3, "type": "SCALAR"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": pos_bytes},
            {"buffer": 0, "byteOffset": pos_bytes, "byteLength": nrm_bytes},
            {"buffer": 0, "byteOffset": pos_bytes + nrm_bytes, "byteLength": idx_bytes}
        ],
        "buffers": [{"byteLength": pos_bytes + nrm_bytes + idx_bytes}]
    });
    let mut bin = Vec::new();
    for p in &positions {
        f32s(&mut bin, p);
    }
    for _ in 0..3 {
        f32s(&mut bin, &[0.0, 0.0, 1.0]);
    }
    for i in &indices {
        bin.extend_from_slice(&u32le(*i));
    }
    assert_eq!(
        decode_glb_mesh(&build_glb(&doc, &bin)),
        Err(GlbError::IndexOutOfRange {
            index: 3,
            vertices: 3
        })
    );
}

#[test]
fn empty_and_tiny_inputs_are_refused() {
    for len in 0..12 {
        let bytes = vec![0u8; len];
        assert!(
            decode_glb_mesh(&bytes).is_err(),
            "len {len} must be refused"
        );
    }
    assert_eq!(decode_glb_mesh(b""), Err(GlbError::NotGlb));
    assert_eq!(decode_glb_mesh(b"glTF"), Err(GlbError::NotGlb));
}

#[test]
fn wrong_magic_and_version_are_refused() {
    let base = unit_triangle();

    let mut bad_magic = base.clone();
    bad_magic[..4].copy_from_slice(b"GLTF");
    assert_eq!(decode_glb_mesh(&bad_magic), Err(GlbError::NotGlb));

    let mut bad_version = base.clone();
    bad_version[4..8].copy_from_slice(&u32le(1));
    assert_eq!(decode_glb_mesh(&bad_version), Err(GlbError::NotGlb));
}

/// 声明长度远大于实际字节：不得按声明去读越界内存。
#[test]
fn oversized_declared_lengths_are_refused_without_overread() {
    let base = unit_triangle();
    for huge in [u32::MAX, u32::MAX - 1, 0x7FFF_FFFF] {
        let mut bytes = base.clone();
        // JSON chunk 长度字段（偏移 12）。
        bytes[12..16].copy_from_slice(&u32le(huge));
        assert!(
            decode_glb_mesh(&bytes).is_err(),
            "declared JSON length {huge} must be refused"
        );
    }
}
