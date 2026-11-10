// OriEra L5 高斯泼溅 JS 镜像——与 crates/oriera-splat 公式同源(PLAN-splat v2 M2)。
//
// 覆盖确定性部分:PLY 按名解码 + KHR 激活 + EWA 投影 + 确定性前到背排序;
// GPU 混合与 CPU 参考光栅不在镜像内(那两层的正确性由 Rust 侧互证门槛锁定)。
// 双端共读金样 contracts/golden/splat_golden.json:原始透传字段位型精确、
// 激活值容差 1e-6(exp/sigmoid 实现间 1ulp)、投影容差 5e-3/1px(f32 vs f64 累计)、
// 排序精确(金样深度两两分离 0.5)。计算全程 f64——视觉 demo 无需 f32 逐位,
// 上述容差域即 #188 评审「Math.fround 边界约定」的落地形式。

const NEAR_Z = 0.2; // project.rs NEAR_Z
const LOW_PASS = 0.3; // project.rs LOW_PASS(像素²)
const ALPHA_MIN = 1 / 255; // project.rs ALPHA_MIN
export const SH_C0 = 0.2820947917738781; // ply.rs SH_C0

export class SplatPlyError extends Error {
  constructor(message) {
    super(message);
    this.name = "SplatPlyError";
  }
}

const REQUIRED = [
  "x", "y", "z", "f_dc_0", "f_dc_1", "f_dc_2", "opacity",
  "scale_0", "scale_1", "scale_2", "rot_0", "rot_1", "rot_2", "rot_3",
];
const SCALAR_SIZE = { char: 1, int8: 1, uchar: 1, uint8: 1, short: 2, int16: 2, ushort: 2, uint16: 2, int: 4, int32: 4, uint: 4, uint32: 4, float: 4, float32: 4, double: 8, float64: 8 };

/** 解码标准 3DGS PLY(binary_little_endian、vertex 为首个 element)。
 * 与 ply.rs 同一支持面与拒绝面:ascii/大端/list/chunk/packed_* 一律报错;
 * 属性按名映射;原始透传(positions/shDc)与 Rust 位型一致。
 * 返回 {count, shDegree, comments, positions, scales, rotations, opacities, shDc}
 * (SoA:positions/shDc 为 Float32Array(3n),rotations Float32Array(4n),其余 n)。 */
