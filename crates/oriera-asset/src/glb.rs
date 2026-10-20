//! GLB 最小网格解码器 —— L8 生成资产的第一个真实消费者(ADR 0003 v0.2 深校验的起点)。
//!
//! 支持面(诚实清单,与 `validate_asset` 同一哲学:解码器不存在的深度绝不假装):
//! - 只取第一个 mesh 的第一个 primitive;
//! - 顶点属性仅 `POSITION`(VEC3 / f32);
//! - 索引仅 SCALAR 的 u32 / u16(无索引则按顶点序绘制);
//! - 拓扑必须为 triangles(mode 缺省视为 4);
//! - 交错布局(byteStride)、材质/纹理/法线/UV/蒙皮/变形目标一律显式拒绝或忽略并在
//!   文档声明——错误必须可判定,绝不猜。
//!
//! 所有 accessor 读取前先做边界校验(声明字节必须完整落在 buffer 内),
//! 与 `validate_asset` 的尺寸钳制构成两级防线。

use serde_json::Value;
use std::fmt;

/// 解码出的最小网格:位置 + 索引 + 包围盒(由数据实算,不信 accessor 声明)。
#[derive(Debug, Clone, PartialEq)]
pub struct GlbMesh {
    pub positions: Vec<[f32; 3]>,
    /// 为空 = 非索引网格,按顶点序绘制。
    pub indices: Vec<u32>,
    /// 实算包围盒(渲染端归一化用)。
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlbError {
    /// 魔数 / 版本 / 总长不符。
    NotGlb,
    /// 声明长度超出实际字节。
    Truncated(&'static str),
    BadJson(String),
    /// 文档里没有 mesh / primitive / POSITION。
    MissingMesh,
    /// 明确不支持的特性(交错布局、非三角拓扑、byteStride>组件宽…)。
    Unsupported(&'static str),
    /// accessor 的 componentType 不在支持表。
    BadComponent(u32),
    /// 声明的读取范围越过 buffer 实际字节。
    OutOfBounds {
        view: usize,
        need: usize,
        have: usize,
    },
}

impl fmt::Display for GlbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GlbError::NotGlb => write!(f, "not a GLB container"),
            GlbError::Truncated(what) => write!(f, "truncated GLB: {what}"),
            GlbError::BadJson(e) => write!(f, "JSON chunk invalid: {e}"),
            GlbError::MissingMesh => write!(f, "no mesh primitive with POSITION"),
            GlbError::Unsupported(what) => write!(f, "unsupported GLB feature: {what}"),
            GlbError::BadComponent(t) => write!(f, "unsupported accessor componentType {t}"),
            GlbError::OutOfBounds { view, need, have } => {
                write!(
                    f,
                    "accessor {view} needs {need} bytes but buffer view has {have}"
                )
            }
        }
    }
}

impl std::error::Error for GlbError {}

const GLB_MAGIC: &[u8; 4] = b"glTF";
const COMPONENT_FLOAT: u32 = 5126;
const COMPONENT_UINT32: u32 = 5125;
const COMPONENT_UINT16: u32 = 5123;

/// 解码 GLB 的第一个网格。BIN chunk 不存在或无 mesh 时返回显式错误。
pub fn decode_glb_mesh(bytes: &[u8]) -> Result<GlbMesh, GlbError> {
    if bytes.len() < 12 || &bytes[0..4] != GLB_MAGIC {
        return Err(GlbError::NotGlb);
    }
    let u32le = |off: usize| -> usize {
        u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]) as usize
    };
    if u32le(4) != 2 {
        return Err(GlbError::NotGlb);
    }
    if u32le(8) != bytes.len() {
        return Err(GlbError::NotGlb);
    }

    // chunk 0:JSON
    if bytes.len() < 20 || &bytes[16..20] != b"JSON" {
        return Err(GlbError::NotGlb);
    }
    let json_len = u32le(12);
    let json_end = 20 + json_len;
    if json_end > bytes.len() || json_len == 0 {
        return Err(GlbError::Truncated("JSON chunk"));
    }
    let doc: Value = serde_json::from_slice(&bytes[20..json_end])
        .map_err(|e| GlbError::BadJson(e.to_string()))?;

    // chunk 1(若存在):BIN —— 网格数据的唯一来源(buffer 0)
    // chunk 头布局:{ length: u32, type: [u8;4] }
    let bin: &[u8] = if json_end + 8 <= bytes.len() {
        let bin_len = u32le(json_end);
        let bin_start = json_end + 8;
        if &bytes[json_end + 4..json_end + 8] != b"BIN\0" || bin_start + bin_len > bytes.len() {
            return Err(GlbError::Truncated("BIN chunk"));
        }
        &bytes[bin_start..bin_start + bin_len]
    } else {
        &[]
    };

    let primitive = doc
        .pointer("/meshes/0/primitives/0")
        .ok_or(GlbError::MissingMesh)?;
    if let Some(mode) = primitive.get("mode").and_then(Value::as_u64) {
        if mode != 4 {
            return Err(GlbError::Unsupported("non-triangle primitive mode"));
        }
    }
    let position_acc_idx = primitive
        .pointer("/attributes/POSITION")
        .and_then(Value::as_u64)
        .ok_or(GlbError::MissingMesh)? as usize;

    let accessors = doc
        .get("accessors")
        .and_then(Value::as_array)
        .ok_or(GlbError::MissingMesh)?;
    let buffer_views = doc.get("bufferViews").and_then(Value::as_array);

    let positions = read_positions(accessors, buffer_views, position_acc_idx, bin)?;
    if positions.is_empty() {
        return Err(GlbError::MissingMesh);
    }

    let indices = match primitive.get("indices").and_then(Value::as_u64) {
        Some(idx) => read_indices(accessors, buffer_views, idx as usize, bin)?,
        None => Vec::new(),
    };

    // 包围盒由数据实算;accessor 声明的 min/max 只作旁证,不作事实。
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in &positions {
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    Ok(GlbMesh {
        positions,
        indices,
        min,
        max,
    })
}

