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
    /// 顶点法线(VEC3 / f32);None = 文件未提供,渲染端自行退化。
    pub normals: Option<Vec<[f32; 3]>>,
    /// 材质基础色 RGBA(pbrMetallicRoughness.baseColorFactor);
    /// None = 无材质声明,渲染端用默认着色。
    pub base_color: Option<[f32; 4]>,
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
    /// 索引值越过顶点数(坏文件在解码期拒绝,不留给渲染端炸)。
    IndexOutOfRange {
        index: u32,
        vertices: usize,
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
            GlbError::IndexOutOfRange { index, vertices } => {
                write!(f, "index {index} out of range for {vertices} vertices")
            }
        }
    }
}

impl std::error::Error for GlbError {}

const GLB_MAGIC: &[u8; 4] = b"glTF";
const COMPONENT_FLOAT: u32 = 5126;
const COMPONENT_UINT32: u32 = 5125;
const COMPONENT_UINT16: u32 = 5123;

/// 解析 GLB 容器:魔数/版本/总长、JSON chunk、BIN chunk(若存在)。
/// `bin` 生命周期挂在 `bytes` 上;无 BIN chunk 时为空切片。
fn parse_glb_container(bytes: &[u8]) -> Result<(Value, &[u8]), GlbError> {
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
    Ok((doc, bin))
}

/// 解码 GLB 的第一个网格。BIN chunk 不存在或无 mesh 时返回显式错误。
pub fn decode_glb_mesh(bytes: &[u8]) -> Result<GlbMesh, GlbError> {
    let (doc, bin) = parse_glb_container(bytes)?;

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

    // 法线可选:声明了就必须与 POSITION 等长(坏文件直接拒绝)。
    let normals = match primitive
        .pointer("/attributes/NORMAL")
        .and_then(Value::as_u64)
    {
        Some(idx) => {
            let n = read_positions(accessors, buffer_views, idx as usize, bin)?;
            if n.len() != positions.len() {
                return Err(GlbError::Unsupported("NORMAL count mismatches POSITION"));
            }
            Some(n)
        }
        None => None,
    };

    // 材质基础色:materials[primitive.material].pbrMetallicRoughness
    // .baseColorFactor(线性 RGBA,glTF 2.0 规范)。纹理贴图未解码
    // —— 依赖图像解码器,留给渲染端需要时再议(诚实边界)。
    let base_color = primitive
        .get("material")
        .and_then(Value::as_u64)
        .and_then(|mi| doc.pointer(&format!("/materials/{mi}")))
        .and_then(|m| m.pointer("/pbrMetallicRoughness/baseColorFactor"))
        .and_then(Value::as_array)
        .and_then(|arr| {
            let f: Vec<f32> = arr
                .iter()
                .filter_map(Value::as_f64)
                .map(|x| x as f32)
                .collect();
            (f.len() == 4).then_some([f[0], f[1], f[2], f[3]])
        });

    let indices = match primitive.get("indices").and_then(Value::as_u64) {
        Some(idx) => read_indices(accessors, buffer_views, idx as usize, bin)?,
        None => Vec::new(),
    };
    // 索引必须落在顶点数内:越界索引不该等渲染端 draw 才炸。
    if let Some(&bad) = indices.iter().find(|&&i| i as usize >= positions.len()) {
        return Err(GlbError::IndexOutOfRange {
            index: bad,
            vertices: positions.len(),
        });
    }

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
        normals,
        base_color,
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
    // accessor 自身的 byteOffset(合法 glTF 特性)会改变读取起点——
    // 本解码器不支持,必须显式拒绝而不是静默读错字节。
    if acc.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) != 0 {
        return Err(GlbError::Unsupported("accessor byteOffset"));
    }
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
    // 全部用 checked 算术:这里的数字来自不可信 JSON,溢出/回绕会把
    // 下面的边界校验整个绕过(release 无 overflow-checks)。
    let offset = view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0);
    let view_len = view.get("byteLength").and_then(Value::as_u64).unwrap_or(0);
    let need = match (count as u64).checked_mul(natural as u64) {
        Some(n) => n.min(usize::MAX as u64) as usize,
        None => usize::MAX, // 声明字节总数溢出 = 必然越界,按上界报错
    };
    let off_end = offset.checked_add(view_len).ok_or(GlbError::OutOfBounds {
        view: view_idx,
        need,
        have: 0,
    })?;
    if off_end > bin.len() as u64 || need as u64 > view_len {
        return Err(GlbError::OutOfBounds {
            view: view_idx,
            need,
            have: view_len.min(bin.len() as u64).saturating_sub(offset) as usize,
        });
    }
    Ok(offset as usize..offset as usize + need)
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
        .as_chunks::<12>()
        .0
        .iter()
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
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect())
        }
        COMPONENT_UINT16 => {
            let range = locate(accessors, views, idx, COMPONENT_UINT16, 2, 1, bin)?;
            Ok(bin[range]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]) as u32)
                .collect())
        }
        other => Err(GlbError::BadComponent(other)),
    }
}

// ---------------------------------------------------------------------------
// 场景解码(#152 第二刀):全部 mesh×primitive + 节点图 TRS + 材质摊位。
// 与冻结的 decode_glb_mesh 并存:真实 glTF 文件的 accessor byteOffset 与
// bufferView byteStride 在此**支持**(带完整校验),冻结路径维持原拒绝语义。
// ---------------------------------------------------------------------------

