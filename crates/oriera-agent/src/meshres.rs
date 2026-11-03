//! URDF mesh 资产管线（feature `urdf`）—— URDF-IMPORT.md U3：让 U2 里
//! 只"登记 URI"的 `<mesh filename="...">` 真正"上岗"。
//!
//! 三段职责，全部手写、零新增外部依赖：
//! * [`resolve_mesh_uri`] — URI → 本地路径（package:// / model:// /
//!   file:// / 裸相对），词法归一 + `..` 逃逸拒绝；
//! * [`parse_stl`] — STL 全量三角网格（二进制优先判定 + ASCII 容错，
//!   顶点按位去重合索引，退化/非有限面剔除并计数）；
//! * [`gltf_probe`] — GLB/glTF 轻量探针（容器校验 + JSON chunk 元信息）。
//!
//! 依赖边界（诚实清单）：GLB **全量**解码（法线/索引/包围盒/BIN chunk）
//! 在 L4 资产中心 `oriera_asset::glb::decode_glb_mesh`——L9 不反向依赖
//! L4，这里只报"有什么"（meshes/primitives/name），不搬"是什么"；
//! JSON chunk 解析复用本 crate 已有的可选依赖 serde_json（`urdf`
//! feature 下启用），不为此引新 crate。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// 去重后的三角网格：顶点按位去重 + 索引，退化三角形剔除并计数。
#[derive(Debug, Clone, PartialEq)]
pub struct TriMesh {
    pub vertices: Vec<[f32; 3]>,
    /// STL 只有面法线，去重后不映射顶点法线——恒为空（诚实边界，渲染端
    /// 按几何推导）；GLB 全量解码（L4）才有真顶点法线。
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<[u32; 3]>,
    /// `[min, max]`，由存活顶点实算（不信文件头声明）。
    pub bounds: [[f32; 3]; 2],
    /// 被剔除的退化/非有限三角形数——计数不静默。
    pub degenerate: usize,
}

/// GLB/glTF 探针结果：容器合法性 + 元信息（无几何）。
#[derive(Debug, Clone, PartialEq)]
pub struct GltfInfo {
    /// glTF `meshes` 数组长度。
    pub mesh_count: usize,
    /// 所有 mesh 的 `primitives` 总数。
    pub primitive_count: usize,
    /// 首个 mesh 的 `name`（声明字段，缺失为 None）。
    pub name: Option<String>,
    /// true = GLB 容器；false = 裸 glTF JSON（.gltf）。
    pub container_glb: bool,
}

/// 加载结果：按扩展名分流的 mesh 资产（字节来源大小随行）。
#[derive(Debug, Clone)]
pub enum UrdfMesh {
    /// STL（二进制/ASCII 同形）：全量三角网格。
    Stl { mesh: TriMesh, source_bytes: usize },
    /// GLB/glTF：轻量探针信息（几何解码走 L4 `oriera_asset::glb`）。
    Probed { info: GltfInfo, source_bytes: usize },
}

const GLB_MAGIC: &[u8; 4] = b"glTF";

/// 解析 URDF mesh URI → 本地文件路径。
///
/// | URI 形态 | 解析 |
/// |---|---|
/// | `package://<pkg>/<rel>` | `packages[pkg]/<rel>`，缺 pkg 报错并列已知包 |
/// | `model://<pkg>/<rel>` | 同 package 语义（Gazebo） |
/// | `file://<path>` | 剥前缀按本地路径（绝对按本地 OS 语义） |
/// | 裸相对路径 | `base_dir.join(path)`；绝对路径原样归一 |
///
/// 路径安全：反斜杠统一归一为 `/`（Windows 工具链产出，跨平台行为
/// 一致）；词法归一后 `..` 越过 base_dir/包根即拒绝；含 `:` 的相对
/// 组件拒绝（Windows 盘符/ADS 逃逸面）。纯词法操作，不触盘。
pub fn resolve_mesh_uri(
    uri: &str,
    base_dir: &Path,
    packages: &BTreeMap<String, PathBuf>,
) -> Result<PathBuf, String> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Err("mesh URI 为空".into());
    }
    if let Some(rest) = uri
        .strip_prefix("package://")
        .or_else(|| uri.strip_prefix("model://"))
    {
        let rest = rest.replace('\\', "/");
        let (pkg, rel) = rest
            .split_once('/')
            .ok_or_else(|| format!("mesh URI `{uri}` 缺包内相对路径（package://<pkg>/<rel>）"))?;
        if rel.is_empty() {
            return Err(format!("mesh URI `{uri}` 缺包内相对路径"));
        }
        let root = packages.get(pkg).ok_or_else(|| {
            let known = if packages.is_empty() {
                "（未注册任何包）".to_string()
            } else {
                packages.keys().cloned().collect::<Vec<_>>().join(", ")
            };
            format!("mesh URI `{uri}` 的包 `{pkg}` 未注册；已知包：{known}")
        })?;
        join_checked(root, rel).map_err(|e| format!("mesh URI `{uri}`：{e}"))
    } else {
        resolve_local(uri.strip_prefix("file://").unwrap_or(uri), base_dir)
    }
}

