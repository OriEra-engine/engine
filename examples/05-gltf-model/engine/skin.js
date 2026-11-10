// OriEra L4 glTF 蒙皮 JS 镜像——与 crates/oriera-asset/src/glb.rs decode_glb_rig
// 公式同源(#153 M1,demo 05/11 摘掉 three.js 的骨骼动画底座)。
// 覆盖:GLB 容器、skins(joints+IBM 列主序)、animations(channels×samplers,
// LINEAR/STEP)、node TRS/matrix(列主序 16 元分解)、JOINTS_0/WEIGHTS_0、
// 关节面板(jointMatricesAt)与 CPU 蒙皮(skinVertices)。
// 双端金样 skin_golden.json 对拍:palette 容差 1e-5(JS f64 vs Rust 混合
// f32/f64 的超越函数边界)、蒙皮顶点 1e-4。
// 不支持面与 Rust 一致:CUBICSPLINE/morph weights 动画、normalized
// JOINTS/WEIGHTS、非三角形 mode 显式拒绝。

export class SkinError extends Error {
  constructor(message) {
    super(message);
    this.name = "SkinError";
  }
}

const COMPONENT_FLOAT = 5126;
const COMPONENT_UINT32 = 5125;
const COMPONENT_UINT16 = 5123;
const COMPONENT_UINT8 = 5121;

/** 解码 GLB 蒙皮场景。返回与 GlbRig 同构的摊位(字段 camelCase):
 * { primitives, skins, animations, roots, nodes }
 * - nodes[i] = { children, mesh, skin, translation, rotation(xyzw), scale }(f64)
 * - skins[i] = { joints, inverseBind(行主序 16×n) }
 * - animations[i] = { name, duration, channels: [{ node, path, times, values, step }] }
 * - primitives[i] = { positions, normals?, uvs?, indices, joints(u16×4), weights, baseColor?, baseColorTexture?, mrTexture?, skin } */