/// GLB 内嵌纹理的原始字节摊位(mime 来自 images[].mimeType)。
/// 解码不在此层——PNG 助手见 [`decode_png_texture`](feature `texture`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlbTextureBytes {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// 场景里的一个 primitive:顶点已应用节点世界变换(世界坐标)。
#[derive(Debug, Clone, PartialEq)]
pub struct GlbPrimitive {
    pub positions: Vec<[f32; 3]>,
    /// 世界坐标法线(经逆转置矩阵变换后归一化)。
    pub normals: Option<Vec<[f32; 3]>>,
    /// TEXCOORD_0(可选;纹理采样用)。
    pub uvs: Option<Vec<[f32; 2]>>,
    pub indices: Vec<u32>,
    /// pbrMetallicRoughness.baseColorFactor(线性 RGBA,默认 [1,1,1,1])。
    pub base_color: Option<[f32; 4]>,
    /// metallicFactor(规范默认 1.0;摊位,着色随 PBR 刀次)。
    pub metallic: f32,
    /// roughnessFactor(规范默认 1.0;同上)。
    pub roughness: f32,
    /// baseColorTexture 原始字节(GLB 内嵌 PNG;无纹理则 None)。
    pub base_color_texture: Option<GlbTextureBytes>,
    /// metallicRoughnessTexture 原始字节(G 通道=粗糙度,B=金属度;无则 None)。
    pub metallic_roughness_texture: Option<GlbTextureBytes>,
    /// 实算包围盒(世界坐标)。
    pub min: [f32; 3],
    pub max: [f32; 3],
}

/// 解码出的完整场景:首场景可达的全部 primitive(世界坐标)。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GlbScene {
    pub primitives: Vec<GlbPrimitive>,
}

/// 场景路径的 accessor 定位:与冻结的 [`locate`] 相比**支持** accessor
/// byteOffset 与 bufferView byteStride(真实 glTF 文件的常态),校验同样
/// 全 checked:字节区间必须完整落在 view 内。
fn locate_scene(
    accessors: &[Value],
    views: &[Value],
    idx: usize,
    component: u32,
    comp_bytes: usize,
    vec_arity: usize,
    bin: &[u8],
) -> Result<StrideRange, GlbError> {
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
    let view = views.get(view_idx).ok_or(GlbError::OutOfBounds {
        view: view_idx,
        need: 0,
        have: views.len(),
    })?;
    let natural = comp_bytes * vec_arity;
    let view_off = view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0);
    let view_len = view.get("byteLength").and_then(Value::as_u64).unwrap_or(0);
    let stride = match view.get("byteStride").and_then(Value::as_u64) {
        Some(s) if (s as usize) > natural => s as usize, // 交错:按步长逐元素
        Some(_) => natural,
        None => natural,
    };
    // 元素 i 起点 = view_off + acc_off + i·stride;总占位 = (count-1)·stride + natural。
    let acc_off = acc.get("byteOffset").and_then(Value::as_u64).unwrap_or(0);
    let tail = (count as u64)
        .checked_sub(1)
        .and_then(|c| c.checked_mul(stride as u64))
        .and_then(|t| t.checked_add(natural as u64))
        .ok_or(GlbError::OutOfBounds {
            view: view_idx,
            need: usize::MAX,
            have: bin.len(),
        })?;
    let start = view_off.checked_add(acc_off).ok_or(GlbError::OutOfBounds {
        view: view_idx,
        need: tail.min(usize::MAX as u64) as usize,
        have: view_len.min(usize::MAX as u64) as usize,
    })?;
    // 相对校验(acc_off + 数据 ≤ view_len)与绝对校验(view_off+view_len ≤ bin)分开:
    // start 含 view_off,不能直接和相对的 view_len 比。
    if acc_off + tail > view_len || view_off + view_len > bin.len() as u64 {
        return Err(GlbError::OutOfBounds {
            view: view_idx,
            need: (acc_off + tail) as usize,
            have: view_len.min(usize::MAX as u64) as usize,
        });
    }
    Ok(StrideRange {
        start: start as usize,
        count,
        stride,
    })
}

/// 带步长的元素区间(场景路径):元素 i 从 `start + i·stride` 起。
#[derive(Clone, Copy)]
struct StrideRange {
    start: usize,
    count: usize,
    stride: usize,
}

impl StrideRange {
    fn offset_of(&self, i: usize) -> usize {
        self.start + i * self.stride
    }
}

/// 按元素步长读出 f32 向量(arity 2/3)。
fn read_f32_vecs(
    accessors: &[Value],
    views: &[Value],
    idx: usize,
    arity: usize,
    bin: &[u8],
) -> Result<Vec<[f32; 3]>, GlbError> {
    // arity=2 时第三分量恒 0(调用方只取前 arity 个)
    let range = locate_scene(accessors, views, idx, COMPONENT_FLOAT, 4, arity, bin)?;
    let mut out = Vec::with_capacity(range.count);
    for i in 0..range.count {
        let at = range.offset_of(i);
        let mut v = [0.0f32; 3];
        for (k, slot) in v.iter_mut().enumerate().take(arity) {
            let b = at + k * 4;
            *slot = f32::from_le_bytes([bin[b], bin[b + 1], bin[b + 2], bin[b + 3]]);
        }
        out.push(v);
    }
    Ok(out)
}

fn read_indices_scene(
    accessors: &[Value],
    views: &[Value],
    idx: usize,
    bin: &[u8],
) -> Result<Vec<u32>, GlbError> {
    let acc = accessors.get(idx).ok_or(GlbError::MissingMesh)?;
    let component = acc
        .get("componentType")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let (component, comp_bytes) = match component {
        COMPONENT_UINT32 => (COMPONENT_UINT32, 4usize),
        COMPONENT_UINT16 => (COMPONENT_UINT16, 2usize),
        other => return Err(GlbError::BadComponent(other)),
    };
    let range = locate_scene(accessors, views, idx, component, comp_bytes, 1, bin)?;
    let mut out = Vec::with_capacity(range.count);
    for i in 0..range.count {
        let at = range.offset_of(i);
        out.push(if comp_bytes == 4 {
            u32::from_le_bytes([bin[at], bin[at + 1], bin[at + 2], bin[at + 3]])
        } else {
            u32::from(u16::from_le_bytes([bin[at], bin[at + 1]]))
        });
    }
    Ok(out)
}

