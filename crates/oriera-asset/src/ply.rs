//! 3DGS 高斯泼溅 PLY 解码器 —— L5 `oriera-splat` 的资产入口(PLAN-splat v2 M0)。
//!
//! 与 [`crate::glb`] 同一哲学:解码器不存在的深度绝不假装,错误必须可判定,绝不猜。
//!
//! 支持面(诚实清单):
//! - 只认 `format binary_little_endian`(INRIA 3DGS 与主流导出器的默认);ascii/大端显式拒绝;
//! - `vertex` 必须是首个 element(其后的 element 不读);vertex 内出现 list 属性即拒绝;
//! - 属性**按 header 里的名字**映射,与顺序无关。必需 `x y z`、`f_dc_0..2`、`opacity`、
//!   `scale_0..2`、`rot_0..3`(float/double);其余标量属性(`nx ny nz`、`f_rest_*`…)按步长跳过;
//! - `f_rest_*` 个数只接受 0/9/24/45(SH 0–3 阶):只记录阶数、不保留系数
//!   (本版颜色只用 DC 项,每 splat 固定 56 字节);
//! - SuperSplat 压缩 PLY(`chunk` element / `packed_*` 属性)显式拒绝——那是另一种布局;
//! - 声明顶点数 × 步长先 `checked_mul` 再与实际剩余字节比对,不符即 [`PlyError::Truncated`]:
//!   分配量因此被输入长度封顶,恶意文件头无法触发无界分配。
//!
//! 输出**存激活后的值**,语义对齐 Khronos 已批准的 KHR_gaussian_splatting:
//! 线性尺度 `exp(scale_i)`、线性不透明度 `sigmoid(opacity)`、glTF 序 `[x, y, z, w]` 单位四元数
//! (PLY 的 `rot_0..3` 为 w 在前)。约定出处:Kerbl et al. 2023 参考实现的导出格式;
//! 本实现为原创,未拷贝任何第三方源代码。

use std::fmt;

/// SH 0 阶基函数 Y₀₀ = ½·√(1/π)(KHR_gaussian_splatting 取同值 0.2820947917738781)。
pub const SH_C0: f32 = 0.282_094_8;

/// 解码出的高斯云(SoA,各列下标对齐)。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SplatCloud {
    /// 中心位置(文件坐标系,不做轴变换)。
    pub positions: Vec<[f32; 3]>,
    /// 线性尺度 = exp(PLY `scale_i`):沿各局部主轴的标准差。
    pub scales: Vec<[f32; 3]>,
    /// 单位四元数,glTF 序 `[x, y, z, w]`。
    pub rotations: Vec<[f32; 4]>,
    /// 线性不透明度 = sigmoid(PLY `opacity`),∈ [0, 1]。
    pub opacities: Vec<f32>,
    /// SH 0 阶系数原值(`f_dc_0..2`);颜色见 [`SplatCloud::base_color`]。
    pub sh_dc: Vec<[f32; 3]>,
    /// 文件携带的 SH 阶数(0–3);高阶系数本版不保留。
    pub sh_degree: u8,
    /// header 中 `comment` / `obj_info` 行原文——溯源种子(导出工具、作者、生成标识)。
    pub comments: Vec<String>,
}