export function decodeSplatPly(bytes) {
  const latin = (() => {
    let s = "";
    for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    return s;
  })();
  const endTag = "end_header";
  const at = latin.indexOf(endTag);
  if (at < 0 || !latin.startsWith("ply")) throw new SplatPlyError("不是 PLY 文件(缺少 ply 魔行或 end_header)");
  const rest = latin.slice(at + endTag.length);
  const eol = rest.startsWith("\r\n") ? 2 : rest.startsWith("\n") ? 1 : -1;
  if (eol < 0 || latin[at - 1] !== "\n") throw new SplatPlyError("PLY 头部格式不合法(end_header 未独占一行)");
  const headerEnd = at + endTag.length + eol;

  const headerText = latin.slice(0, at); // 不含 end_header 行(与 Rust find_end_header 一致)
  const lines = headerText.split(/\r?\n/);
  if (lines[0] !== "ply") throw new SplatPlyError("不是 PLY 文件(首行必须是 ply)");
  let format = null;
  const comments = [];
  const elements = [];
  for (let i = 1; i < lines.length - 1; i++) {
    const words = lines[i].trim().split(/\s+/).filter(Boolean);
    if (words.length === 0) continue;
    switch (words[0]) {
      case "format":
        format = words[1];
        break;
      case "comment":
      case "obj_info":
        comments.push(lines[i].trim());
        break;
      case "element":
        if (words.length < 3 || !/^\d+$/.test(words[2])) throw new SplatPlyError(`PLY 头部第 ${i + 1} 行:element 缺数量`);
        elements.push({ name: words[1], count: Number(words[2]), props: [], hasList: false });
        break;
      case "property":
        if (words.length < 3) throw new SplatPlyError(`PLY 头部第 ${i + 1} 行:property 不完整`);
        if (words[1] === "list") {
          if (elements.length === 0) throw new SplatPlyError("PLY 头部:property 出现在 element 之前");
          elements[elements.length - 1].hasList = true;
          continue;
        }
        if (!(words[1] in SCALAR_SIZE)) throw new SplatPlyError(`PLY 头部:未知属性类型 ${words[1]}`);
        if (elements.length === 0) throw new SplatPlyError("PLY 头部:property 出现在 element 之前");
        elements[elements.length - 1].props.push({ name: words[2], type: words[1] });
        break;
      default:
        throw new SplatPlyError(`PLY 头部:未知关键字 ${words[0]}`);
    }
  }
  if (format !== "binary_little_endian") {
    throw new SplatPlyError(`只支持 binary_little_endian 的 PLY(当前是 ${format ?? "未声明"})`);
  }
  const isCompressed =
    elements.some((e) => e.name === "chunk") ||
    elements.some((e) => e.name === "vertex" && e.props.some((p) => p.name.startsWith("packed_")));
  if (isCompressed) throw new SplatPlyError("这是压缩版(SuperSplat chunk/packed)PLY,本查看器暂不支持——请先导出为标准 3DGS PLY");
  const vertex = elements[0];
  if (!vertex || vertex.name !== "vertex") throw new SplatPlyError("vertex 必须是 PLY 的第一个 element");
  if (vertex.hasList) throw new SplatPlyError("vertex 内含 list 属性,不在支持范围");

  // 按名建表:偏移 = 之前属性宽度之和
  const fields = new Map(); // name → {offset, double}
  let stride = 0;
  let shRest = 0;
  for (const p of vertex.props) {
    const slot = REQUIRED.indexOf(p.name);
    if (slot >= 0) {
      if (p.type !== "float" && p.type !== "double") {
        throw new SplatPlyError(`必需属性 ${p.name} 的类型是 ${p.type},只认 float/double`);
      }
      fields.set(p.name, { offset: stride, double: p.type === "double" });
    } else if (p.name.startsWith("f_rest_")) {
      shRest++;
    }
    stride += SCALAR_SIZE[p.type];
  }
  for (const name of REQUIRED) {
    if (!fields.has(name)) throw new SplatPlyError(`缺少必需属性 ${name}`);
  }
  const shDegree = { 0: 0, 9: 1, 24: 2, 45: 3 }[shRest];
  if (shDegree === undefined) throw new SplatPlyError(`f_rest_* 有 ${shRest} 个,只支持 0/9/24/45(SH 0–3 阶)`);

  const bodyStart = headerEnd;
  const available = bytes.length - bodyStart;
  const needed = vertex.count * stride; // JS number 精确到 2^53,无 checked_mul 溢出位
  if (needed > available) {
    throw new SplatPlyError(`文件被截断:声明 ${vertex.count} 个顶点需要 ${needed} 字节,实际只剩 ${available}`);
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const readF64 = (row, f) => view.getFloat64(row + f.offset, true);
  const readF32 = (row, f) => view.getFloat32(row + f.offset, true);
  const read = (row, f) => (f.double ? readF64(row, f) : readF32(row, f));

  const n = vertex.count;
  const cloud = {
    count: n,
    shDegree,
    comments,
    positions: new Float32Array(3 * n),
    scales: new Float32Array(3 * n),
    rotations: new Float32Array(4 * n),
    opacities: new Float32Array(n),
    shDc: new Float32Array(3 * n),
  };
  const get = (name) => fields.get(name);
  for (let i = 0; i < n; i++) {
    const row = bodyStart + i * stride;
    const px = read(row, get("x")), py = read(row, get("y")), pz = read(row, get("z"));
    const d0 = read(row, get("f_dc_0")), d1 = read(row, get("f_dc_1")), d2 = read(row, get("f_dc_2"));
    const logit = read(row, get("opacity"));
    const s0 = Math.exp(read(row, get("scale_0")));
    const s1 = Math.exp(read(row, get("scale_1")));
    const s2 = Math.exp(read(row, get("scale_2")));
    const qw = read(row, get("rot_0")), qx = read(row, get("rot_1"));
    const qy = read(row, get("rot_2")), qz = read(row, get("rot_3"));
    const fail = (what) => {
      throw new SplatPlyError(`第 ${i + 1} 个 splat 的 ${what} 不是有效数值(文件可能损坏)`);
    };
    if (![px, py, pz].every(Number.isFinite)) fail("坐标");
    if (![d0, d1, d2].every(Number.isFinite)) fail("颜色系数");
    if (!Number.isFinite(logit)) fail("不透明度");
    if (![s0, s1, s2].every(Number.isFinite)) fail("尺度");
    const norm = Math.hypot(qw, qx, qy, qz);
    if (!Number.isFinite(norm) || norm <= 0) fail("旋转四元数");
    cloud.positions.set([px, py, pz], 3 * i);
    cloud.shDc.set([d0, d1, d2], 3 * i);
    cloud.opacities[i] = 1 / (1 + Math.exp(-logit));
    cloud.scales.set([s0, s1, s2], 3 * i);
    // PLY 是 w 在前;镜像内存与 Rust 一致用 [x, y, z, w]
    cloud.rotations.set([qx / norm, qy / norm, qz / norm, qw / norm], 4 * i);
  }
  return cloud;
}

/** 泼溅相机(与 SplatCamera::look_at 同式:OpenCV 约定 z=前向;像素中心 i+0.5)。 */
export function makeCamera(eye, target, up, fovYDeg, width, height) {
  const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
  const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
  const cross = (a, b) => [
    a[1] * b[2] - a[2] * b[1],
    a[2] * b[0] - a[0] * b[2],
    a[0] * b[1] - a[1] * b[0],
  ];
  const norm = (v) => {
    const l = Math.hypot(v[0], v[1], v[2]);
    return [v[0] / l, v[1] / l, v[2] / l];
  };
  const z = norm(sub(target, eye)); // 前向(OpenCV;不是 GL 的 eye-target!)
  let x = cross(z, up);
  if (dot(x, x) < 1e-12) {
    const alt = Math.abs(z[1]) < 0.9 ? [0, 1, 0] : [1, 0, 0];
    x = cross(z, alt);
  }
  x = norm(x);
  const y = cross(z, x);
  const f = height * 0.5 / Math.tan((fovYDeg * Math.PI) / 360);
  return {
    rotation: [x, y, z],
    translation: [-dot(x, eye), -dot(y, eye), -dot(z, eye)],
    fx: f, fy: f,
    cx: width * 0.5, cy: height * 0.5,
    width, height,
    tanHalf: [width * 0.5 / f, height * 0.5 / f],
  };
}

const toCamera = (cam, p) => [
  dot3(cam.rotation[0], p) + cam.translation[0],
  dot3(cam.rotation[1], p) + cam.translation[1],
  dot3(cam.rotation[2], p) + cam.translation[2],
];
const dot3 = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];