/// 本地路径（file:// 剥前缀后的剩余物或裸路径）：绝对 → 锚点下归一；
/// 相对 → base_dir 之下归一（`..` 越过 base_dir 拒绝）。
fn resolve_local(rest: &str, base_dir: &Path) -> Result<PathBuf, String> {
    let mut p = rest.replace('\\', "/");
    // RFC 8089 三斜杠形式 `file:///C:/x.stl` 剥前缀后是 `/C:/x.stl`——
    // Windows 上无 Prefix 组件不算绝对路径（审查 P1：会落入相对分支
    // 且 `C:` 组件被当盘符逃逸误拒）。前导 `/` + 盘符 → 去掉前导 `/`。
    if p.len() >= 3
        && p.as_bytes()[0] == b'/'
        && p.as_bytes()[1].is_ascii_alphabetic()
        && p.as_bytes()[2] == b':'
    {
        p.remove(0);
    }
    let candidate = Path::new(&p);
    if candidate.is_absolute() {
        normalize_absolute(candidate)
    } else {
        join_checked(base_dir, &p).map_err(|e| format!("相对 mesh 路径：{e}"))
    }
}

/// `root.join(rel)` 的安全版：按 `/` 切分逐层下推（组件级 push，绕开
/// PathBuf::push 对带根路径的整串替换），`..` 在根处被拒绝。
fn join_checked(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut cur = root.to_path_buf();
    let mut depth = 0usize;
    for comp in rel.split('/').filter(|c| !c.is_empty() && *c != ".") {
        if comp == ".." {
            if depth == 0 {
                return Err(format!(
                    "路径 `{rel}` 的 `..` 越过根 `{}`（逃逸拒绝）",
                    root.display()
                ));
            }
            cur.pop();
            depth -= 1;
        } else if comp.contains(':') {
            return Err(format!(
                "路径 `{rel}` 组件 `{comp}` 含 `:`（盘符/ADS 逃逸面，拒绝）"
            ));
        } else {
            cur.push(comp);
            depth += 1;
        }
    }
    Ok(cur)
}

/// 绝对路径词法归一：锚点（`/` 或 `C:\`）之下消化 `.`/`..`，`..` 越过
/// 锚点即拒绝。
fn normalize_absolute(p: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    let mut anchor = PathBuf::new();
    let mut anchor_done = false;
    for comp in p.components() {
        match comp {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                out.push(comp.as_os_str());
                if !anchor_done {
                    anchor.push(comp.as_os_str());
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                anchor_done = true;
                if out == anchor {
                    return Err(format!("路径 `{}` 的 `..` 越过根（逃逸拒绝）", p.display()));
                }
                out.pop();
            }
            std::path::Component::Normal(_) => {
                anchor_done = true;
                out.push(comp.as_os_str());
            }
        }
    }
    Ok(out)
}