/// 定位一个 accessor 声明的读取区间并做全部合法性与边界校验。
/// 返回 buffer 内绝对区间。
fn locate(
    accessors: &[Value],
    buffer_views: Option<&Vec<Value>>,
    idx: usize,
    component: u32,
    comp_bytes: usize,
    vec_arity: usize,
    bin: &[u8],
) -> Result<std::ops::Range<usize>, GlbError> {
    let acc = accessors.get(idx).ok_or(GlbError::MissingMesh)?;
    let actual = acc
        .get("componentType")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    if actual != component {
        return Err(GlbError::BadComponent(actual));
    }
    let count = acc.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
    if count == 0 {
        return Err(GlbError::MissingMesh);
    }
    let view_idx = acc.get("bufferView").and_then(Value::as_u64).unwrap_or(0) as usize;
    let views = buffer_views.ok_or(GlbError::MissingMesh)?;
    let view = views.get(view_idx).ok_or(GlbError::OutOfBounds {
        view: view_idx,
        need: 0,
        have: views.len(),
    })?;
    // 交错布局:stride 超过本属性自然宽度即拒绝(不做部分读取的猜测)。
    let natural = comp_bytes * vec_arity;
    if let Some(stride) = view.get("byteStride").and_then(Value::as_u64) {
        if stride as usize > natural {
            return Err(GlbError::Unsupported(
                "interleaved buffer view (byteStride)",
            ));
        }
    }
    let offset = view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let view_len = view.get("byteLength").and_then(Value::as_u64).unwrap_or(0) as usize;
    let need = count * natural;
    if offset + view_len > bin.len() || need > view_len {
        return Err(GlbError::OutOfBounds {
            view: view_idx,
            need,
            have: view_len.min(bin.len().saturating_sub(offset)),
        });
    }
    Ok(offset..offset + need)
}

fn read_positions(
    accessors: &[Value],
    views: Option<&Vec<Value>>,
    idx: usize,
    bin: &[u8],
) -> Result<Vec<[f32; 3]>, GlbError> {
    let range = locate(accessors, views, idx, COMPONENT_FLOAT, 4, 3, bin)?;
    let raw = &bin[range];
    Ok(raw
        .chunks_exact(12)
        .map(|c| {
            [
                f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                f32::from_le_bytes([c[8], c[9], c[10], c[11]]),
            ]
        })
        .collect())
}

fn read_indices(
    accessors: &[Value],
    views: Option<&Vec<Value>>,
    idx: usize,
    bin: &[u8],
) -> Result<Vec<u32>, GlbError> {
    let acc = accessors.get(idx).ok_or(GlbError::MissingMesh)?;
    let component = acc
        .get("componentType")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    match component {
        COMPONENT_UINT32 => {
            let range = locate(accessors, views, idx, COMPONENT_UINT32, 4, 1, bin)?;
            Ok(bin[range]
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect())
        }
        COMPONENT_UINT16 => {
            let range = locate(accessors, views, idx, COMPONENT_UINT16, 2, 1, bin)?;
            Ok(bin[range]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]) as u32)
                .collect())
        }
        other => Err(GlbError::BadComponent(other)),
    }
}