/// glTF 四元数(xyzw,**w 在后**——与 PLY 的 w 在前相反)→ 3×3 旋转矩阵。
fn quat_xyzw_to_mat3([x, y, z, w]: [f64; 4]) -> [[f32; 3]; 3] {
    let (x, y, z, w) = (x as f32, y as f32, z as f32, w as f32);
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - w * z),
            2.0 * (x * z + w * y),
        ],
        [
            2.0 * (x * y + w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - w * x),
        ],
        [
            2.0 * (x * z - w * y),
            2.0 * (y * z + w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

type Mat4 = [[f32; 4]; 4];

const IDENTITY4: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn mul4(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0; 4]; 4];
    for r in 0..4 {
        for c in 0..4 {
            out[r][c] = (0..4).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    out
}

/// 节点局部变换:TRS(缺省单位)或 node.matrix(glTF 列主序 16 元组)。
/// p' = T·R·S·p(行主序存储,列向量)。
fn node_local_matrix(node: &Value) -> Result<Mat4, GlbError> {
    if let Some(arr) = node.get("matrix").and_then(Value::as_array) {
        if arr.len() != 16 {
            return Err(GlbError::Unsupported("node matrix must have 16 elements"));
        }
        let v: Vec<f32> = arr
            .iter()
            .map(|x| x.as_f64().unwrap_or(0.0) as f32)
            .collect();
        let mut m = [[0.0; 4]; 4];
        for r in 0..4 {
            for c in 0..4 {
                m[r][c] = v[c * 4 + r]; // 列主序 → 行主序
            }
        }
        return Ok(m);
    }
    let t = node.get("translation").and_then(Value::as_array);
    let r = node.get("rotation").and_then(Value::as_array);
    let s = node.get("scale").and_then(Value::as_array);
    let mut m = IDENTITY4;
    if let Some(s) = s {
        if s.len() != 3 {
            return Err(GlbError::Unsupported("node scale must have 3 elements"));
        }
        for c in 0..3 {
            m[c][c] = s[c].as_f64().unwrap_or(1.0) as f32;
        }
    }
    if let Some(r) = r {
        if r.len() != 4 {
            return Err(GlbError::Unsupported(
                "node rotation must be a 4-element quaternion",
            ));
        }
        let rot = quat_xyzw_to_mat3([
            r[0].as_f64().unwrap_or(0.0),
            r[1].as_f64().unwrap_or(0.0),
            r[2].as_f64().unwrap_or(0.0),
            r[3].as_f64().unwrap_or(1.0),
        ]);
        let scaled = m;
        let mut next = [[0.0; 4]; 4];
        for rr in 0..3 {
            for c in 0..3 {
                next[rr][c] = rot[rr][c] * scaled[c][c];
            }
        }
        next[3][3] = 1.0;
        m = next;
    }
    if let Some(t) = t {
        if t.len() != 3 {
            return Err(GlbError::Unsupported(
                "node translation must have 3 elements",
            ));
        }
        for r in 0..3 {
            m[r][3] = t[r].as_f64().unwrap_or(0.0) as f32;
        }
    }
    Ok(m)
}

/// 世界矩阵的上 3×3 的逆转置(法线变换);奇异(退化缩放)→ None。
fn normal_matrix(world: &Mat4) -> Option<[[f32; 3]; 3]> {
    let m = [
        [world[0][0], world[0][1], world[0][2]],
        [world[1][0], world[1][1], world[1][2]],
        [world[2][0], world[2][1], world[2][2]],
    ];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    // 只拒真正奇异(det 归零):绝对阈值会误伤小尺度模型(如 ToyCar scale=1e-4)
    if !det.is_finite() || det == 0.0 {
        return None;
    }
    let inv = 1.0 / det;
    // 逆矩阵(伴随/行列式),再转置
    let inv_t = [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv,
        ],
    ];
    Some(inv_t) // 逆的转置 = 转置的逆:上面按行写出的即逆转置(核对见单测)
}

/// 解码首场景的全部 primitive(世界坐标)。支持真实文件的 accessor
/// byteOffset 与交错布局;纹理取原始字节摊位(PNG 解码走 feature `texture`
/// 的 [`decode_png_texture`])。显式拒绝面:skins、非三角、材质扩展、
/// JPEG/未知纹理、外部图片 URI、节点环、退化变换。
pub fn decode_glb_scene(bytes: &[u8]) -> Result<GlbScene, GlbError> {
    let (doc, bin) = parse_glb_container(bytes)?;
    let scene_idx = doc.get("scene").and_then(Value::as_u64).unwrap_or(0) as usize;
    let scene = doc
        .pointer(&format!("/scenes/{scene_idx}"))
        .ok_or(GlbError::MissingMesh)?;
    let root_indices: Vec<usize> = scene
        .get("nodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_u64)
                .map(|v| v as usize)
                .collect()
        })
        .unwrap_or_default();
    let nodes = doc.get("nodes").and_then(Value::as_array);
    let nodes = nodes.ok_or(GlbError::MissingMesh)?;

    let accessors = doc.get("accessors").and_then(Value::as_array);
    let views = doc.get("bufferViews").and_then(Value::as_array);
    let meshes = doc.get("meshes").and_then(Value::as_array);
    let materials = doc.get("materials").and_then(Value::as_array);
    let images = doc.get("images").and_then(Value::as_array);
    let textures = doc.get("textures").and_then(Value::as_array);

    let mut scene_out = GlbScene::default();
    let mut visited = std::collections::HashSet::new();
    // (节点下标, 世界矩阵, 深度)——深度上限防炸栈;环由 visited 显式拒绝。
    let mut queue: std::collections::VecDeque<(usize, Mat4, u32)> = root_indices
        .into_iter()
        .map(|i| (i, IDENTITY4, 0))
        .collect();
    while let Some((ni, world, depth)) = queue.pop_front() {
        if depth > 64 {
            return Err(GlbError::Unsupported("node graph deeper than 64"));
        }
        if !visited.insert(ni) {
            return Err(GlbError::Unsupported("node graph contains a cycle"));
        }
        let node = nodes.get(ni).ok_or(GlbError::MissingMesh)?;
        let local = node_local_matrix(node)?;
        let world = mul4(&world, &local);
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for c in children {
                let ci = c
                    .as_u64()
                    .ok_or(GlbError::Unsupported("non-integer child index"))?
                    as usize;
                queue.push_back((ci, world, depth + 1));
            }
        }
        let mesh_idx = match node.get("mesh").and_then(Value::as_u64) {
            Some(m) => m as usize,
            None => continue, // 纯变换节点(挂灯/挂相机位)合法,跳过
        };
        let mesh = meshes
            .and_then(|ms| ms.get(mesh_idx))
            .ok_or(GlbError::MissingMesh)?;
        if let Some(skin) = node.get("skin") {
            let _ = skin;
            return Err(GlbError::Unsupported("skins (mesh deform) not supported"));
        }
        for primitive in mesh
            .get("primitives")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            if let Some(mode) = primitive.get("mode").and_then(Value::as_u64) {
                if mode != 4 {
                    return Err(GlbError::Unsupported("non-triangle primitive mode"));
                }
            }
            if primitive
                .get("extensions")
                .and_then(|e| e.get("KHR_draco_mesh_compression"))
                .is_some()
            {
                return Err(GlbError::Unsupported(
                    "KHR_draco_mesh_compression (Draco 压缩网格)",
                ));
            }
            // 其余 primitive 级扩展(unlit 等)不影响消费通道,忽略。
            let acc_idx = |name: &str| {
                primitive
                    .pointer(&format!("/attributes/{name}"))
                    .and_then(Value::as_u64)
                    .map(|v| v as usize)
            };
            let pos_idx = acc_idx("POSITION").ok_or(GlbError::MissingMesh)?;
            let (accessors, views) = match (accessors, views) {
                (Some(a), Some(v)) => (a, v),
                _ => return Err(GlbError::MissingMesh),
            };
            let mut positions = read_f32_vecs(accessors, views, pos_idx, 3, bin)?;
            let mut normals = match acc_idx("NORMAL") {
                Some(idx) => {
                    let n = read_f32_vecs(accessors, views, idx, 3, bin)?;
                    if n.len() != positions.len() {
                        return Err(GlbError::Unsupported("NORMAL count mismatches POSITION"));
                    }
                    Some(n)
                }
                None => None,
            };
            let uvs = match acc_idx("TEXCOORD_0") {
                Some(idx) => {
                    // vec2:read_f32_vecs 返回第三分量 0,压成二元组
                    let raw = read_f32_vecs(accessors, views, idx, 2, bin)?;
                    Some(raw.iter().map(|v| [v[0], v[1]]).collect::<Vec<_>>())
                }
                None => None,
            };
            let indices = match primitive.get("indices").and_then(Value::as_u64) {
                Some(idx) => read_indices_scene(accessors, views, idx as usize, bin)?,
                None => Vec::new(),
            };
            if let Some(&bad) = indices.iter().find(|&&i| i as usize >= positions.len()) {
                return Err(GlbError::IndexOutOfRange {
                    index: bad,
                    vertices: positions.len(),
                });
            }
            // 材质摊位(因子默认值来自 glTF 2.0 规范)
            let mut base_color = None;
            let mut metallic = 1.0;
            let mut roughness = 1.0;
            let mut base_color_texture = None;
            let mut mr_texture_bytes = None;
            if let Some(mi) = primitive.get("material").and_then(Value::as_u64) {
                let mat =
                    materials
                        .and_then(|ms| ms.get(mi as usize))
                        .ok_or(GlbError::Unsupported(
                            "primitive references missing material",
                        ))?;
                // 扩展策略:只有**替换**baseColor 数据模型的扩展必须拒
                // (specularGlossiness 整个换掉 PBR);clearcoat/sheen/transmission
                // 等表面附加项不影响本解码器消费的通道,忽略并继续。
                if let Some(exts) = mat.get("extensions") {
                    if exts.get("KHR_materials_pbrSpecularGlossiness").is_some() {
                        return Err(GlbError::Unsupported(
                            "KHR_materials_pbrSpecularGlossiness replaces the baseColor model",
                        ));
                    }
                }
                if let Some(pbr) = mat.get("pbrMetallicRoughness") {
                    if let Some(arr) = pbr.get("baseColorFactor").and_then(Value::as_array) {
                        let f: Vec<f32> = arr
                            .iter()
                            .filter_map(Value::as_f64)
                            .map(|x| x as f32)
                            .collect();
                        if f.len() == 4 {
                            base_color = Some([f[0], f[1], f[2], f[3]]);
                        }
                    }
                    metallic = pbr
                        .get("metallicFactor")
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0) as f32;
                    roughness = pbr
                        .get("roughnessFactor")
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0) as f32;
                    if let Some(tex) = pbr.get("baseColorTexture") {
                        let tex_idx =
                            tex.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let source = textures
                            .and_then(|ts| ts.get(tex_idx))
                            .and_then(|t| t.get("source"))
                            .and_then(Value::as_u64)
                            .ok_or(GlbError::Unsupported(
                                "baseColorTexture references missing texture/source",
                            ))? as usize;
                        let image = images
                            .and_then(|is| is.get(source))
                            .ok_or(GlbError::Unsupported("texture references missing image"))?;
                        let mime = image
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        if mime != "image/png" {
                            return Err(GlbError::Unsupported(
                                "non-PNG baseColorTexture (JPEG 等)",
                            ));
                        }
                        if image.get("uri").is_some() {
                            return Err(GlbError::Unsupported(
                                "external image URI (GLB 内嵌 image 才可)",
                            ));
                        }
                        let view_idx =
                            image.get("bufferView").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let view = views.get(view_idx).ok_or(GlbError::Unsupported(
                            "texture references missing bufferView",
                        ))?;
                        let voff =
                            view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let vlen =
                            view.get("byteLength").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let end = voff.checked_add(vlen).ok_or(GlbError::OutOfBounds {
                            view: view_idx,
                            need: usize::MAX,
                            have: bin.len(),
                        })?;
                        if end > bin.len() {
                            return Err(GlbError::OutOfBounds {
                                view: view_idx,
                                need: end,
                                have: bin.len(),
                            });
                        }
                        base_color_texture = Some(GlbTextureBytes {
                            mime,
                            bytes: bin[voff..end].to_vec(),
                        });
                    }
                    if let Some(tex) = pbr.get("metallicRoughnessTexture") {
                        let tex_idx =
                            tex.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let source = textures
                            .and_then(|ts| ts.get(tex_idx))
                            .and_then(|t| t.get("source"))
                            .and_then(Value::as_u64)
                            .ok_or(GlbError::Unsupported(
                                "metallicRoughnessTexture references missing texture/source",
                            ))? as usize;
                        let image = images
                            .and_then(|is| is.get(source))
                            .ok_or(GlbError::Unsupported("MR texture references missing image"))?;
                        let mime = image
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        if mime != "image/png" {
                            return Err(GlbError::Unsupported("non-PNG metallicRoughnessTexture"));
                        }
                        if image.get("uri").is_some() {
                            return Err(GlbError::Unsupported(
                                "external image URI (GLB 内嵌 image 才可)",
                            ));
                        }
                        let view_idx =
                            image.get("bufferView").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let view = views.get(view_idx).ok_or(GlbError::Unsupported(
                            "MR texture references missing bufferView",
                        ))?;
                        let voff =
                            view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let vlen =
                            view.get("byteLength").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let end = voff.checked_add(vlen).ok_or(GlbError::OutOfBounds {
                            view: view_idx,
                            need: usize::MAX,
                            have: bin.len(),
                        })?;
                        if end > bin.len() {
                            return Err(GlbError::OutOfBounds {
                                view: view_idx,
                                need: end,
                                have: bin.len(),
                            });
                        }
                        mr_texture_bytes = Some(GlbTextureBytes {
                            mime,
                            bytes: bin[voff..end].to_vec(),
                        });
                    }
                }
            }
            // 世界变换应用到顶点与法线(法线走逆转置,再归一化)
            let nmat = normals.as_ref().and_then(|_| normal_matrix(&world));
            if normals.is_some() && nmat.is_none() {
                return Err(GlbError::Unsupported(
                    "degenerate node transform (zero determinant)",
                ));
            }
            const IDENTITY3: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
            let nmat = nmat.unwrap_or(IDENTITY3);
            for (i, p) in positions.iter_mut().enumerate() {
                let wp = [
                    world[0][0] * p[0] + world[0][1] * p[1] + world[0][2] * p[2] + world[0][3],
                    world[1][0] * p[0] + world[1][1] * p[1] + world[1][2] * p[2] + world[1][3],
                    world[2][0] * p[0] + world[2][1] * p[1] + world[2][2] * p[2] + world[2][3],
                ];
                *p = wp;
                if let Some(n) = normals.as_mut() {
                    let raw = n[i];
                    let wn = [
                        nmat[0][0] * raw[0] + nmat[0][1] * raw[1] + nmat[0][2] * raw[2],
                        nmat[1][0] * raw[0] + nmat[1][1] * raw[1] + nmat[1][2] * raw[2],
                        nmat[2][0] * raw[0] + nmat[2][1] * raw[1] + nmat[2][2] * raw[2],
                    ];
                    let len = (wn[0] * wn[0] + wn[1] * wn[1] + wn[2] * wn[2]).sqrt();
                    n[i] = if len > f32::EPSILON {
                        [wn[0] / len, wn[1] / len, wn[2] / len]
                    } else {
                        [0.0, 0.0, 1.0]
                    };
                }
            }
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            for p in &positions {
                for i in 0..3 {
                    min[i] = min[i].min(p[i]);
                    max[i] = max[i].max(p[i]);
                }
            }
            scene_out.primitives.push(GlbPrimitive {
                positions,
                normals,
                uvs,
                indices,
                base_color,
                metallic,
                roughness,
                base_color_texture,
                metallic_roughness_texture: mr_texture_bytes,
                min,
                max,
            });
        }
    }
    Ok(scene_out)
}