/** EWA 投影(与 project.rs 同式,剔除即 compaction;下标升序)。
 * 返回 [{index, depth, mean:[x,y], extent:[w,h], conic:[a,b,c], color:[r,g,b], opacity}]。 */
export function projectSplats(cloud, camera) {
  const [limX, limY] = [1.3 * camera.tanHalf[0], 1.3 * camera.tanHalf[1]];
  const out = [];
  const n = cloud.count;
  for (let i = 0; i < n; i++) {
    const opacity = cloud.opacities[i];
    if (Number.isNaN(opacity) || opacity < ALPHA_MIN) continue;
    const t = toCamera(camera, [cloud.positions[3 * i], cloud.positions[3 * i + 1], cloud.positions[3 * i + 2]]);
    const z = t[2];
    if (Number.isNaN(z) || z <= NEAR_Z) continue;
    // Σ = M·Mᵀ,M = R·S;T = J·W·M 的两行;2D 协方差对角加 LOW_PASS。
    const [qx, qy, qz, qw] = [cloud.rotations[4 * i], cloud.rotations[4 * i + 1], cloud.rotations[4 * i + 2], cloud.rotations[4 * i + 3]];
    const s = [cloud.scales[3 * i], cloud.scales[3 * i + 1], cloud.scales[3 * i + 2]];
    const m = [
      [1 - 2 * (qy * qy + qz * qz), 2 * (qx * qy - qw * qz), 2 * (qx * qz + qw * qy)],
      [2 * (qx * qy + qw * qz), 1 - 2 * (qx * qx + qz * qz), 2 * (qy * qz - qw * qx)],
      [2 * (qx * qz - qw * qy), 2 * (qy * qz + qw * qx), 1 - 2 * (qx * qx + qy * qy)],
    ].map((row) => row.map((v, k) => v * s[k]));
    const wm = camera.rotation.map((row) => [
      row[0] * m[0][0] + row[1] * m[1][0] + row[2] * m[2][0],
      row[0] * m[0][1] + row[1] * m[1][1] + row[2] * m[2][1],
      row[0] * m[0][2] + row[1] * m[1][2] + row[2] * m[2][2],
    ]);
    const tx = Math.min(Math.max(t[0] / z, -limX), limX) * z;
    const ty = Math.min(Math.max(t[1] / z, -limY), limY) * z;
    const j0 = [camera.fx / z, 0, (-camera.fx * tx) / (z * z)];
    const j1 = [0, camera.fy / z, (-camera.fy * ty) / (z * z)];
    const mul = (v, mat) => [
      v[0] * mat[0][0] + v[1] * mat[1][0] + v[2] * mat[2][0],
      v[0] * mat[0][1] + v[1] * mat[1][1] + v[2] * mat[2][1],
      v[0] * mat[0][2] + v[1] * mat[1][2] + v[2] * mat[2][2],
    ];
    const t0 = mul(j0, wm);
    const t1 = mul(j1, wm);
    const a = dot3(t0, t0) + LOW_PASS;
    const b = dot3(t0, t1);
    const c = dot3(t1, t1) + LOW_PASS;
    const det = a * c - b * b;
    if (Number.isNaN(det) || det <= 0) continue;
    const extent = [Math.ceil(3 * Math.sqrt(a)), Math.ceil(3 * Math.sqrt(c))];
    const mean = [camera.fx * t[0] / z + camera.cx, camera.fy * t[1] / z + camera.cy];
    if (mean[0] + extent[0] < 0 || mean[1] + extent[1] < 0 || mean[0] - extent[0] > camera.width || mean[1] - extent[1] > camera.height) continue;
    const inv = 1 / det;
    out.push({
      index: i,
      depth: z,
      mean,
      extent,
      conic: [c * inv, -b * inv, a * inv],
      color: [
        Math.max(0, 0.5 + SH_C0 * cloud.shDc[3 * i]),
        Math.max(0, 0.5 + SH_C0 * cloud.shDc[3 * i + 1]),
        Math.max(0, 0.5 + SH_C0 * cloud.shDc[3 * i + 2]),
      ],
      opacity,
    });
  }
  return out;
}