/// 解析 STL（二进制优先判定 + ASCII 容错）。
///
/// 判定规则（JS 镜像 `examples/15-embodied-sim/engine/urdf_mesh.js`
/// 同一套，双端 parity）：
/// 1. 长度 ≥ 84 且 `84 + 50 × 头声明三角形数 == 实际长度` → 二进制
///    （即使以 "solid" 开头——长度精确命中公式的按二进制走，防 ASCII
///    头伪装；真实 ASCII 文件长度几乎不可能精确命中）；
/// 2. 否则去前导空白后以 "solid" 开头 → ASCII；
/// 3. 双判据皆失 → 报错并给出两个判据的实测值。
pub fn parse_stl(bytes: &[u8]) -> Result<TriMesh, String> {
    if bytes.len() >= 84 {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        let expect = 50usize.checked_mul(n).and_then(|b| 84usize.checked_add(b));
        if expect == Some(bytes.len()) {
            return parse_stl_binary(bytes, n);
        }
    }
    let head = bytes.iter().position(|b| !b.is_ascii_whitespace());
    if matches!(head, Some(i) if bytes[i..].starts_with(b"solid")) {
        return parse_stl_ascii(bytes);
    }
    Err(format!(
        "STL 判定失败：长度 {len} 不吻合二进制公式 84+50×n{declared}，且不以 solid 开头",
        len = bytes.len(),
        declared = if bytes.len() >= 84 {
            let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]);
            format!("（头声明 n={n}）")
        } else {
            String::new()
        },
    ))
}

/// 二进制 STL：80B header + LE u32 数 + 每三角形 50B
/// （normal 3×f32 + 顶点 9×f32 + u16 attr，attr 忽略）。
/// 长度已在 [`parse_stl`] 交叉验证——超大 n 的内存放大被公式挡在门外。
fn parse_stl_binary(bytes: &[u8], n: usize) -> Result<TriMesh, String> {
    let mut tris = Vec::with_capacity(n);
    for t in 0..n {
        let base = 84 + 50 * t;
        let mut tri = [[0.0f32; 3]; 3];
        for (vi, v) in tri.iter_mut().enumerate() {
            let off = base + 12 + 12 * vi;
            *v = [
                f32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]),
                f32::from_le_bytes([
                    bytes[off + 4],
                    bytes[off + 5],
                    bytes[off + 6],
                    bytes[off + 7],
                ]),
                f32::from_le_bytes([
                    bytes[off + 8],
                    bytes[off + 9],
                    bytes[off + 10],
                    bytes[off + 11],
                ]),
            ];
        }
        tris.push(tri);
    }
    build_mesh(&tris, 0)
}

/// ASCII STL 容错 token 扫描：只消费 `vertex` 及其后 3 个数，其余
/// （solid/facet/normal/outer/loop/end*/endsolid）全部跳过——缺
/// endsolid、指数记数（1.0e-3 / 2.5E-1）、杂散空白均可。
fn parse_stl_ascii(bytes: &[u8]) -> Result<TriMesh, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "ASCII STL 含非 UTF-8 字节".to_string())?;
    let mut tokens = text.split_ascii_whitespace();
    let mut tris: Vec<[[f32; 3]; 3]> = Vec::new();
    let mut facet: Vec<[f32; 3]> = Vec::with_capacity(3);
    let mut saw_vertex = false;
    while let Some(tok) = tokens.next() {
        if tok != "vertex" {
            continue;
        }
        saw_vertex = true;
        let mut v = [0.0f32; 3];
        for c in &mut v {
            let num = tokens
                .next()
                .ok_or("ASCII STL 戛然止于 vertex（缺 3 个坐标）")?;
            *c = num
                .parse::<f32>()
                .map_err(|_| format!("ASCII STL vertex 坐标非法：`{num}`"))?;
        }
        facet.push(v);
        if facet.len() == 3 {
            tris.push([facet[0], facet[1], facet[2]]);
            facet.clear();
        }
    }
    if !saw_vertex {
        return Err("ASCII STL 无 vertex 数据".into());
    }
    if !facet.is_empty() {
        return Err(format!(
            "ASCII STL 顶点数不是 3 的倍数（悬空 {} 个）",
            facet.len()
        ));
    }
    build_mesh(&tris, 0)
}