export function decodeGlbRig(bytes) {
  const { doc, bin, binDv } = parseGlbContainer(bytes);

  const sceneIdx = typeof doc.scene === "number" ? doc.scene : 0;
  const scene = doc.scenes?.[sceneIdx];
  if (!scene || !Array.isArray(scene.nodes)) throw new SkinError("场景缺少根节点表");
  const roots = (scene.nodes || []).map((v) => v | 0);
  const nodesJson = doc.nodes;
  if (!Array.isArray(nodesJson)) throw new SkinError("GLB 没有 nodes 表");
  const accessors = doc.accessors;
  const views = doc.bufferViews;
  if (!Array.isArray(accessors) || !Array.isArray(views)) {
    throw new SkinError("GLB 缺 accessors/bufferViews 表");
  }

  // 节点静态本地 TRS(matrix 形式按列主序 16 元分解)
  const nodes = nodesJson.map((node) => {
    let mt = [0, 0, 0], mq = [0, 0, 0, 1], ms = [1, 1, 1];
    if (Array.isArray(node.matrix)) {
      if (node.matrix.length !== 16) throw new SkinError("node matrix 必须为 16 元");
      const m = node.matrix.map(Number);
      const at = (r, c) => m[c * 4 + r]; // 列主序
      mt = [at(0, 3), at(1, 3), at(2, 3)];
      const sx = Math.hypot(at(0, 0), at(1, 0), at(2, 0));
      const sy = Math.hypot(at(0, 1), at(1, 1), at(2, 1));
      const sz = Math.hypot(at(0, 2), at(1, 2), at(2, 2));
      const rx = [at(0, 0) / sx, at(1, 0) / sx, at(2, 0) / sx];
      const ry = [at(0, 1) / sy, at(1, 1) / sy, at(2, 1) / sy];
      const rz = [at(0, 2) / sz, at(1, 2) / sz, at(2, 2) / sz];
      const trace = rx[0] + ry[1] + rz[2];
      if (trace > 0) {
        const s = Math.sqrt(1 + trace) * 2;
        mq = [(ry[2] - rz[1]) / s, (rz[0] - rx[2]) / s, (rx[1] - ry[0]) / s, 0.25 * s];
      } else if (rx[0] > ry[1] && rx[0] > rz[2]) {
        const s = Math.sqrt(1 + rx[0] - ry[1] - rz[2]) * 2;
        mq = [0.25 * s, (ry[0] + rx[1]) / s, (rz[0] + rx[2]) / s, (ry[2] - rz[1]) / s];
      } else if (ry[1] > rz[2]) {
        const s = Math.sqrt(1 + ry[1] - rx[0] - rz[2]) * 2;
        mq = [(ry[0] + rx[1]) / s, 0.25 * s, (rz[1] + ry[2]) / s, (rz[0] - rx[2]) / s];
      } else {
        const s = Math.sqrt(1 + rz[2] - rx[0] - ry[1]) * 2;
        mq = [(rz[0] + rx[2]) / s, (rz[1] + ry[2]) / s, 0.25 * s, (rx[1] - ry[0]) / s];
      }
      ms = [sx, sy, sz];
    }
    const get3 = (arr, def) =>
      Array.isArray(arr) ? [Number(arr[0] || 0), Number(arr[1] || 0), Number(arr[2] ?? 1)] : def;
    return {
      children: (node.children || []).map((v) => v | 0),
      mesh: typeof node.mesh === "number" ? node.mesh : null,
      skin: typeof node.skin === "number" ? node.skin : null,
      translation: get3(node.translation, mt),
      rotation: Array.isArray(node.rotation) && node.rotation.length === 4
        ? node.rotation.map(Number)
        : mq,
      scale: get3(node.scale, ms),
    };
  });

  // skins:joints + inverseBindMatrices(glTF MAT4 列主序 → 行主序 16)
  const skins = (doc.skins || []).map((skin) => {
    const joints = (skin.joints || []).map((v) => v | 0);
    const ibmIdx = typeof skin.inverseBindMatrices === "number" ? skin.inverseBindMatrices : 0;
    const r = locate(accessors, views, ibmIdx, COMPONENT_FLOAT, 4, 16, bin);
    const inverseBind = [];
    for (let i = 0; i < r.count; i++) {
      const at = r.start + i * r.stride;
      const m = new Array(16);
      for (let row = 0; row < 4; row++) {
        for (let col = 0; col < 4; col++) {
          m[row * 4 + col] = binDv.getFloat32(at + (col * 4 + row) * 4, true); // 列主序读
        }
      }
      inverseBind.push(m);
    }
    return { joints, inverseBind };
  });

  // animations:channels × samplers(LINEAR/STEP;CUBICSPLINE/weights 拒)
  const animations = (doc.animations || []).map((anim) => {
    const channels = [];
    let duration = 0;
    for (const ch of anim.channels || []) {
      const node = ch?.target?.node;
      if (typeof node !== "number") throw new SkinError("animation channel 缺 target.node");
      let step = false;
      switch (ch?.target?.path) {
        case "translation": case "rotation": case "scale": break;
        case "weights": continue; // 附加型表情通道跳过(与 Rust 同策略,骨骼动画不受影响)
        default: throw new SkinError("未知 animation path");
      }
      const sampler = (anim.samplers || [])[ch.sampler | 0];
      if (!sampler) throw new SkinError("animation channel 缺 sampler");
      switch (sampler.interpolation ?? "LINEAR") {
        case "LINEAR": break;
        case "STEP": step = true; break;
        default: throw new SkinError("CUBICSPLINE interpolation 不支持");
      }
      const times = readF32(accessors, views, sampler.input | 0, 1, bin, binDv)
        .map((v) => v[0]);
      const values = ch.target.path === "rotation"
        ? readVec4Raw(accessors, views, sampler.output | 0, bin, binDv)
        : readF32(accessors, views, sampler.output | 0, 3, bin, binDv)
            .map((v) => [v[0], v[1], v[2], 0]);
      if (times.length !== values.length) {
        throw new SkinError("animation sampler times/values count 不一致");
      }
      if (times.length) duration = Math.max(duration, times[times.length - 1]);
      if (ch.target.path === "rotation") {
        // 旋转值归一化(导出器常见非单位四元数)
        for (const v of values) {
          const l = Math.hypot(v[0], v[1], v[2], v[3]);
          if (l > 1.1920929e-7) { v[0] /= l; v[1] /= l; v[2] /= l; v[3] /= l; }
        }
      }
      channels.push({ node: node | 0, path: ch.target.path, times, values, step });
    }
    return { name: anim.name ?? null, duration, channels };
  });

  // primitives:bind 空间顶点 + JOINTS/WEIGHTS + 材质纹理字节摊位
  const meshes = doc.meshes;
  if (!Array.isArray(meshes)) throw new SkinError("GLB 缺 meshes 表");
  const primitives = [];
  const textureCache = new Map();
  let meshIdx = 0;
  for (const mesh of meshes) {
    for (const primitive of mesh.primitives || []) {
      if (typeof primitive.mode === "number" && primitive.mode !== 4) {
        throw new SkinError("非三角形 primitive mode");
      }
      const attrs = primitive.attributes || {};
      const attr = (name) => (typeof attrs[name] === "number" ? attrs[name] : null);
      const posIdx = attr("POSITION");
      if (posIdx == null) throw new SkinError("primitive 缺 POSITION");
      const positions = readF32(accessors, views, posIdx, 3, bin, binDv);
      const normals = attr("NORMAL") != null
        ? readF32(accessors, views, attr("NORMAL"), 3, bin, binDv) : null;
      const uvs = attr("TEXCOORD_0") != null
        ? readF32(accessors, views, attr("TEXCOORD_0"), 2, bin, binDv) : null;
      let joints = null;
      if (attr("JOINTS_0") != null) {
        const idx = attr("JOINTS_0");
        const acc = accessors[idx];
        if (acc.normalized === true) throw new SkinError("normalized JOINTS/WEIGHTS 不支持");
        const component = acc.componentType;
        if (component !== COMPONENT_UINT8 && component !== COMPONENT_UINT16) {
          throw new SkinError(`JOINTS_0 componentType ${component} 不支持`);
        }
        const r = locate(accessors, views, idx, component, component === COMPONENT_UINT8 ? 1 : 2, 4, bin);
        joints = [];
        for (let i = 0; i < r.count; i++) {
          const at = r.start + i * r.stride;
          const row = new Array(4);
          for (let k = 0; k < 4; k++) {
            row[k] = component === COMPONENT_UINT8 ? bin[at + k * 1] : binDv.getUint16(at + k * 2, true);
          }
          joints.push(row);
        }
      }
      let weights = null;
      if (attr("WEIGHTS_0") != null) {
        const idx = attr("WEIGHTS_0");
        if (accessors[idx].normalized === true) throw new SkinError("normalized JOINTS/WEIGHTS 不支持");
        weights = readF32(accessors, views, idx, 4, bin, binDv);
      }
      // 材质摊位(与 decode_glb_rig 同面:baseColorFactor/baseColorTexture/MR)
      const mat = (doc.materials || [])[primitive.material ?? 0] ?? null;
      const pbr = mat?.pbrMetallicRoughness ?? {};
      const baseColor = Array.isArray(pbr.baseColorFactor)
        ? pbr.baseColorFactor.map(Number) : null;
      const texInfo = (texIdx) => {
        const tex = (doc.textures || [])[texIdx];
        const imgIdx = tex?.source;
        const image = (doc.images || [])[imgIdx];
        if (!image) return null;
        const bvIdx = image.bufferView;
        if (typeof bvIdx !== "number") return null; // 外部 URI 不支持
        const view = views[bvIdx];
        const bytes = bin.subarray(view.byteOffset | 0, (view.byteOffset | 0) + (view.byteLength | 0));
        const key = bvIdx;
        if (!textureCache.has(key)) {
          textureCache.set(key, { bytes: new Uint8Array(bytes), mime: image.mimeType ?? "image/png" });
        }
        return textureCache.get(key);
      };
      const baseColorTexture = typeof pbr.baseColorTexture?.index === "number"
        ? texInfo(pbr.baseColorTexture.index) : null;
      const mrTexture = typeof pbr.metallicRoughnessTexture?.index === "number"
        ? texInfo(pbr.metallicRoughnessTexture.index) : null;
      const indices = attr("indices") != null
        ? readIndices(accessors, views, attr("indices"), bin, binDv) : [];
      primitives.push({
        mesh: meshIdx,
        positions, normals, uvs, indices, joints, weights,
        baseColor,
        metallic: typeof pbr.metallicFactor === "number" ? pbr.metallicFactor : 1,
        roughness: typeof pbr.roughnessFactor === "number" ? pbr.roughnessFactor : 1,
        baseColorTexture, mrTexture,
        skin: typeof primitive.skin === "number" ? primitive.skin
          : (nodes.find((n) => n.mesh != null)?.skin ?? 0),
      });
    }
    meshIdx++;
  }

  return { primitives, skins, animations, roots, nodes };
}