/** 确定性前到背排序(与 sort.rs 同一键语义:深度位型比较,平局按下标)。
 * 返回 projected 中的原始下标序列。 */
export function depthOrder(projected) {
  const bits = new Float32Array(1);
  const view = new Uint32Array(bits.buffer);
  const keyed = projected.map((p, slot) => {
    bits[0] = p.depth;
    return { slot, bits: view[0], index: p.index };
  });
  keyed.sort((u, v) => (u.bits - v.bits) || (u.index - v.index));
  return keyed.map((k) => k.index);
}

/** 程序化「霓虹花瓣环」(与 scene.rs petal_ring 同设计的 f64 版;视觉场景,非契约)。
 * 三圈切向扁长椭球花瓣 + 中心亮核,Q∞ 品牌色;同一 (count, seed) 产出同一朵云。 */
export function petalRing(count, seed) {
  const PALETTE = [
    [0.243, 0.902, 1.0],
    [1.0, 0.784, 0.38],
    [1.0, 0.42, 0.169],
  ];
  let state = BigInt(seed >>> 0);
  const unit = () => {
    state = (state * 6364136223846793005n + 1442695040888963407n) & 0xffffffffffffffffn;
    return Number((state >> 24n) & 0xffffffffn) / 4294967296;
  };
  const cloud = {
    count,
    shDegree: 0,
    comments: [`comment oriera-splat petal_ring count=${count} seed=${seed}`],
    positions: new Float32Array(3 * count),
    scales: new Float32Array(3 * count),
    rotations: new Float32Array(4 * count),
    opacities: new Float32Array(count),
    shDc: new Float32Array(3 * count),
  };
  const put = (i, pos, scale, rot, rgb, opacity) => {
    cloud.positions.set(pos, 3 * i);
    cloud.scales.set(scale, 3 * i);
    cloud.rotations.set(rot, 4 * i);
    cloud.opacities[i] = opacity;
    cloud.shDc.set(rgb.map((v) => (v - 0.5) / SH_C0), 3 * i);
  };
  const core = Math.floor(count / 10);
  for (let i = 0; i < count; i++) {
    if (i < core) {
      const r = 0.22 * Math.sqrt(unit());
      const ang = Math.PI * 2 * unit();
      const s = 0.02 + 0.02 * unit();
      put(i, [r * Math.cos(ang), 0.04 * (unit() - 0.5), r * Math.sin(ang)], [s, s, s], [0, 0, 0, 1], [1, 0.95, 0.85], 0.5 + 0.4 * unit());
    } else {
      const ring = (i - core) % 3;
      const radius = 0.9 + 0.6 * ring + 0.08 * (unit() - 0.5);
      const theta = Math.PI * 2 * unit();
      const y = 0.15 * Math.sin(3 * theta) + 0.04 * (unit() - 0.5);
      const length = 0.12 + 0.1 * unit();
      const half = (-(theta + Math.PI / 2)) * 0.5;
      const shade = 0.75 + 0.35 * unit();
      put(
        i,
        [radius * Math.cos(theta), y, radius * Math.sin(theta)],
        [length, 0.01, 0.035],
        [0, Math.sin(half), 0, Math.cos(half)],
        PALETTE[ring].map((c) => c * shade),
        0.55 + 0.4 * unit(),
      );
    }
  }
  return cloud;
}

/** DC 项颜色(与 SplatCloud::base_color 同式,只钳下限)。 */
export function baseColor(cloud, i) {
  return [
    Math.max(0, 0.5 + SH_C0 * cloud.shDc[3 * i]),
    Math.max(0, 0.5 + SH_C0 * cloud.shDc[3 * i + 1]),
    Math.max(0, 0.5 + SH_C0 * cloud.shDc[3 * i + 2]),
  ];
}