/// 三角形流 → [`TriMesh`]：顶点按位去重合索引、退化/非有限面剔除并
/// 计数、bounds 由存活顶点实算、孤儿顶点修剪（索引重映射）。
fn build_mesh(tris: &[[[f32; 3]; 3]], mut degenerate: usize) -> Result<TriMesh, String> {
    let mut dedup: HashMap<[u32; 3], u32> = HashMap::new();
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<[u32; 3]> = Vec::new();
    for tri in tris {
        // 非有限坐标按退化剔除：bounds 绝不被 NaN/Inf 污染
        if tri.iter().any(|v| v.iter().any(|c| !c.is_finite())) {
            degenerate += 1;
            continue;
        }
        let mut idx = [0u32; 3];
        for (vi, v) in tri.iter().enumerate() {
            // 按位去重：位形即身份（-0.0 与 0.0 视为不同点，可判定）
            let key = [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()];
            let next = verts.len() as u32;
            idx[vi] = *dedup.entry(key).or_insert(next);
            if idx[vi] == next {
                verts.push(*v);
            }
        }
        if idx[0] == idx[1] || idx[1] == idx[2] || idx[0] == idx[2] {
            degenerate += 1;
            continue;
        }
        indices.push(idx);
    }
    if indices.is_empty() {
        return Err(format!(
            "STL 无有效三角形（{degenerate} 个全部退化/非有限）"
        ));
    }
    // 修剪被剔除面独占的孤儿顶点（其坐标不得进 bounds）
    let mut used = vec![false; verts.len()];
    for t in &indices {
        for &i in t {
            used[i as usize] = true;
        }
    }
    let mut remap = vec![0u32; verts.len()];
    let mut vertices = Vec::with_capacity(verts.len());
    let mut bounds = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
    for (i, v) in verts.iter().enumerate() {
        if !used[i] {
            continue;
        }
        remap[i] = vertices.len() as u32;
        vertices.push(*v);
        for a in 0..3 {
            bounds[0][a] = bounds[0][a].min(v[a]);
            bounds[1][a] = bounds[1][a].max(v[a]);
        }
    }
    for t in indices.iter_mut() {
        for i in t.iter_mut() {
            *i = remap[*i as usize];
        }
    }
    Ok(TriMesh {
        vertices,
        normals: Vec::new(),
        indices,
        bounds,
        degenerate,
    })
}

/// GLB/glTF 轻量探针：容器校验（magic `glTF` / 0x46546C67、
/// version==2、声明总长与实际一致）+ JSON chunk 元信息。裸 .gltf
/// （JSON 文本，无容器）同路解析。不做几何解码——那是 L4 的活。
pub fn gltf_probe(bytes: &[u8]) -> Result<GltfInfo, String> {
    let json: &[u8] = if bytes.starts_with(GLB_MAGIC) {
        if bytes.len() < 20 {
            return Err(format!(
                "GLB 头截断（{} 字节，容器 + JSON chunk 头需 ≥20）",
                bytes.len()
            ));
        }
        let u32le = |off: usize| {
            u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
        };
        if u32le(4) != 2 {
            return Err("GLB version ≠ 2（本探针只认 glTF 2.0 容器）".into());
        }
        let total = u32le(8) as usize;
        if total != bytes.len() {
            return Err(format!("GLB 声明总长 {total} ≠ 实际 {} 字节", bytes.len()));
        }
        if &bytes[16..20] != b"JSON" {
            return Err("GLB 第一个 chunk 不是 JSON".into());
        }
        let json_len = u32le(12) as usize;
        let json_end = 20usize
            .checked_add(json_len)
            .ok_or("GLB JSON chunk 长度溢出")?;
        if json_end > bytes.len() {
            return Err(format!(
                "GLB JSON chunk 截断：声明 {json_len} 字节超出实际 {}",
                bytes.len()
            ));
        }
        &bytes[20..json_end]
    } else {
        let looks_json = bytes
            .iter()
            .find(|b| !b.is_ascii_whitespace())
            .is_some_and(|&b| b == b'{');
        if !looks_json {
            return Err(
                "gltf_probe：既非 GLB 容器（magic `glTF`），也非 glTF JSON（首字符 `{`）".into(),
            );
        }
        bytes
    };
    let doc: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| format!("glTF JSON 非法：{e}"))?;
    let meshes = doc.get("meshes").and_then(serde_json::Value::as_array);
    let mesh_count = meshes.map_or(0, Vec::len);
    let primitive_count = meshes.map_or(0, |ms| {
        ms.iter()
            .filter_map(|m| m.get("primitives"))
            .filter_map(serde_json::Value::as_array)
            .map(Vec::len)
            .sum()
    });
    let name = meshes
        .and_then(|ms| ms.first())
        .and_then(|m| m.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Ok(GltfInfo {
        mesh_count,
        primitive_count,
        name,
        container_glb: bytes.starts_with(GLB_MAGIC),
    })
}

