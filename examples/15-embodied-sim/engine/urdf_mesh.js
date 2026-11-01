// URDF mesh 解析（example/15-embodied-sim）——URDF-IMPORT.md U3 的 JS 侧镜像。
//
// 与 Rust 真源对照：crates/oriera-agent/src/meshres.rs（parse_stl）。
// 二进制/ASCII 判定规则与 Rust 同一套（双端 parity）：
//   1. 长度 ≥ 84 且 84 + 50×头声明三角形数 === 实际长度 → 二进制
//      （即使以 "solid" 开头——长度精确命中公式的按二进制走，防 ASCII
//      头伪装；真实 ASCII 文件长度几乎不可能精确命中）；
//   2. 否则去前导空白后以 "solid" 开头 → ASCII；
//   3. 双判据皆失 → 抛错（给出两个判据的实测值）。
//
// 输出形状（与 Rust TriMesh 有意不同，各端各表）：
//   JS → 非索引展开：每三角形 9 个位置浮点 + 9 个法线浮点（面法线重复
//        3 次），直接喂 three.js BufferGeometry，与 STLLoader 同语义；
//        不剔除退化面（那是消费方的后处理）。
//   Rust → 顶点按位去重 + 索引 + 退化计数（合批/物理/校验用）。

// 纯函数：解析 STL（ArrayBuffer 或 TypedArray 视图），二进制/ASCII 双路。
export function parseStl(buffer) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  // 判据 1：二进制公式交叉验证
  if (bytes.length >= 84) {
    const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const n = dv.getUint32(80, true);
    if (84 + 50 * n === bytes.length) return parseBinaryStl(dv, n);
  }
  // 判据 2：solid 开头（容忍前导空白）→ ASCII
  let first = 0;
  while (first < bytes.length && (bytes[first] === 32 || (bytes[first] >= 9 && bytes[first] <= 13))) first++;
  if (bytes.length - first >= 5 &&
      bytes[first] === 0x73 && bytes[first + 1] === 0x6f && // "so"
      bytes[first + 2] === 0x6c && bytes[first + 3] === 0x69 && bytes[first + 4] === 0x64) { // "lid"
    return parseAsciiStl(new TextDecoder().decode(bytes.subarray(first)));
  }
  const declared = bytes.length >= 84
    ? `（头声明 n=${new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(80, true)}）`
    : "";
  throw new Error(
    `STL 判定失败：长度 ${bytes.length} 不吻合二进制公式 84+50×n${declared}，且不以 solid 开头`);
}

// 二进制 STL：80B header + LE u32 数 + 每三角形 50B（normal 3×f32 LE +
// 顶点 9×f32 LE + u16 attr 忽略）。长度已在外层交叉验证。
function parseBinaryStl(dv, count) {
  const positions = new Float32Array(count * 9);
  const normals = new Float32Array(count * 9);
  let p = 0;
  for (let t = 0; t < count; t++) {
    const base = 84 + 50 * t;
    const nx = dv.getFloat32(base, true);
    const ny = dv.getFloat32(base + 4, true);
    const nz = dv.getFloat32(base + 8, true);
    for (let vi = 0; vi < 3; vi++) {
      const off = base + 12 + 12 * vi;
      positions[p] = dv.getFloat32(off, true);
      positions[p + 1] = dv.getFloat32(off + 4, true);
      positions[p + 2] = dv.getFloat32(off + 8, true);
      normals[p] = nx;
      normals[p + 1] = ny;
      normals[p + 2] = nz;
      p += 3;
    }
  }
  return { positions, normals };
}

// ASCII STL 容错 token 扫描：只消费 vertex 及其后 3 个数，其余
// （solid/facet/normal/outer/loop/end*/endsolid）全部跳过——缺 endsolid、
// 指数记数（1.0e-3）、杂散空白均可。
function parseAsciiStl(text) {
  const tokens = text.split(/\s+/).filter(Boolean);
  const pos = [];
  const nrm = [];
  let nx = 0, ny = 0, nz = 0; // 最近一次 facet normal（缺省零向量）
  for (let i = 0; i < tokens.length; i++) {
    const tok = tokens[i];
    if (tok === "normal") {
      nx = stlNum(tokens[++i], "normal x");
      ny = stlNum(tokens[++i], "normal y");
      nz = stlNum(tokens[++i], "normal z");
    } else if (tok === "vertex") {
      pos.push(stlNum(tokens[++i], "vertex x"), stlNum(tokens[++i], "vertex y"), stlNum(tokens[++i], "vertex z"));
      nrm.push(nx, ny, nz);
    }
  }
  if (pos.length === 0) throw new Error("ASCII STL 无 vertex 数据");
  if (pos.length % 9 !== 0) {
    throw new Error(`ASCII STL 顶点数不是 3 的倍数（${pos.length / 3} 个坐标）`);
  }
  return { positions: Float32Array.from(pos), normals: Float32Array.from(nrm) };
}

// STL 数字字段：parseFloat 语义 + 有限性守卫（NaN/Infinity 拒绝）。
function stlNum(tok, what) {
  const v = typeof tok === "string" ? Number(tok) : NaN;
  if (!Number.isFinite(v)) throw new Error(`ASCII STL ${what} 字段非法：${tok}`);
  return v;
}