impl SplatCloud {
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// DC 项颜色 `max(0, 0.5 + C0·dc)`(KHR 的 0.5 偏置约定)。与 INRIA 参考实现一致,
    /// 这里只钳下限;上限由光栅输出量化时统一钳到 [0, 1]。
    pub fn base_color(&self, i: usize) -> [f32; 3] {
        let dc = self.sh_dc[i];
        [
            (0.5 + SH_C0 * dc[0]).max(0.0),
            (0.5 + SH_C0 * dc[1]).max(0.0),
            (0.5 + SH_C0 * dc[2]).max(0.0),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlyError {
    /// 不以 `ply` 魔行开头,或找不到 `end_header`。
    NotPly,
    /// header 语法错误(附行号与原因)。
    BadHeader(String),
    /// 非 `binary_little_endian`(值为 header 声明的格式名)。
    UnsupportedFormat(String),
    /// SuperSplat 压缩 PLY:另一种布局,本版不解码。
    CompressedPly,
    /// 布局不在支持面(vertex 非首个 element、vertex 内含 list、顶点数超 u32…)。
    UnsupportedLayout(&'static str),
    /// 缺少必需属性。
    MissingProperty(&'static str),
    /// 必需属性不是 float/double。
    BadPropertyType { name: String, ty: String },
    /// `f_rest_*` 个数不是 0/9/24/45。
    BadShRest(usize),
    /// 声明的顶点数据超出实际字节(乘法溢出时 needed = u64::MAX)。
    Truncated { needed: u64, available: u64 },
    /// 第 `index` 个 splat 含非有限值或零长四元数。
    InvalidValue { index: usize, what: &'static str },
}

impl fmt::Display for PlyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlyError::NotPly => write!(f, "not a PLY file (missing `ply` magic or `end_header`)"),
            PlyError::BadHeader(why) => write!(f, "malformed PLY header: {why}"),
            PlyError::UnsupportedFormat(fmt_name) => {
                write!(
                    f,
                    "unsupported PLY format `{fmt_name}` (binary_little_endian only)"
                )
            }
            PlyError::CompressedPly => {
                write!(
                    f,
                    "compressed (SuperSplat chunk/packed) PLY is not supported"
                )
            }
            PlyError::UnsupportedLayout(what) => write!(f, "unsupported PLY layout: {what}"),
            PlyError::MissingProperty(name) => write!(f, "missing required property `{name}`"),
            PlyError::BadPropertyType { name, ty } => {
                write!(
                    f,
                    "property `{name}` has type `{ty}`, expected float or double"
                )
            }
            PlyError::BadShRest(n) => {
                write!(
                    f,
                    "{n} f_rest_* properties; expected 0, 9, 24 or 45 (SH degree 0-3)"
                )
            }
            PlyError::Truncated { needed, available } => write!(
                f,
                "vertex data needs {needed} bytes but only {available} follow the header"
            ),
            PlyError::InvalidValue { index, what } => {
                write!(
                    f,
                    "splat {index}: invalid {what} (non-finite or zero-length)"
                )
            }
        }
    }
}

impl std::error::Error for PlyError {}

/// 必需属性,按 [`decode_splat_ply`] 读取顺序排列。
const REQUIRED: [&str; 14] = [
    "x", "y", "z", "f_dc_0", "f_dc_1", "f_dc_2", "opacity", "scale_0", "scale_1", "scale_2",
    "rot_0", "rot_1", "rot_2", "rot_3",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Scalar {
    fn parse(ty: &str) -> Option<Self> {
        Some(match ty {
            "char" | "int8" => Scalar::I8,
            "uchar" | "uint8" => Scalar::U8,
            "short" | "int16" => Scalar::I16,
            "ushort" | "uint16" => Scalar::U16,
            "int" | "int32" => Scalar::I32,
            "uint" | "uint32" => Scalar::U32,
            "float" | "float32" => Scalar::F32,
            "double" | "float64" => Scalar::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Scalar::I8 | Scalar::U8 => 1,
            Scalar::I16 | Scalar::U16 => 2,
            Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
            Scalar::F64 => 8,
        }
    }
}

struct Property {
    name: String,
    ty: String,
    scalar: Scalar,
}

struct Element {
    name: String,
    count: u64,
    props: Vec<Property>,
    has_list: bool,
}

/// 一个必需字段在顶点行内的字节偏移与宽度。
#[derive(Clone, Copy)]
struct Field {
    offset: usize,
    double: bool,
}

impl Field {
    fn read(self, row: &[u8]) -> f32 {
        // 行切片来自 chunks_exact(stride),偏移在建表时已保证落在 stride 内。
        if self.double {
            let mut b = [0u8; 8];
            b.copy_from_slice(&row[self.offset..self.offset + 8]);
            f64::from_le_bytes(b) as f32
        } else {
            let mut b = [0u8; 4];
            b.copy_from_slice(&row[self.offset..self.offset + 4]);
            f32::from_le_bytes(b)
        }
    }
}

/// 解码 3DGS PLY。只接受字节、先验长度后分配、坏输入一律返回错误(不 panic)。
pub fn decode_splat_ply(bytes: &[u8]) -> Result<SplatCloud, PlyError> {
    let (header_end, body_start) = find_end_header(bytes).ok_or(PlyError::NotPly)?;
    let text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| PlyError::BadHeader("header is not valid UTF-8".into()))?;
    let (format, comments, elements) = parse_header(text)?;

    match format.as_deref() {
        Some("binary_little_endian") => {}
        Some(other) => return Err(PlyError::UnsupportedFormat(other.to_string())),
        None => return Err(PlyError::BadHeader("missing `format` line".into())),
    }
    let compressed = elements.iter().any(|e| e.name == "chunk")
        || elements
            .iter()
            .filter(|e| e.name == "vertex")
            .flat_map(|e| &e.props)
            .any(|p| p.name.starts_with("packed_"));
    if compressed {
        return Err(PlyError::CompressedPly);
    }
    let vertex = match elements.first() {
        Some(e) if e.name == "vertex" => e,
        _ => {
            return Err(PlyError::UnsupportedLayout(
                "`vertex` must be the first element",
            ))
        }
    };
    if vertex.has_list {
        return Err(PlyError::UnsupportedLayout("list property inside `vertex`"));
    }

    // 按名建表:偏移 = 之前所有属性宽度之和。
    let mut fields = [None::<Field>; REQUIRED.len()];
    let mut offset = 0usize;
    let mut sh_rest = 0usize;
    for prop in &vertex.props {
        if let Some(slot) = REQUIRED.iter().position(|r| *r == prop.name) {
            let double = match prop.scalar {
                Scalar::F32 => false,
                Scalar::F64 => true,
                _ => {
                    return Err(PlyError::BadPropertyType {
                        name: prop.name.clone(),
                        ty: prop.ty.clone(),
                    })
                }
            };
            fields[slot] = Some(Field { offset, double });
        } else if prop.name.starts_with("f_rest_") {
            sh_rest += 1;
        }
        offset += prop.scalar.size();
    }
    let stride = offset;
    let mut resolved = [Field {
        offset: 0,
        double: false,
    }; REQUIRED.len()];
    for (slot, name) in REQUIRED.iter().enumerate() {
        resolved[slot] = fields[slot].ok_or(PlyError::MissingProperty(name))?;
    }
    let sh_degree = match sh_rest {
        0 => 0,
        9 => 1,
        24 => 2,
        45 => 3,
        n => return Err(PlyError::BadShRest(n)),
    };

    let body = &bytes[body_start..];
    let available = body.len() as u64;
    let needed = vertex
        .count
        .checked_mul(stride as u64)
        .ok_or(PlyError::Truncated {
            needed: u64::MAX,
            available,
        })?;
    if needed > available {
        return Err(PlyError::Truncated { needed, available });
    }
    if vertex.count > u64::from(u32::MAX) {
        return Err(PlyError::UnsupportedLayout("vertex count exceeds u32::MAX"));
    }

    let n = vertex.count as usize;
    let mut cloud = SplatCloud {
        positions: Vec::with_capacity(n),
        scales: Vec::with_capacity(n),
        rotations: Vec::with_capacity(n),
        opacities: Vec::with_capacity(n),
        sh_dc: Vec::with_capacity(n),
        sh_degree,
        comments,
    };
    let [x, y, z, dc0, dc1, dc2, op, s0, s1, s2, r0, r1, r2, r3] = resolved;
    for (index, row) in body.chunks_exact(stride).take(n).enumerate() {
        let invalid = |what| PlyError::InvalidValue { index, what };
        let pos = [x.read(row), y.read(row), z.read(row)];
        if !pos.iter().all(|v| v.is_finite()) {
            return Err(invalid("position"));
        }
        let dc = [dc0.read(row), dc1.read(row), dc2.read(row)];
        if !dc.iter().all(|v| v.is_finite()) {
            return Err(invalid("sh_dc"));
        }
        let logit = op.read(row);
        if !logit.is_finite() {
            return Err(invalid("opacity"));
        }
        // exp 溢出(log 尺度 > ~88)同样判为非有限值。
        let scale = [s0.read(row).exp(), s1.read(row).exp(), s2.read(row).exp()];
        if !scale.iter().all(|v| v.is_finite()) {
            return Err(invalid("scale"));
        }
        let (w, qx, qy, qz) = (r0.read(row), r1.read(row), r2.read(row), r3.read(row));
        let norm = (w * w + qx * qx + qy * qy + qz * qz).sqrt();
        if !(norm.is_finite() && norm > 0.0) {
            return Err(invalid("rotation"));
        }
        cloud.positions.push(pos);
        cloud.sh_dc.push(dc);
        cloud.opacities.push(1.0 / (1.0 + (-logit).exp()));
        cloud.scales.push(scale);
        cloud
            .rotations
            .push([qx / norm, qy / norm, qz / norm, w / norm]);
    }
    Ok(cloud)
}

/// 返回 (`end_header` 起始偏移, 数据区起始偏移)。`end_header` 必须独占一行,
/// 行尾 `\n` 或 `\r\n`。
fn find_end_header(bytes: &[u8]) -> Option<(usize, usize)> {
    const TAG: &[u8] = b"end_header";
    let mut from = 0;
    while let Some(off) = bytes.get(from..)?.windows(TAG.len()).position(|w| w == TAG) {
        let at = from + off;
        let rest = &bytes[at + TAG.len()..];
        let eol = if rest.starts_with(b"\r\n") {
            Some(2)
        } else if rest.starts_with(b"\n") {
            Some(1)
        } else {
            None
        };
        if let (true, Some(eol)) = (at > 0 && bytes[at - 1] == b'\n', eol) {
            return Some((at, at + TAG.len() + eol));
        }
        from = at + 1;
    }
    None
}

type Header = (Option<String>, Vec<String>, Vec<Element>);

fn parse_header(text: &str) -> Result<Header, PlyError> {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("ply") {
        return Err(PlyError::NotPly);
    }
    let mut format = None;
    let mut comments = Vec::new();
    let mut elements: Vec<Element> = Vec::new();
    for (i, raw) in lines.enumerate() {
        let bad = |why: String| PlyError::BadHeader(format!("line {}: {why}", i + 2));
        let line = raw.trim();
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            None => {}
            Some("format") => {
                let name = words
                    .next()
                    .ok_or_else(|| bad("`format` without a name".into()))?;
                format = Some(name.to_string());
            }
            Some("comment") | Some("obj_info") => comments.push(line.to_string()),
            Some("element") => {
                let name = words
                    .next()
                    .ok_or_else(|| bad("`element` without a name".into()))?;
                let count = words
                    .next()
                    .and_then(|c| c.parse::<u64>().ok())
                    .ok_or_else(|| bad(format!("element `{name}` has no valid count")))?;
                elements.push(Element {
                    name: name.to_string(),
                    count,
                    props: Vec::new(),
                    has_list: false,
                });
            }
            Some("property") => {
                let element = elements
                    .last_mut()
                    .ok_or_else(|| bad("`property` before any `element`".into()))?;
                let ty = words
                    .next()
                    .ok_or_else(|| bad("`property` without a type".into()))?;
                if ty == "list" {
                    element.has_list = true;
                    continue;
                }
                let name = words
                    .next()
                    .ok_or_else(|| bad(format!("`{ty}` property without a name")))?;
                let scalar = Scalar::parse(ty)
                    .ok_or_else(|| bad(format!("unknown property type `{ty}`")))?;
                element.props.push(Property {
                    name: name.to_string(),
                    ty: ty.to_string(),
                    scalar,
                });
            }
            Some(other) => return Err(bad(format!("unknown keyword `{other}`"))),
        }
    }
    Ok((format, comments, elements))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::LN_2;

    struct Splat {
        pos: [f64; 3],
        dc: [f64; 3],
        opacity_logit: f64,
        log_scale: [f64; 3],
        rot_wxyz: [f64; 4],
    }

    const UNIT: Splat = Splat {
        pos: [1.0, 2.0, 3.0],
        dc: [0.5, -0.2, 0.0],
        opacity_logit: 0.0,
        log_scale: [0.0, LN_2, -LN_2],
        rot_wxyz: [2.0, 0.0, 0.0, 0.0],
    };

    fn prop(ty: &str, name: &str) -> (String, String) {
        (ty.to_string(), name.to_string())
    }

    /// INRIA 导出布局:x y z [nx ny nz] f_dc_0..2 f_rest_* opacity scale_0..2 rot_0..3。
    fn inria_props(rest: usize, normals: bool) -> Vec<(String, String)> {
        let mut p = vec![prop("float", "x"), prop("float", "y"), prop("float", "z")];
        if normals {
            p.extend(["nx", "ny", "nz"].map(|n| prop("float", n)));
        }
        p.extend(["f_dc_0", "f_dc_1", "f_dc_2"].map(|n| prop("float", n)));
        p.extend((0..rest).map(|k| prop("float", &format!("f_rest_{k}"))));
        p.push(prop("float", "opacity"));
        p.extend(["scale_0", "scale_1", "scale_2"].map(|n| prop("float", n)));
        p.extend(["rot_0", "rot_1", "rot_2", "rot_3"].map(|n| prop("float", n)));
        p
    }

    fn inria_row(s: &Splat, rest: usize, normals: bool) -> Vec<f64> {
        let mut r = s.pos.to_vec();
        if normals {
            r.extend([0.0, 0.0, 1.0]);
        }
        r.extend(s.dc);
        // 解码器必须跳过的垃圾值。
        r.extend(std::iter::repeat_n(99.0, rest));
        r.push(s.opacity_logit);
        r.extend(s.log_scale);
        r.extend(s.rot_wxyz);
        r
    }

    /// 测试用写出器:header 按声明顺序,body 按属性类型写小端。
    fn write_ply(
        format: &str,
        comments: &[&str],
        props: &[(String, String)],
        rows: &[Vec<f64>],
    ) -> Vec<u8> {
        let mut head = format!("ply\nformat {format} 1.0\n");
        for c in comments {
            head.push_str(c);
            head.push('\n');
        }
        head.push_str(&format!("element vertex {}\n", rows.len()));
        for (ty, name) in props {
            head.push_str(&format!("property {ty} {name}\n"));
        }
        head.push_str("end_header\n");
        let mut out = head.into_bytes();
        for row in rows {
            assert_eq!(row.len(), props.len(), "row width must match header");
            for ((ty, _), v) in props.iter().zip(row) {
                match ty.as_str() {
                    "float" => out.extend_from_slice(&(*v as f32).to_le_bytes()),
                    "double" => out.extend_from_slice(&v.to_le_bytes()),
                    "uchar" => out.push(*v as u8),
                    other => panic!("test writer: unsupported type {other}"),
                }
            }
        }
        out
    }

    fn unit_file(rest: usize, normals: bool) -> Vec<u8> {
        write_ply(
            "binary_little_endian",
            &[],
            &inria_props(rest, normals),
            &[inria_row(&UNIT, rest, normals)],
        )
    }

    /// 替换 header 里的第一处文本(测试构造坏文件头用)。
    fn patch(bytes: &[u8], from: &str, to: &str) -> Vec<u8> {
        let at = bytes
            .windows(from.len())
            .position(|w| w == from.as_bytes())
            .expect("pattern present");
        [&bytes[..at], to.as_bytes(), &bytes[at + from.len()..]].concat()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-6 * b.abs().max(1.0)
    }

    #[test]
    fn decodes_inria_layout_into_khr_activated_values() {
        let c = decode_splat_ply(&unit_file(0, false)).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c.sh_degree, 0);
        assert_eq!(c.positions[0], [1.0, 2.0, 3.0]);
        // 线性尺度 = exp(log 尺度)
        let s = c.scales[0];
        assert!(
            close(s[0], 1.0) && close(s[1], 2.0) && close(s[2], 0.5),
            "{s:?}"
        );
        // sigmoid(0) = 0.5
        assert!(close(c.opacities[0], 0.5));
        // 文件 wxyz (2,0,0,0) → 归一化 → 内存 xyzw (0,0,0,1)
        assert_eq!(c.rotations[0], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(c.sh_dc[0], [0.5, -0.2, 0.0]);
        let col = c.base_color(0);
        assert!(close(col[0], 0.5 + SH_C0 * 0.5));
        assert!(close(col[1], 0.5 - SH_C0 * 0.2));
        assert!(close(col[2], 0.5));
    }

    #[test]
    fn rotation_is_w_first_in_file_and_xyzw_in_memory() {
        let tilted = Splat {
            rot_wxyz: [1.0, 2.0, 3.0, 4.0],
            ..UNIT
        };
        let bytes = write_ply(
            "binary_little_endian",
            &[],
            &inria_props(0, false),
            &[inria_row(&tilted, 0, false)],
        );
        let q = decode_splat_ply(&bytes).unwrap().rotations[0];
        let n = 30f32.sqrt();
        let want = [2.0 / n, 3.0 / n, 4.0 / n, 1.0 / n];
        assert!(q.iter().zip(want).all(|(a, b)| close(*a, b)), "{q:?}");
        let len = q.iter().map(|v| v * v).sum::<f32>();
        assert!(close(len, 1.0));
    }

    #[test]
    fn base_color_clamps_negative_light_to_zero() {
        let dark = Splat {
            dc: [-10.0, 0.0, 10.0],
            ..UNIT
        };
        let bytes = write_ply(
            "binary_little_endian",
            &[],
            &inria_props(0, false),
            &[inria_row(&dark, 0, false)],
        );
        let col = decode_splat_ply(&bytes).unwrap().base_color(0);
        assert_eq!(col[0], 0.0);
        assert!(close(col[1], 0.5));
        // 上限不在解码层钳制(光栅量化时统一钳)。
        assert!(col[2] > 1.0);
    }

    #[test]
    fn properties_map_by_name_not_by_offset() {
        let mut props = inria_props(0, false);
        let mut row = inria_row(&UNIT, 0, false);
        props.reverse();
        row.reverse();
        let shuffled = write_ply("binary_little_endian", &[], &props, &[row]);
        assert_eq!(
            decode_splat_ply(&shuffled).unwrap(),
            decode_splat_ply(&unit_file(0, false)).unwrap()
        );
    }

    #[test]
    fn skips_normals_and_sh_rest_for_every_degree() {
        let base = decode_splat_ply(&unit_file(0, false)).unwrap();
        for (rest, degree) in [(0, 0), (9, 1), (24, 2), (45, 3)] {
            let c = decode_splat_ply(&unit_file(rest, true)).unwrap();
            assert_eq!(c.sh_degree, degree, "rest={rest}");
            assert_eq!(c.positions, base.positions);
            assert_eq!(c.sh_dc, base.sh_dc);
            assert_eq!(c.opacities, base.opacities);
            assert_eq!(c.scales, base.scales);
            assert_eq!(c.rotations, base.rotations);
        }
    }

    #[test]
    fn rejects_sh_rest_counts_outside_degree_0_to_3() {
        assert_eq!(
            decode_splat_ply(&unit_file(10, false)),
            Err(PlyError::BadShRest(10))
        );
    }

    #[test]
    fn rejects_ascii_and_big_endian() {
        let rows = [inria_row(&UNIT, 0, false)];
        for format in ["ascii", "binary_big_endian"] {
            let bytes = write_ply(format, &[], &inria_props(0, false), &rows);
            assert_eq!(
                decode_splat_ply(&bytes),
                Err(PlyError::UnsupportedFormat(format.to_string()))
            );
        }
    }

    #[test]
    fn rejects_list_property_inside_vertex() {
        let bytes = b"ply\nformat binary_little_endian 1.0\nelement vertex 1\n\
property list uchar int idx\nproperty float x\nend_header\n\0\0\0\0\0";
        assert_eq!(
            decode_splat_ply(bytes),
            Err(PlyError::UnsupportedLayout("list property inside `vertex`"))
        );
    }

    #[test]
    fn rejects_supersplat_compressed_layout() {
        let bytes = b"ply\nformat binary_little_endian 1.0\nelement chunk 1\n\
property float min_x\nelement vertex 1\nproperty uint packed_position\nend_header\n\
\0\0\0\0\0\0\0\0";
        assert_eq!(decode_splat_ply(bytes), Err(PlyError::CompressedPly));
    }

    #[test]
    fn vertex_must_be_the_first_element() {
        let bytes = b"ply\nformat binary_little_endian 1.0\nelement face 0\n\
property list uchar int vertex_indices\nelement vertex 0\nproperty float x\nend_header\n";
        assert_eq!(
            decode_splat_ply(bytes),
            Err(PlyError::UnsupportedLayout(
                "`vertex` must be the first element"
            ))
        );
    }

    #[test]
    fn missing_required_property_is_named() {
        let props: Vec<_> = inria_props(0, false)
            .into_iter()
            .filter(|(_, n)| n != "opacity")
            .collect();
        let mut row = inria_row(&UNIT, 0, false);
        row.remove(6); // opacity 在 x y z dc×3 之后
        let bytes = write_ply("binary_little_endian", &[], &props, &[row]);
        assert_eq!(
            decode_splat_ply(&bytes),
            Err(PlyError::MissingProperty("opacity"))
        );
    }

    #[test]
    fn required_fields_accept_double_but_not_integers() {
        let mut props = inria_props(0, false);
        for p in props.iter_mut().take(3) {
            p.0 = "double".into();
        }
        let bytes = write_ply(
            "binary_little_endian",
            &[],
            &props,
            &[inria_row(&UNIT, 0, false)],
        );
        assert_eq!(
            decode_splat_ply(&bytes).unwrap().positions[0],
            [1.0, 2.0, 3.0]
        );

        props[0].0 = "uchar".into();
        let bytes = write_ply(
            "binary_little_endian",
            &[],
            &props,
            &[inria_row(&UNIT, 0, false)],
        );
        assert_eq!(
            decode_splat_ply(&bytes),
            Err(PlyError::BadPropertyType {
                name: "x".into(),
                ty: "uchar".into()
            })
        );
    }

    #[test]
    fn truncated_body_is_rejected_before_allocation() {
        let bytes = patch(
            &unit_file(0, false),
            "element vertex 1\n",
            "element vertex 1000\n",
        );
        let stride = 14 * 4;
        assert_eq!(
            decode_splat_ply(&bytes),
            Err(PlyError::Truncated {
                needed: 1000 * stride,
                available: stride
            })
        );
    }

    #[test]
    fn count_times_stride_overflow_is_an_error_not_a_panic() {
        let bytes = patch(
            &unit_file(0, false),
            "element vertex 1\n",
            "element vertex 18446744073709551615\n",
        );
        assert_eq!(
            decode_splat_ply(&bytes),
            Err(PlyError::Truncated {
                needed: u64::MAX,
                available: 14 * 4
            })
        );
    }

    #[test]
    fn non_finite_values_are_rejected_with_their_index() {
        let bad_pos = Splat {
            pos: [f64::NAN, 0.0, 0.0],
            ..UNIT
        };
        let rows = [inria_row(&UNIT, 0, false), inria_row(&bad_pos, 0, false)];
        let bytes = write_ply("binary_little_endian", &[], &inria_props(0, false), &rows);
        assert_eq!(
            decode_splat_ply(&bytes),
            Err(PlyError::InvalidValue {
                index: 1,
                what: "position"
            })
        );

        // log 尺度 100 → exp 溢出为 inf。
        let huge = Splat {
            log_scale: [100.0, 0.0, 0.0],
            ..UNIT
        };
        let bytes = write_ply(
            "binary_little_endian",
            &[],
            &inria_props(0, false),
            &[inria_row(&huge, 0, false)],
        );
        assert_eq!(
            decode_splat_ply(&bytes),
            Err(PlyError::InvalidValue {
                index: 0,
                what: "scale"
            })
        );
    }

    #[test]
    fn zero_length_quaternion_is_rejected() {
        let zero = Splat {
            rot_wxyz: [0.0; 4],
            ..UNIT
        };
        let bytes = write_ply(
            "binary_little_endian",
            &[],
            &inria_props(0, false),
            &[inria_row(&zero, 0, false)],
        );
        assert_eq!(
            decode_splat_ply(&bytes),
            Err(PlyError::InvalidValue {
                index: 0,
                what: "rotation"
            })
        );
    }

    #[test]
    fn header_comments_are_kept_as_provenance() {
        let comments = ["comment Generated by OriEra test", "obj_info scanner=demo"];
        let bytes = write_ply(
            "binary_little_endian",
            &comments,
            &inria_props(0, false),
            &[inria_row(&UNIT, 0, false)],
        );
        assert_eq!(decode_splat_ply(&bytes).unwrap().comments, comments);
    }

    #[test]
    fn crlf_header_lines_are_accepted() {
        let lf = unit_file(9, true);
        let end = lf
            .windows(b"end_header\n".len())
            .position(|w| w == b"end_header\n")
            .unwrap()
            + b"end_header\n".len();
        let header = std::str::from_utf8(&lf[..end])
            .unwrap()
            .replace('\n', "\r\n");
        let crlf = [header.as_bytes(), &lf[end..]].concat();
        assert_eq!(decode_splat_ply(&crlf), decode_splat_ply(&lf));
        assert!(decode_splat_ply(&crlf).is_ok());
    }

    #[test]
    fn non_ply_inputs_are_rejected() {
        assert_eq!(decode_splat_ply(b""), Err(PlyError::NotPly));
        assert_eq!(decode_splat_ply(b"glTF\x02\0\0\0"), Err(PlyError::NotPly));
        // 缺 end_header
        assert_eq!(
            decode_splat_ply(b"ply\nformat binary_little_endian 1.0\nelement vertex 0\n"),
            Err(PlyError::NotPly)
        );
        // 魔行不对
        assert_eq!(
            decode_splat_ply(b"plyx\nformat binary_little_endian 1.0\nend_header\n"),
            Err(PlyError::NotPly)
        );
    }

    #[test]
    fn empty_vertex_element_is_an_empty_cloud() {
        let bytes = write_ply("binary_little_endian", &[], &inria_props(24, false), &[]);
        let c = decode_splat_ply(&bytes).unwrap();
        assert!(c.is_empty());
        assert_eq!(c.sh_degree, 2);
    }
}