/** 采样通道在时间 t 的值。与 sample_channel 同式:LINEAR lerp/短弧 slerp、
 * STEP 保持前值、越界钳制两端。返回 [x,y,z,w](rotation 为 xyzw 四元数)。 */
export function sampleChannel(ch, t) {
  const times = ch.times;
  if (!times.length) return [0, 0, 0, 0];
  if (t <= times[0]) return ch.values[0];
  if (t >= times[times.length - 1]) return ch.values[times.length - 1];
  let k = 0;
  while (k + 1 < times.length && times[k + 1] <= t) k++;
  const t0 = times[k], t1 = times[k + 1];
  const v0 = ch.values[k], v1 = ch.values[k + 1];
  if (ch.step || t1 <= t0) return v0;
  const a = (t - t0) / (t1 - t0);
  if (ch.path === "rotation") {
    let [x1, y1, z1, w1] = v1;
    const dot = v0[0] * x1 + v0[1] * y1 + v0[2] * z1 + v0[3] * w1;
    if (dot < 0) { x1 = -x1; y1 = -y1; z1 = -z1; w1 = -w1; }
    const dot2 = v0[0] * x1 + v0[1] * y1 + v0[2] * z1 + v0[3] * w1;
    const theta = Math.acos(Math.min(1, Math.max(-1, dot2)));
    if (theta < 1e-6) return v0;
    const s0 = Math.sin((1 - a) * theta) / Math.sin(theta);
    const s1 = Math.sin(a * theta) / Math.sin(theta);
    return [
      s0 * v0[0] + s1 * x1, s0 * v0[1] + s1 * y1,
      s0 * v0[2] + s1 * z1, s0 * v0[3] + s1 * w1,
    ];
  }
  return [
    v0[0] + (v1[0] - v0[0]) * a,
    v0[1] + (v1[1] - v0[1]) * a,
    v0[2] + (v1[2] - v0[2]) * a,
    1.0,
  ];
}