/// 加载 URDF mesh：URI 解析（[`resolve_mesh_uri`]）+ 按扩展名分流
/// （大小写不敏感）：`.stl` → [`UrdfMesh::Stl`]；`.glb`/`.gltf` →
/// [`UrdfMesh::Probed`]；其余显式报错。阻塞文件 IO——CLI/工具链用，
/// 不进热循环。
pub fn load_urdf_mesh(
    uri: &str,
    base_dir: &Path,
    packages: &BTreeMap<String, PathBuf>,
) -> Result<UrdfMesh, String> {
    let path = resolve_mesh_uri(uri, base_dir, packages)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let bytes =
        std::fs::read(&path).map_err(|e| format!("mesh 文件读取失败 `{}`：{e}", path.display()))?;
    match ext.as_str() {
        "stl" => parse_stl(&bytes)
            .map(|mesh| UrdfMesh::Stl {
                mesh,
                source_bytes: bytes.len(),
            })
            .map_err(|e| format!("`{}`：{e}", path.display())),
        "glb" | "gltf" => gltf_probe(&bytes)
            .map(|info| UrdfMesh::Probed {
                info,
                source_bytes: bytes.len(),
            })
            .map_err(|e| format!("`{}`：{e}", path.display())),
        other => Err(format!(
            "mesh `{}` 扩展名 `.{other}` 不受支持（支持 .stl/.glb/.gltf；OBJ/DAE 等 L4 管线扩展）",
            path.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// 测试：STL/GLB 全部内存合成（不依赖外部 fixture）；临时文件走
// env::temp_dir 唯一子目录，Drop 整树清理。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 四面体（4 顶点 4 面）：bounds 精确可断言。
    const TETRA: [[[f32; 3]; 3]; 4] = [
        [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        [[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    ];

    /// 合成二进制 STL（80B header + 计数 + 50B/面，法线占位零）。
    fn write_binary_stl(tris: &[[[f32; 3]; 3]]) -> Vec<u8> {
        let mut out = vec![0u8; 80];
        out.extend_from_slice(&(tris.len() as u32).to_le_bytes());
        for t in tris {
            for v in std::iter::once(&[0.0f32; 3]).chain(t.iter()) {
                for c in v {
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        out
    }

    /// 构造最小 GLB（只有 JSON chunk，4 字节对齐补空格；探针不读 BIN）。
    fn build_glb(json: &str) -> Vec<u8> {
        let json_pad = json.len().div_ceil(4) * 4;
        let total = 12 + 8 + json_pad;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(GLB_MAGIC);
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_pad as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(json.as_bytes());
        out.extend(std::iter::repeat_n(b' ', json_pad - json.len()));
        out
    }

    /// 唯一临时目录 + Drop 整树删除（并发测试安全）。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let d = std::env::temp_dir().join(format!(
                "oriera_meshres_{}_{}_{}",
                std::process::id(),
                tag,
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&d).expect("创建临时目录");
            Self(d)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pkg_map(root: &Path) -> BTreeMap<String, PathBuf> {
        BTreeMap::from([
            ("oriera_arm".to_string(), root.join("oriera_arm")),
            (
                "panda_description".to_string(),
                root.join("panda_description"),
            ),
        ])
    }

    // ---- STL 解析 ----

    #[test]
    fn binary_stl_roundtrip() {
        let m = parse_stl(&write_binary_stl(&TETRA)).expect("binary tetra");
        assert_eq!(m.vertices.len(), 4, "四面体 4 顶点去重");
        assert_eq!(m.indices.len(), 4);
        assert_eq!(m.degenerate, 0);
        assert_eq!(m.bounds, [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]]);
        for t in &m.indices {
            assert!(t.iter().all(|&i| (i as usize) < m.vertices.len()));
            assert!(t[0] != t[1] && t[1] != t[2] && t[0] != t[2]);
        }
    }

    #[test]
    fn duplicate_vertices_are_deduped_with_indices() {
        // 四边形 = 两三角形共享一条对角边：6 个位姿 → 4 个唯一顶点 + 2 面
        let tris = [
            [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]],
            [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
        ];
        let m = parse_stl(&write_binary_stl(&tris)).unwrap();
        assert_eq!(m.vertices.len(), 4);
        assert_eq!(m.indices.len(), 2);
    }

    #[test]
    fn degenerate_and_nonfinite_are_culled_and_counted() {
        let mut tris = TETRA.to_vec();
        tris.push([[2.0, 2.0, 2.0]; 3]); // 三点重合
        tris.push([[2.0, 2.0, 2.0], [3.0, 3.0, 3.0], [3.0, 3.0, 3.0]]); // 两点重合
        tris.push([[9.9, 0.0, 0.0], [f32::NAN, 0.0, 0.0], [0.0, 9.9, 0.0]]); // 非有限
        let m = parse_stl(&write_binary_stl(&tris)).unwrap();
        assert_eq!(m.indices.len(), 4, "仅原四面体存活");
        assert_eq!(m.degenerate, 3);
        // bounds 不被 culled 面的远点/NaN 污染（孤儿顶点已修剪）
        assert_eq!(m.bounds, [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]]);
    }

    #[test]
    fn ascii_stl_parses_with_dedup() {
        let ascii = b"solid quad
  facet normal 0 0 -1
    outer loop
      vertex 0 0 0
      vertex 0 1 0
      vertex 1 0 0
    endloop
  endfacet
  facet normal 0 0 -1
    outer loop
      vertex 1 1 0
      vertex 1 0 0
      vertex 0 0 0
    endloop
  endfacet
endsolid quad
";
        let m = parse_stl(ascii).expect("ascii stl");
        assert_eq!(m.vertices.len(), 4, "共享边去重");
        assert_eq!(m.indices.len(), 2);
        assert_eq!(m.bounds, [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0]]);
    }

    #[test]
    fn ascii_stl_tolerates_scientific_notation_and_missing_endsolid() {
        let ascii = b"solid x
facet normal 0 0 1
outer loop
vertex 1.0e-1 0.0 0.0
vertex 0.0 2.5E-1 0.0
vertex 0.0 0.0 3.0
endloop
endfacet
";
        let m = parse_stl(ascii).unwrap();
        assert_eq!(m.vertices.len(), 3);
        assert!((m.vertices[0][0] - 0.1).abs() < 1e-6);
        assert_eq!(m.bounds[1][2], 3.0);
    }

    #[test]
    fn solid_header_with_binary_body_is_binary() {
        // 伪装防线：以 "solid" 开头、长度精确吻合二进制公式 → 按二进制走
        let mut bytes = write_binary_stl(&TETRA);
        bytes[..5].copy_from_slice(b"solid");
        let m = parse_stl(&bytes).expect("长度吻合公式必须按二进制解析");
        assert_eq!(m.vertices.len(), 4);
        assert_eq!(m.indices.len(), 4);
    }

    #[test]
    fn undecidable_bytes_are_an_honest_error() {
        // 非 solid 开头 + 长度不吻合 → 报错给出两个判据的实测值
        let mut bytes = write_binary_stl(&TETRA);
        bytes.truncate(bytes.len() - 10);
        let err = parse_stl(&bytes).unwrap_err();
        assert!(err.contains("solid"), "错误需提及 ASCII 判据：{err}");
        assert!(err.contains("二进制"), "错误需提及二进制判据：{err}");
        // 纯垃圾字节同理
        assert!(parse_stl(b"\x00\x01garbage").is_err());
    }

    #[test]
    fn empty_and_vertexless_stl_error() {
        assert!(parse_stl(b"solid x\nendsolid x\n").is_err());
        assert!(parse_stl(b"").is_err());
    }

    // ---- GLB/glTF 探针 ----

    #[test]
    fn glb_probe_counts_meshes_primitives_and_name() {
        let glb = build_glb(
            r#"{"asset":{"version":"2.0"},"meshes":[
            {"name":"link_a","primitives":[{"attributes":{"POSITION":0}},{"attributes":{"POSITION":0}}]},
            {"primitives":[{"attributes":{"POSITION":0}}]}]}"#,
        );
        let info = gltf_probe(&glb).expect("probe");
        assert_eq!(info.mesh_count, 2);
        assert_eq!(info.primitive_count, 3);
        assert_eq!(info.name.as_deref(), Some("link_a"));
        assert!(info.container_glb);
    }

    #[test]
    fn bare_gltf_json_probes_too() {
        let info = gltf_probe(br#"{"meshes":[{"name":"m","primitives":[{},{}]}]}"#).expect("probe");
        assert_eq!(info.mesh_count, 1);
        assert_eq!(info.primitive_count, 2);
        assert_eq!(info.name.as_deref(), Some("m"));
        assert!(!info.container_glb);
    }

    #[test]
    fn glb_without_meshes_probes_to_zero() {
        let info = gltf_probe(&build_glb(r#"{"asset":{"version":"2.0"}}"#)).unwrap();
        assert_eq!(info.mesh_count, 0);
        assert_eq!(info.primitive_count, 0);
        assert_eq!(info.name, None);
    }

    #[test]
    fn bad_glb_headers_are_rejected_honestly() {
        // 坏 magic（也非 JSON）
        assert!(gltf_probe(b"not a glb at all....").is_err());
        // version != 2
        let mut v = build_glb("{}");
        v[4] = 3;
        assert!(gltf_probe(&v).unwrap_err().contains("version"));
        // 声明总长不符
        let mut v = build_glb("{}");
        v[8..12].copy_from_slice(&999u32.to_le_bytes());
        assert!(gltf_probe(&v).unwrap_err().contains("总长"));
        // JSON chunk 非法
        assert!(gltf_probe(&build_glb("{ nope")).is_err());
        // 容器截断
        assert!(gltf_probe(&build_glb("{}")[..10]).is_err());
    }

    // ---- URI 解析 ----

    #[test]
    fn package_uri_resolves_into_package_root() {
        let t = TempDir::new("pkg_ok");
        let pkgs = pkg_map(&t.0);
        assert_eq!(
            resolve_mesh_uri("package://oriera_arm/meshes/link.stl", &t.0, &pkgs).unwrap(),
            t.0.join("oriera_arm").join("meshes").join("link.stl")
        );
        // model:// 同 package 语义
        assert_eq!(
            resolve_mesh_uri("model://panda_description/visual/link0.glb", &t.0, &pkgs).unwrap(),
            t.0.join("panda_description")
                .join("visual")
                .join("link0.glb")
        );
        // Windows 反斜杠 URI 归一为 '/'
        assert_eq!(
            resolve_mesh_uri("package://oriera_arm/meshes\\link.stl", &t.0, &pkgs).unwrap(),
            t.0.join("oriera_arm").join("meshes").join("link.stl")
        );
    }

    #[test]
    fn missing_package_error_lists_known_packages() {
        let t = TempDir::new("pkg_miss");
        let pkgs = pkg_map(&t.0);
        let err = resolve_mesh_uri("package://nope_pkg/m.stl", &t.0, &pkgs).unwrap_err();
        assert!(err.contains("nope_pkg"), "{err}");
        assert!(
            err.contains("oriera_arm") && err.contains("panda_description"),
            "报错需列已知包：{err}"
        );
        // 完全未注册时的提示同样可判定
        let err = resolve_mesh_uri("package://nope_pkg/m.stl", &t.0, &BTreeMap::new()).unwrap_err();
        assert!(err.contains("未注册任何包"), "{err}");
    }

    #[test]
    fn dotdot_escape_is_rejected() {
        let t = TempDir::new("escape");
        let pkgs = pkg_map(&t.0);
        // 包根逃逸
        let err =
            resolve_mesh_uri("package://oriera_arm/../../etc/passwd", &t.0, &pkgs).unwrap_err();
        assert!(err.contains("逃逸"), "{err}");
        // base_dir 逃逸（裸相对路径）
        let err = resolve_mesh_uri("../outside.stl", &t.0, &pkgs).unwrap_err();
        assert!(err.contains("逃逸"), "{err}");
        // 包内回跳不出根 → 词法归一放行（不搞一刀切）
        assert_eq!(
            resolve_mesh_uri("package://oriera_arm/a/../b.stl", &t.0, &pkgs).unwrap(),
            t.0.join("oriera_arm").join("b.stl")
        );
        // Windows 盘符组件拒绝（PathBuf::push 会整串替换的逃逸面）
        let err = resolve_mesh_uri("package://oriera_arm/C:/evil.stl", &t.0, &pkgs).unwrap_err();
        assert!(err.contains(":"), "{err}");
        // 绝对路径越过根
        assert!(resolve_mesh_uri("file:///../up.stl", &t.0, &pkgs).is_err());
    }

    #[test]
    fn file_and_relative_paths_resolve() {
        let t = TempDir::new("file_rel");
        let pkgs = pkg_map(&t.0);
        // 裸相对 → base_dir.join
        assert_eq!(
            resolve_mesh_uri("meshes/link.stl", &t.0, &pkgs).unwrap(),
            t.0.join("meshes").join("link.stl")
        );
        // file:// 剥前缀（相对语义）
        assert_eq!(
            resolve_mesh_uri("file://meshes/link.stl", &t.0, &pkgs).unwrap(),
            t.0.join("meshes").join("link.stl")
        );
        // file:// 绝对路径（锚点下归一）
        let abs = t.0.join("abs.stl");
        let uri = format!("file://{}", abs.display());
        assert_eq!(resolve_mesh_uri(&uri, &t.0, &pkgs).unwrap(), abs);
        // RFC 8089 三斜杠 + 盘符（Windows 最常见写法）：`file:///C:/..`
        // 剥前缀后 `/C:/..` 要被识别为绝对路径而非盘符逃逸误拒（审查 P1）。
        #[cfg(windows)]
        {
            let uri3 = format!("file:///{}", abs.display().to_string().replace('\\', "/"));
            assert!(
                uri3.starts_with("file:///") && uri3.as_bytes()[9] == b':',
                "fixture: {uri3}"
            );
            assert_eq!(resolve_mesh_uri(&uri3, &t.0, &pkgs).unwrap(), abs);
        }
        // 空 URI
        assert!(resolve_mesh_uri("  ", &t.0, &pkgs).is_err());
    }

    // ---- load_urdf_mesh（临时文件，Drop 清理）----

    #[test]
    fn load_routes_by_extension_case_insensitive() {
        let t = TempDir::new("routes");
        let pkgs = pkg_map(&t.0);
        let pkg_dir = t.0.join("oriera_arm");
        std::fs::create_dir_all(pkg_dir.join("meshes")).unwrap();

        // .STL（大写）→ 全量网格 + 字节来源大小
        std::fs::write(pkg_dir.join("meshes/link.STL"), write_binary_stl(&TETRA)).unwrap();
        match load_urdf_mesh("package://oriera_arm/meshes/link.STL", &t.0, &pkgs).unwrap() {
            UrdfMesh::Stl { mesh, source_bytes } => {
                assert_eq!(mesh.indices.len(), 4);
                assert_eq!(source_bytes, 84 + 50 * 4);
            }
            other => panic!("expected Stl, got {other:?}"),
        }

        // .GLB（大写）→ 探针
        let glb = build_glb(r#"{"meshes":[{"name":"c","primitives":[{},{}]}]}"#);
        std::fs::write(pkg_dir.join("meshes/cover.GLB"), &glb).unwrap();
        match load_urdf_mesh("package://oriera_arm/meshes/cover.GLB", &t.0, &pkgs).unwrap() {
            UrdfMesh::Probed { info, .. } => {
                assert_eq!(info.mesh_count, 1);
                assert_eq!(info.primitive_count, 2);
            }
            other => panic!("expected Probed, got {other:?}"),
        }

        // 不认识的扩展 → 显式报错（不静默跳过）
        std::fs::write(pkg_dir.join("meshes/part.obj"), b"v 0 0 0").unwrap();
        let err = load_urdf_mesh("package://oriera_arm/meshes/part.obj", &t.0, &pkgs).unwrap_err();
        assert!(err.contains("obj") && err.contains("stl"), "{err}");
    }

    #[test]
    fn load_missing_file_is_an_honest_error() {
        let t = TempDir::new("miss_file");
        let pkgs = pkg_map(&t.0);
        let err = load_urdf_mesh("package://oriera_arm/ghost.stl", &t.0, &pkgs).unwrap_err();
        assert!(err.contains("ghost") && err.contains("读取失败"), "{err}");
    }
}