// ---------------------------------------------------------------------------
// 测试:容器由测试自建(不依赖外部 fixture 文件),覆盖正常/截断/越界/交错/非三角。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn u32le(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// 构造合法 GLB:JSON chunk(4 字节对齐补空格)+ BIN chunk。
    fn build_glb(json: &Value, bin: &[u8]) -> Vec<u8> {
        let json_raw = serde_json::to_vec(json).unwrap();
        let json_pad = json_raw.len().div_ceil(4) * 4;
        let bin_pad = bin.len().div_ceil(4) * 4;
        let total = 12 + 8 + json_pad + 8 + bin_pad;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(GLB_MAGIC);
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

    /// 一个标准三角网格文档:3 顶点 + 3 索引,bufferView 严格衔接。
    fn triangle_doc(pos_len: usize, idx_len: usize) -> Value {
        serde_json::json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": []}],
            "meshes": [{"primitives": [{
                "attributes": {"POSITION": 0},
                "indices": 1,
                "mode": 4
            }]}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5125, "count": 3, "type": "SCALAR"}
            ],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": pos_len, "target": 34962},
                {"buffer": 0, "byteOffset": pos_len, "byteLength": idx_len, "target": 34963}
            ],
            "buffers": [{"byteLength": pos_len + idx_len}]
        })
    }

    fn triangle_bin() -> Vec<u8> {
        let mut bin = Vec::new();
        for p in [[-1.0f32, -0.5, 0.0], [1.0, -0.5, 0.0], [0.0, 0.8, 0.0]] {
            for c in p {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        for i in [0u32, 1, 2] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        bin
    }

    #[test]
    fn decodes_indexed_triangle() {
        let bin = triangle_bin();
        let glb = build_glb(&triangle_doc(36, 12), &bin);
        let mesh = decode_glb_mesh(&glb).expect("triangle decodes");
        assert_eq!(mesh.positions.len(), 3);
        assert_eq!(mesh.positions[2], [0.0, 0.8, 0.0]);
        assert_eq!(mesh.indices, vec![0, 1, 2]);
        // 包围盒实算,不取声明
        assert_eq!(mesh.min, [-1.0, -0.5, 0.0]);
        assert_eq!(mesh.max, [1.0, 0.8, 0.0]);
    }

    #[test]
    fn non_indexed_mesh_draws_in_vertex_order() {
        let bin = triangle_bin()[..36].to_vec();
        let mut doc = triangle_doc(36, 0);
        doc["meshes"][0]["primitives"][0]
            .as_object_mut()
            .unwrap()
            .remove("indices");
        let glb = build_glb(&doc, &bin);
        let mesh = decode_glb_mesh(&glb).expect("non-indexed decodes");
        assert_eq!(mesh.positions.len(), 3);
        assert!(mesh.indices.is_empty());
    }

    #[test]
    fn u16_indices_decode() {
        let mut bin = triangle_bin()[..36].to_vec();
        for i in [0u16, 1, 2] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let doc = serde_json::json!({
            "asset": {"version": "2.0"},
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}
            ],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 6}
            ],
            "buffers": [{"byteLength": 42}]
        });
        let glb = build_glb(&doc, &bin);
        let mesh = decode_glb_mesh(&glb).expect("u16 decodes");
        assert_eq!(mesh.indices, vec![0, 1, 2]);
    }

    #[test]
    fn accessor_overflow_is_rejected() {
        // accessor 声明 3 顶点(36B),但 bufferView 只给 20B —— 必须显式拒绝
        let bin = triangle_bin();
        let doc = triangle_doc(20, 12);
        let glb = build_glb(&doc, &bin);
        assert!(matches!(
            decode_glb_mesh(&glb),
            Err(GlbError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn interleaved_stride_is_rejected() {
        let mut doc = triangle_doc(36, 12);
        doc["bufferViews"][0]["byteStride"] = serde_json::json!(24);
        let glb = build_glb(&doc, &triangle_bin());
        assert!(matches!(
            decode_glb_mesh(&glb),
            Err(GlbError::Unsupported(_))
        ));
    }

    #[test]
    fn wrong_magic_is_not_glb() {
        assert_eq!(decode_glb_mesh(b"not a glb at all"), Err(GlbError::NotGlb));
    }

    #[test]
    fn empty_scene_has_no_mesh() {
        // 与 genai::tiny_glb 同形的空场景:容器合法,但没有 mesh 可解码
        let doc =
            serde_json::json!({"asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": []}]});
        let glb = build_glb(&doc, b"");
        assert_eq!(decode_glb_mesh(&glb), Err(GlbError::MissingMesh));
    }

    #[test]
    fn non_triangle_mode_is_rejected() {
        let bin = triangle_bin();
        let mut doc = triangle_doc(36, 12);
        doc["meshes"][0]["primitives"][0]["mode"] = serde_json::json!(1); // LINES
        let glb = build_glb(&doc, &bin);
        assert!(matches!(
            decode_glb_mesh(&glb),
            Err(GlbError::Unsupported(_))
        ));
    }
}