const IDENTITY16 = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];

function mul16(a, b) {
  const out = new Array(16);
  for (let r = 0; r < 4; r++) {
    for (let c = 0; c < 4; c++) {
      out[r * 4 + c] =
        a[r * 4] * b[c] + a[r * 4 + 1] * b[4 + c] + a[r * 4 + 2] * b[8 + c] + a[r * 4 + 3] * b[12 + c];
    }
  }
  return out;
}

function quatToMat3(q) {
  const [x, y, z, w] = q;
  return [
    [1 - 2 * (y * y + z * z), 2 * (x * y - w * z), 2 * (x * z + w * y)],
    [2 * (x * y + w * z), 1 - 2 * (x * x + z * z), 2 * (y * z - w * x)],
    [2 * (x * z - w * y), 2 * (y * z + w * x), 1 - 2 * (x * x + y * y)],
  ];
}

/** 采样时间 t 的全部节点世界矩阵(行主序 16,下标=节点号;不可达节点为 null)。
 * jointMatricesAt 的遍历基底,亦供刚体子件(挂关节节点上的眼睛等)随骨骼取
 * 每帧模型矩阵。 */
export function nodeWorldsAt(rig, animIdx, t) {
  const override = new Map();
  const anim = animIdx != null ? rig.animations[animIdx] : null;
  if (anim) {
    for (const ch of anim.channels) {
      const rest = rig.nodes[ch.node];
      if (!override.has(ch.node)) override.set(ch.node, [rest.translation, rest.rotation, rest.scale]);
      const v = sampleChannel(ch, t);
      const entry = override.get(ch.node);
      if (ch.path === "translation") entry[0] = [v[0], v[1], v[2]];
      else if (ch.path === "rotation") entry[1] = [v[0], v[1], v[2], v[3]];
      else entry[2] = [v[0], v[1], v[2]];
    }
  }
  const world = rig.nodes.map(() => null);
  const queue = rig.roots.map((i) => [i, IDENTITY16]);
  while (queue.length) {
    const [ni, parent] = queue.shift();
    if (world[ni] !== null) continue; // 共享节点只展开一次
    const rest = rig.nodes[ni];
    const [tr, qr, sr] = override.get(ni) ?? [rest.translation, rest.rotation, rest.scale];
    const rot = quatToMat3(qr);
    const local = [
      rot[0][0] * sr[0], rot[0][1] * sr[1], rot[0][2] * sr[2], tr[0],
      rot[1][0] * sr[0], rot[1][1] * sr[1], rot[1][2] * sr[2], tr[1],
      rot[2][0] * sr[0], rot[2][1] * sr[1], rot[2][2] * sr[2], tr[2],
      0, 0, 0, 1,
    ];
    const w = mul16(parent, local);
    world[ni] = w;
    for (const c of rest.children) queue.push([c, w]);
  }
  return world;
}