/// PNG 纹理解码(feature `texture`,png crate 纯 Rust;JPEG 等在解码层之上
/// 已被 mime 门挡住)。输出统一为 RGBA8。
#[cfg(feature = "texture")]
pub fn decode_png_texture(t: &GlbTextureBytes) -> Result<RgbaTexture, GlbError> {
    if t.mime != "image/png" {
        return Err(GlbError::Unsupported("non-PNG texture"));
    }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(&t.bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .map_err(|_| GlbError::Unsupported("PNG decode failed"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|_| GlbError::Unsupported("PNG frame decode failed"))?;
    Ok(RgbaTexture {
        width: info.width,
        height: info.height,
        rgba: normalize_to_rgba8(&buf, info.color_type, info.width, info.height),
    })
}

/// 解码出的 RGBA8 纹理像素。
#[cfg(feature = "texture")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaTexture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[cfg(feature = "texture")]
// mutable 切片没有 const 泛型变体(std 1.87 MSRV),此函数三处循环保留
// chunks_exact_mut(4)——本函数是该 lint 的合法例外。
#[allow(clippy::needless_range_loop)]
fn normalize_to_rgba8(raw: &[u8], color_type: png::ColorType, width: u32, height: u32) -> Vec<u8> {
    let px = (width as usize) * (height as usize);
    let mut out = vec![0u8; px * 4];
    match color_type {
        png::ColorType::Rgba => out.copy_from_slice(&raw[..px * 4]),
        png::ColorType::Rgb => {
            for i in 0..px {
                let px8 = &mut out[i * 4..i * 4 + 4];
                px8[..3].copy_from_slice(&raw[i * 3..i * 3 + 3]);
                px8[3] = 255;
            }
        }
        png::ColorType::Grayscale => {
            for i in 0..px {
                let px8 = &mut out[i * 4..i * 4 + 4];
                px8[0] = raw[i];
                px8[1] = raw[i];
                px8[2] = raw[i];
                px8[3] = 255;
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for i in 0..px {
                let px8 = &mut out[i * 4..i * 4 + 4];
                px8[0] = raw[i * 2];
                px8[1] = raw[i * 2];
                px8[2] = raw[i * 2];
                px8[3] = raw[i * 2 + 1];
            }
        }
        png::ColorType::Indexed => unreachable!("EXPAND transformation removes indexed output"),
    }
    out
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

    /// 三角网格 + 法线 + 材质基础色 的完整文档。
    fn shaded_triangle_doc() -> Value {
        serde_json::json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [{"mesh": 0}]}],
            "meshes": [{
                "primitives": [{
                    "attributes": {"POSITION": 0, "NORMAL": 1},
                    "indices": 2,
                    "material": 0
                }]
            }],
            "materials": [{
                "pbrMetallicRoughness": {
                    "baseColorFactor": [0.8, 0.2, 0.1, 1.0]
                }
            }],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 2, "componentType": 5125, "count": 3, "type": "SCALAR"}
            ],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 36},
                {"buffer": 0, "byteOffset": 72, "byteLength": 12}
            ],
            "buffers": [{"byteLength": 84}]
        })
    }

    #[test]
    fn normals_and_base_color_decode() {
        let mut bin = Vec::new();
        for v in [[0.0f32, -0.6, 0.0], [0.8, 0.6, 0.0], [-0.8, 0.6, 0.0]] {
            for c in v {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        for v in [[0.0f32, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]] {
            for c in v {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        for i in [0u32, 1, 2] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let glb = build_glb(&shaded_triangle_doc(), &bin);
        let mesh = decode_glb_mesh(&glb).unwrap();
        assert_eq!(mesh.normals.as_deref().unwrap()[1], [0.0, 0.0, 1.0]);
        assert_eq!(mesh.base_color, Some([0.8, 0.2, 0.1, 1.0]));
    }

    #[test]
    fn missing_normals_or_material_stay_none() {
        let doc = serde_json::json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [{"mesh": 0}]}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}
            ],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}],
            "buffers": [{"byteLength": 36}]
        });
        let mut bin = Vec::new();
        for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for c in v {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        let mesh = decode_glb_mesh(&build_glb(&doc, &bin)).unwrap();
        assert!(mesh.normals.is_none());
        assert!(mesh.base_color.is_none());
    }

    #[test]
    fn normal_count_mismatch_is_rejected() {
        let mut doc = shaded_triangle_doc();
        // NORMAL accessor 声明 count=2 与 POSITION=3 不符
        doc["accessors"][1]["count"] = serde_json::json!(2);
        let bin = vec![0u8; 84];
        let glb = build_glb(&doc, &bin);
        match decode_glb_mesh(&glb) {
            Err(GlbError::Unsupported(msg)) => assert!(msg.contains("NORMAL")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
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

    // ---- 敌意输入:数字全部来自不可信 JSON,溢出/回绕必须显式拒绝 ----

    #[test]
    fn overflow_count_is_rejected_not_panics() {
        // count = 2^61,× 12B 溢出 usize —— checked_mul 必须拦下
        let bin = triangle_bin();
        let mut doc = triangle_doc(36, 12);
        doc["accessors"][0]["count"] = serde_json::json!(2_305_843_009_213_693_952u64);
        let glb = build_glb(&doc, &bin);
        assert!(matches!(
            decode_glb_mesh(&glb),
            Err(GlbError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn wraparound_view_bounds_is_rejected_not_panics() {
        // byteOffset+byteLength = 2^64,release 回绕为 0 会骗过边界校验
        let bin = triangle_bin();
        let mut doc = triangle_doc(36, 12);
        doc["bufferViews"][0]["byteOffset"] = serde_json::json!(9_223_372_036_854_775_808u64);
        doc["bufferViews"][0]["byteLength"] = serde_json::json!(9_223_372_036_854_775_808u64);
        let glb = build_glb(&doc, &bin);
        assert!(matches!(
            decode_glb_mesh(&glb),
            Err(GlbError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn index_out_of_vertex_range_is_rejected() {
        // 声明 3 顶点,索引却指到 999999 —— 解码期拒绝,不等渲染端炸
        let mut bin = triangle_bin()[..36].to_vec();
        bin.extend_from_slice(&999_999u32.to_le_bytes());
        bin.extend_from_slice(&1u32.to_le_bytes());
        bin.extend_from_slice(&2u32.to_le_bytes());
        let glb = build_glb(&triangle_doc(36, 12), &bin);
        match decode_glb_mesh(&glb) {
            Err(GlbError::IndexOutOfRange { index, vertices }) => {
                assert_eq!(index, 999_999);
                assert_eq!(vertices, 3);
            }
            other => panic!("expected IndexOutOfRange, got {other:?}"),
        }
    }

    #[test]
    fn accessor_byte_offset_is_rejected() {
        // 合法但本解码器不支持的 accessor byteOffset:显式拒绝而非静默读错字节
        let mut doc = triangle_doc(36, 12);
        doc["accessors"][0]["byteOffset"] = serde_json::json!(12);
        let glb = build_glb(&doc, &triangle_bin());
        match decode_glb_mesh(&glb) {
            Err(GlbError::Unsupported(what)) => assert_eq!(what, "accessor byteOffset"),
            other => panic!("expected Unsupported(accessor byteOffset), got {other:?}"),
        }
    }

    // -------------------------------------------------------------------------
    // 场景解码(#152 第二刀)
    // -------------------------------------------------------------------------

    use serde_json::json;

    /// 自洽场景测试台:位置(36B)@0 + 法线(12B)@36 + 纹理假字节(15B→垫齐)@48 +
    /// 索引 u16×3(6B)@64。返回 (bin, 纹理假字节)。
    fn tri_bin() -> (Vec<u8>, &'static [u8]) {
        let tex = b"\x89PNG-fake-bytes";
        let mut bin = Vec::new();
        for p in [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]] {
            for v in p {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        for v in [0.0f32, 0.0, 1.0] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        for _ in 0..2 {
            for v in [0.0f32, 0.0, 1.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        bin.resize(72, 0);
        bin.extend_from_slice(tex);
        bin.resize(88, 0);
        for i in [0u16, 1, 2] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        (bin, tex)
    }

    /// 自洽场景文档:accessors/bufferViews 与 [`tri_bin`] 布局一一对应。
    fn scene_doc() -> Value {
        json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0}],
            "meshes": [{
                "primitives": [{
                    "attributes": {"POSITION": 0, "NORMAL": 1},
                    "indices": 2,
                    "material": 0
                }]
            }],
            "materials": [{
                "pbrMetallicRoughness": {
                    "baseColorFactor": [0.8, 0.2, 0.1, 1.0],
                    "metallicFactor": 0.25,
                    "roughnessFactor": 0.75
                }
            }],
            "accessors": [
                {"componentType": 5126, "count": 3, "bufferView": 0, "type": "VEC3"},
                {"componentType": 5126, "count": 3, "bufferView": 1, "type": "VEC3"},
                {"componentType": 5123, "count": 3, "bufferView": 2, "type": "SCALAR"}
            ],
            "bufferViews": [
                {"buffer": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 36},
                {"buffer": 0, "byteOffset": 88, "byteLength": 6}
            ]
        })
    }

    fn decode_scene(json_doc: Value, bin: &[u8]) -> Result<GlbScene, GlbError> {
        decode_glb_scene(&build_glb(&json_doc, bin))
    }

    #[test]
    fn scene_decodes_all_primitives_with_material_stall() {
        let (bin, _) = tri_bin();
        let scene = decode_scene(scene_doc(), &bin).unwrap();
        assert_eq!(scene.primitives.len(), 1);
        let p = &scene.primitives[0];
        assert_eq!(p.positions[0], [1.0, 0.0, 0.0]);
        assert_eq!(p.base_color, Some([0.8, 0.2, 0.1, 1.0]));
        assert_eq!(p.metallic, 0.25);
        assert_eq!(p.roughness, 0.75);
        assert!(p.base_color_texture.is_none());
        // 法线已应用单位世界变换并归一化
        assert_eq!(p.normals.as_ref().unwrap()[0], [0.0, 0.0, 1.0]);
    }

    #[test]
    fn scene_walks_two_meshes_with_two_materials() {
        let mut doc = scene_doc();
        doc["meshes"].as_array_mut().unwrap().push(json!({
            "primitives": [{
                "attributes": {"POSITION": 0},
                "material": 1
            }]
        }));
        doc["materials"].as_array_mut().unwrap().push(json!({
            "pbrMetallicRoughness": {"baseColorFactor": [0.0, 1.0, 0.0, 1.0]}
        }));
        doc["nodes"][0]["mesh"] = json!(0);
        doc["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"mesh": 1}));
        doc["scenes"][0]["nodes"] = json!([0, 1]);
        let scene = decode_scene(doc, &tri_bin().0).unwrap();
        assert_eq!(scene.primitives.len(), 2);
        assert_eq!(
            scene.primitives[1].base_color,
            Some([0.0, 1.0, 0.0, 1.0]),
            "材质默认值:第二 mesh 无 metallic/roughness 字段 → 规范默认 1.0"
        );
        assert_eq!(scene.primitives[1].metallic, 1.0);
        assert_eq!(scene.primitives[1].roughness, 1.0);
        assert!(scene.primitives[1].normals.is_none(), "声明了才解析 NORMAL");
    }

    #[test]
    fn scene_composes_parent_trs_before_child_rotation() {
        // parent: T=(1,0,0) S=(2,2,2);child: 绕 Z 转 +90°;mesh 挂在 child。
        // p=(1,0,0) → child R·p = (0,1,0) → parent S → (0,2,0) → parent T → (1,2,0)。
        // 合成顺序错(先转后缩/父子互换)都会给出别的答案。
        let mut doc = scene_doc();
        let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2 as f64;
        doc["nodes"] = json!([
            {"translation": [1.0, 0.0, 0.0], "scale": [2.0, 2.0, 2.0], "children": [1]},
            {"rotation": [0.0, 0.0, inv_sqrt2, inv_sqrt2], "mesh": 0},
        ]);
        doc["scenes"][0]["nodes"] = json!([0]);
        let scene = decode_scene(doc, &tri_bin().0).unwrap();
        assert_eq!(scene.primitives.len(), 1);
        // f32 矩阵合成有 ulp 级尾数,用容差断言
        let p = scene.primitives[0].positions[0];
        for (a, b) in p.iter().zip([1.0f32, 2.0, 0.0]) {
            assert!((a - b).abs() < 1e-4, "{p:?} vs [1,2,0]");
        }
        // 法线 (0,0,1) 经子旋转仍为 (0,0,1),父缩放均匀 → 归一化后不变
        assert_eq!(
            scene.primitives[0].normals.as_ref().unwrap()[0],
            [0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn scene_accepts_column_major_node_matrix() {
        let mut doc = scene_doc();
        // 列主序:平移 (5,0,0) 的单位阵 → m[col*4+row]
        doc["nodes"] = json!([
            {"matrix": [1,0,0,0, 0,1,0,0, 0,0,1,0, 5,0,0,1], "mesh": 0},
        ]);
        let scene = decode_scene(doc, &tri_bin().0).unwrap();
        assert_eq!(scene.primitives[0].positions[0], [6.0, 0.0, 0.0]);
    }

    #[test]
    fn scene_rejects_node_cycles_and_depths() {
        let mut doc = scene_doc();
        doc["nodes"] = json!([
            {"children": [1]},
            {"children": [0]},
        ]);
        doc["scenes"][0]["nodes"] = json!([0]);
        let err = decode_scene(doc, &tri_bin().0).unwrap_err();
        assert_eq!(err, GlbError::Unsupported("node graph contains a cycle"));
    }

    #[test]
    fn scene_rejects_skins_and_material_extensions() {
        let mut doc = scene_doc();
        doc["nodes"][0]["skin"] = json!(0);
        let err = decode_scene(doc, &tri_bin().0).unwrap_err();
        assert_eq!(
            err,
            GlbError::Unsupported("skins (mesh deform) not supported")
        );

        // 表面附加型扩展(clearcoat/sheen 等)忽略不拒——真实模型(ToyCar)常态
        let mut doc = scene_doc();
        doc["materials"][0]["extensions"] = json!({"KHR_materials_clearcoat": {}});
        assert!(decode_scene(doc, &tri_bin().0).is_ok());

        // 替换 baseColor 数据模型的扩展必须拒
        let mut doc = scene_doc();
        doc["materials"][0]["extensions"] = json!({"KHR_materials_pbrSpecularGlossiness": {}});
        let err = decode_scene(doc, &tri_bin().0).unwrap_err();
        assert_eq!(
            err,
            GlbError::Unsupported(
                "KHR_materials_pbrSpecularGlossiness replaces the baseColor model"
            )
        );
    }

    #[test]
    fn scene_texture_stall_keeps_png_bytes_and_rejects_the_rest() {
        let mut doc = scene_doc();
        doc["textures"] = json!([{"source": 0}]);
        doc["images"] = json!([{"mimeType": "image/png", "bufferView": 3}]);
        doc["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index": 0});
        // 纹理假字节已在 tri_bin 的 @72 布局里(view3 = byteOffset 72, byteLength 15)
        let (bin, tex_bytes) = tri_bin();
        doc["bufferViews"].as_array_mut().unwrap().push(json!({
            "buffer": 0, "byteOffset": 72, "byteLength": tex_bytes.len()
        }));
        let scene = decode_scene(doc, &bin).unwrap();
        let tex = scene.primitives[0]
            .base_color_texture
            .as_ref()
            .expect("texture stall");
        assert_eq!(tex.mime, "image/png");
        assert_eq!(tex.bytes, tex_bytes);

        // JPEG mime → 显式拒绝
        let mut doc = scene_doc();
        doc["textures"] = json!([{"source": 0}]);
        doc["images"] = json!([{"mimeType": "image/jpeg", "bufferView": 3}]);
        doc["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index": 0});
        let err = decode_scene(doc, &bin).unwrap_err();
        assert_eq!(
            err,
            GlbError::Unsupported("non-PNG baseColorTexture (JPEG 等)")
        );

        // 外部 URI → 显式拒绝
        let mut doc = scene_doc();
        doc["textures"] = json!([{"source": 0}]);
        doc["images"] = json!([{"mimeType": "image/png", "uri": "duck.png"}]);
        doc["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index": 0});
        let err = decode_scene(doc, &bin).unwrap_err();
        assert_eq!(
            err,
            GlbError::Unsupported("external image URI (GLB 内嵌 image 才可)")
        );
    }

    #[test]
    fn scene_supports_interleaved_views_that_frozen_path_rejects() {
        // 同一份交错文档:场景路径按 stride 解码成功;冻结路径维持显式拒绝。
        let mut bin: Vec<u8> = Vec::new();
        for p in [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]] {
            for v in p {
                bin.extend_from_slice(&v.to_le_bytes());
            }
            bin.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // 步长垫片
        }
        let mut doc = scene_doc();
        doc["bufferViews"] = json!([
            {"buffer": 0, "byteLength": bin.len(), "byteStride": 16},
        ]);
        doc["meshes"][0]["primitives"][0]["indices"] = json!(null);
        doc["meshes"][0]["primitives"][0]["attributes"]["NORMAL"] = json!(null);
        let _ = doc["meshes"][0]["primitives"][0]["attributes"]
            .as_object_mut()
            .unwrap()
            .remove("NORMAL");
        doc["meshes"][0]["primitives"][0]["material"] = json!(null);
        let _ = doc["meshes"][0]["primitives"][0]
            .as_object_mut()
            .unwrap()
            .remove("material");
        let scene = decode_scene(doc.clone(), &bin).unwrap();
        assert_eq!(scene.primitives[0].positions[1], [0.0, 1.0, 0.0]);

        let glb = build_glb(&doc, &bin);
        match decode_glb_mesh(&glb) {
            Err(GlbError::Unsupported(what)) => assert!(what.contains("interleaved"), "{what}"),
            other => panic!("frozen path must keep rejecting interleaved, got {other:?}"),
        }
    }
}