/** 采样时间 t 的全部 skin 关节面板。返回 skins×joints×行主序 16:
 * [skinIdx][jointIdx] = number[16](与 joint_matrices_at 同构)。
 * 动画未覆盖分量回退节点静态 TRS;animIdx=null 即静态姿势。 */
export function jointMatricesAt(rig, animIdx, t) {
  const world = nodeWorldsAt(rig, animIdx, t);
  return rig.skins.map((skin) =>
    skin.joints.map((j, i) => {
      const w = world[j] ?? IDENTITY16;
      const ibm = skin.inverseBind[i] ?? IDENTITY16;
      return mul16(w, ibm);
    }),
  );
}

/** CPU 蒙皮:与 skin_vertices 同式(Σ w·M·p,行主序 16)。 */
export function skinVertices(prim, palette) {
  return prim.positions.map((p, i) => {
    const j = prim.joints[i];
    const w = prim.weights[i];
    const acc = [0, 0, 0];
    for (let k = 0; k < 4; k++) {
      const wt = w[k];
      if (wt <= 0) continue;
      const m = palette[j[k]] ?? IDENTITY16;
      for (let r = 0; r < 3; r++) {
        acc[r] += wt * (m[r * 4] * p[0] + m[r * 4 + 1] * p[1] + m[r * 4 + 2] * p[2] + m[r * 4 + 3]);
      }
    }
    return acc;
  });
}

// ---------- 内部:GLB 容器与 accessor 定位(与 gltf.js 同式,自包含) ----------

function parseGlbContainer(bytes) {
  if (bytes.length < 12 || bytes[0] !== 0x67 || bytes[1] !== 0x6c || bytes[2] !== 0x54 || bytes[3] !== 0x46) {
    throw new SkinError("不是 GLB 文件(缺 glTF 魔数)");
  }
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (dv.getUint32(4, true) !== 2) throw new SkinError("仅支持 GLB 版本 2");
  const total = dv.getUint32(8, true);
  if (total !== bytes.length) throw new SkinError("GLB 总长与文件不符");
  if (bytes.length < 20 || bytes[16] !== 0x4a || bytes[17] !== 0x53 || bytes[18] !== 0x4f || bytes[19] !== 0x4e) {
    throw new SkinError("GLB 首 chunk 不是 JSON");
  }
  const jsonLen = dv.getUint32(12, true);
  const jsonEnd = 20 + jsonLen;
  if (jsonLen === 0 || jsonEnd > bytes.length) throw new SkinError("JSON chunk 截断");
  const doc = JSON.parse(new TextDecoder().decode(bytes.subarray(20, jsonEnd)));
  let bin = new Uint8Array(0);
  let binDv = new DataView(bytes.buffer, bytes.byteOffset, 0);
  if (jsonEnd + 8 <= bytes.length) {
    const binLen = dv.getUint32(jsonEnd, true);
    const binStart = jsonEnd + 8;
    const type = bytes.subarray(jsonEnd + 4, jsonEnd + 8);
    if (type[0] !== 0x42 || type[1] !== 0x49 || type[2] !== 0x4e || type[3] !== 0) {
      throw new SkinError("第二 chunk 不是 BIN");
    }
    if (binStart + binLen > bytes.length) throw new SkinError("BIN chunk 截断");
    bin = bytes.subarray(binStart, binStart + binLen);
    binDv = new DataView(bytes.buffer, bytes.byteOffset + binStart, binLen);
  }
  return { doc, bin, binDv };
}

function locate(accessors, views, accIdx, component, compBytes, arity, bin) {
  const acc = accessors[accIdx];
  if (!acc) throw new SkinError(`accessor ${accIdx} 缺失`);
  if ((acc.componentType ?? 0) !== component) {
    throw new SkinError(`accessor ${accIdx} componentType 是 ${acc.componentType},期望 ${component}`);
  }
  const count = acc.count ?? 0;
  if (count <= 0) throw new SkinError(`accessor ${accIdx} count 为 0`);
  const viewIdx = acc.bufferView ?? 0;
  const view = views[viewIdx];
  if (!view) throw new SkinError(`accessor ${accIdx} 引用的 bufferView ${viewIdx} 缺失`);
  const natural = compBytes * arity;
  const stride = (view.byteStride ?? natural) > natural ? view.byteStride : natural;
  const accOff = acc.byteOffset ?? 0;
  const viewOff = view.byteOffset ?? 0;
  const viewLen = view.byteLength ?? 0;
  const tail = (count - 1) * stride + natural;
  if (accOff + tail > viewLen || viewOff + viewLen > bin.length) {
    throw new SkinError(`accessor ${accIdx} 越界`);
  }
  return { count, start: viewOff + accOff, stride, natural };
}

function readF32(accessors, views, accIdx, arity, bin, binDv) {
  const r = locate(accessors, views, accIdx, COMPONENT_FLOAT, 4, arity, bin);
  const rows = [];
  for (let i = 0; i < r.count; i++) {
    const at = r.start + i * r.stride;
    const row = [];
    for (let k = 0; k < arity; k++) row.push(binDv.getFloat32(at + k * 4, true));
    rows.push(row);
  }
  return rows;
}

function readVec4Raw(accessors, views, accIdx, bin, binDv) {
  const r = locate(accessors, views, accIdx, COMPONENT_FLOAT, 4, 4, bin);
  const rows = [];
  for (let i = 0; i < r.count; i++) {
    const at = r.start + i * r.stride;
    rows.push([
      binDv.getFloat32(at, true), binDv.getFloat32(at + 4, true),
      binDv.getFloat32(at + 8, true), binDv.getFloat32(at + 12, true),
    ]);
  }
  return rows;
}

function readIndices(accessors, views, accIdx, bin, binDv) {
  const acc = accessors[accIdx];
  if (!acc) throw new SkinError(`accessor ${accIdx} 缺失`);
  const component = acc.componentType ?? 0;
  if (component !== COMPONENT_UINT32 && component !== COMPONENT_UINT16 && component !== COMPONENT_UINT8) {
    throw new SkinError(`索引 accessor ${accIdx} componentType ${component} 不支持`);
  }
  const compBytes = component === COMPONENT_UINT32 ? 4 : component === COMPONENT_UINT16 ? 2 : 1;
  const r = locate(accessors, views, accIdx, component, compBytes, 1, bin);
  const out = [];
  for (let i = 0; i < r.count; i++) {
    const at = r.start + i * r.stride;
    out.push(component === COMPONENT_UINT32 ? binDv.getUint32(at, true)
      : component === COMPONENT_UINT16 ? binDv.getUint16(at, true) : bin[at]);
  }
  return out;
}
