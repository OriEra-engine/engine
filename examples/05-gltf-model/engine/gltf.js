// OriEra L4 glTF JS 镜像——与 crates/oriera-asset/src/glb.rs decode_glb_scene 公式同源
// (#152 第三刀,demo 05 摘掉 three.js)。
// 覆盖:GLB 容器、accessor(byteOffset+stride)、场景遍历(TRS/matrix 组合)、材质与
// 纹理字节摊位。GPU 渲染与 PNG 解码不在镜像内(浏览器原生 ImageBitmap 解码;
// 双端金样 gltf_golden.json 对拍:索引/材质精确,变换后顶点/法线容差 1e-4——
// f64 JS vs f32 Rust 的累计漂移,与 M2 的容差域约定同一哲学)。

export class GltfError extends Error {
  constructor(message) {
    super(message);
    this.name = "GltfError";
  }
}

const COMPONENT_FLOAT = 5126;
const COMPONENT_UINT32 = 5125;
const COMPONENT_UINT16 = 5123;

/** 解码 GLB 场景(全部 primitive,世界坐标)。
 * 与 decode_glb_scene 同支持面/拒绝面:vertex 首个 element 的老接口不存在——
 * 这里走 scene 图;skins/Draco/specularGlossiness/JPEG/外部 URI/节点环显式拒绝;
 * accessor byteOffset 与交错 stride 支持。 */
export function decodeGlbScene(bytes) {
  if (bytes.length < 12 || bytes[0] !== 0x67 || bytes[1] !== 0x6c || bytes[2] !== 0x54 || bytes[3] !== 0x46) {
    throw new GltfError("不是 GLB 文件(缺 glTF 魔数)");
  }
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (dv.getUint32(4, true) !== 2) throw new GltfError("仅支持 GLB 版本 2");
  const total = dv.getUint32(8, true);
  if (total !== bytes.length) throw new GltfError("GLB 总长与文件不符");

  if (bytes.length < 20 || bytes[16] !== 0x4a || bytes[17] !== 0x53 || bytes[18] !== 0x4f || bytes[19] !== 0x4e) {
    throw new GltfError("GLB 首_chunk 不是 JSON");
  }
  const jsonLen = dv.getUint32(12, true);
  const jsonEnd = 20 + jsonLen;
  if (jsonLen === 0 || jsonEnd > bytes.length) throw new GltfError("JSON chunk 截断");
  const doc = JSON.parse(new TextDecoder().decode(bytes.subarray(20, jsonEnd)));

  let bin = new Uint8Array(0);
  let binDv = new DataView(bytes.buffer, bytes.byteOffset, 0);
  if (jsonEnd + 8 <= bytes.length) {
    const binLen = dv.getUint32(jsonEnd, true);
    const binStart = jsonEnd + 8;
    const type = bytes.subarray(jsonEnd + 4, jsonEnd + 8);
    if (type[0] !== 0x42 || type[1] !== 0x49 || type[2] !== 0x4e || type[3] !== 0) {
      throw new GltfError("第二 chunk 不是 BIN");
    }
    if (binStart + binLen > bytes.length) throw new GltfError("BIN chunk 截断");
    bin = bytes.subarray(binStart, binStart + binLen);
    // BIN 专属 DataView:readVec/readIndices 的 start 相对 bin,不能整文件混用
    binDv = new DataView(bytes.buffer, bytes.byteOffset + binStart, binLen);
  }

  // 场景根节点
  const sceneIdx = typeof doc.scene === "number" ? doc.scene : 0;
  const scene = doc.scenes?.[sceneIdx];
  if (!scene || !Array.isArray(scene.nodes)) throw new GltfError("场景缺少根节点表");
  const nodes = doc.nodes;
  if (!Array.isArray(nodes)) throw new GltfError("GLB 没有 nodes 表");

  // accessor 定位(byteOffset+stride 支持,全 checked;与 locate_scene 同式)
  const locate = (accIdx, component, compBytes, arity) => {
    const acc = doc.accessors?.[accIdx];
    if (!acc) throw new GltfError(`accessor ${accIdx} 缺失`);
    if ((acc.componentType ?? 0) !== component) {
      throw new GltfError(`accessor ${accIdx} 的 componentType 是 ${acc.componentType},期望 ${component}`);
    }
    const count = acc.count ?? 0;
    if (count <= 0) throw new GltfError(`accessor ${accIdx} count 为 0`);
    const viewIdx = acc.bufferView ?? 0;
    const view = doc.bufferViews?.[viewIdx];
    if (!view) throw new GltfError(`accessor ${accIdx} 引用的 bufferView ${viewIdx} 缺失`);
    const natural = compBytes * arity;
    const stride = (view.byteStride ?? natural) > natural ? view.byteStride : natural;
    const accOff = acc.byteOffset ?? 0;
    const viewOff = view.byteOffset ?? 0;
    const viewLen = view.byteLength ?? 0;
    const tail = (count - 1) * stride + natural;
    if (accOff + tail > viewLen || viewOff + viewLen > bin.length) {
      throw new GltfError(`accessor ${accIdx} 越界:需要 ${accOff + tail} 字节,视图只剩 ${viewLen}`);
    }
    return { count, start: viewOff + accOff, stride, natural, accOff, viewLen };
  };
  const readVec = (accIdx, component, compBytes, arity) => {
    const r = locate(accIdx, component, compBytes, arity);
    const rows = [];
    for (let i = 0; i < r.count; i++) {
      const at = r.start + i * r.stride;
      const row = [];
      for (let k = 0; k < arity; k++) {
        row.push(binDv.getFloat32(at + k * 4, true));
      }
      rows.push(row);
    }
    return rows;
  };
  const readIndices = (accIdx) => {
    const acc = doc.accessors?.[accIdx];
    if (!acc) throw new GltfError(`accessor ${accIdx} 缺失`);
    const component = acc.componentType ?? 0;
    if (component !== COMPONENT_UINT32 && component !== COMPONENT_UINT16) {
      throw new GltfError(`索引 accessor ${accIdx} 的 componentType ${component} 不支持`);
    }
    const r = locate(accIdx, component, component === COMPONENT_UINT32 ? 4 : 2, 1);
    const out = [];
    for (let i = 0; i < r.count; i++) {
      const at = r.start + i * r.stride;
      out.push(component === COMPONENT_UINT32 ? binDv.getUint32(at, true) : binDv.getUint16(at, true));
    }
    return out;
  };

  // 场景遍历:FIFO(与 Rust 实现同序),环拒绝,深度上限 64
  const compose = (parent, local) => {
    const out = [
      [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0],
    ];
    for (let r = 0; r < 4; r++) {
      for (let c = 0; c < 4; c++) {
        out[r][c] = parent[r][0] * local[0][c] + parent[r][1] * local[1][c] +
                    parent[r][2] * local[2][c] + parent[r][3] * local[3][c];
      }
    }
    return out;
  };
  const nodeLocal = (node) => {
    if (Array.isArray(node.matrix)) {
      if (node.matrix.length !== 16) throw new GltfError("node matrix 必须 16 元素");
      // glTF 列主序 → 行主序
      const m = [[0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]];
      for (let r = 0; r < 4; r++) {
        for (let c = 0; c < 4; c++) {
          m[r][c] = node.matrix[c * 4 + r];
        }
      }
      return m;
    }
    const s = Array.isArray(node.scale) ? node.scale : [1, 1, 1];
    const q = Array.isArray(node.rotation) ? node.rotation : [0, 0, 0, 1];
    const t = Array.isArray(node.translation) ? node.translation : [0, 0, 0];
    const [x, y, z, w] = q;
    const rot = [
      [1 - 2 * (y * y + z * z), 2 * (x * y - w * z), 2 * (x * z + w * y)],
      [2 * (x * y + w * z), 1 - 2 * (x * x + z * z), 2 * (y * z - w * x)],
      [2 * (x * z - w * y), 2 * (y * z + w * x), 1 - 2 * (x * x + y * y)],
    ];
    const m = [
      [rot[0][0] * s[0], rot[0][1] * s[1], rot[0][2] * s[2], t[0]],
      [rot[1][0] * s[0], rot[1][1] * s[1], rot[1][2] * s[2], t[1]],
      [rot[2][0] * s[0], rot[2][1] * s[1], rot[2][2] * s[2], t[2]],
      [0, 0, 0, 1],
    ];
    return m;
  };

  const transform = (world, p) => [
    world[0][0] * p[0] + world[0][1] * p[1] + world[0][2] * p[2] + world[0][3],
    world[1][0] * p[0] + world[1][1] * p[1] + world[1][2] * p[2] + world[1][3],
    world[2][0] * p[0] + world[2][1] * p[1] + world[2][2] * p[2] + world[2][3],
  ];
  const normalMatrix = (world) => {
    const a = world[0][0], b = world[0][1], c = world[0][2];
    const d = world[1][0], e = world[1][1], f = world[1][2];
    const g = world[2][0], h = world[2][1], i = world[2][2];
    const det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if (!Number.isFinite(det) || det === 0) return null; // 奇异 → 调用方拒绝
    // 逆转置 = 伴随/det 的转置写法(与 Rust normal_matrix 同式)
    return [
      [(e * i - f * h) / det, (c * h - b * i) / det, (b * f - c * e) / det],
      [(f * g - d * i) / det, (a * i - c * g) / det, (c * d - a * f) / det],
      [(d * h - e * g) / det, (b * g - a * h) / det, (a * e - b * d) / det],
    ];
  };

  const primitives = [];
  const visited = new Set();
  const queue = scene.nodes
    .filter((n) => Number.isInteger(n))
    .map((n) => [n, [[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]], 0]);
  const IDENT3 = [[1, 0, 0], [0, 1, 0], [0, 0, 1]];
  while (queue.length > 0) {
    const [ni, world, depth] = queue.shift();
    if (depth > 64) throw new GltfError("节点图深度超过 64");
    if (visited.has(ni)) throw new GltfError("节点图有环");
    visited.add(ni);
    const node = nodes[ni];
    if (!node) throw new GltfError(`节点 ${ni} 缺失`);
    const local = nodeLocal(node);
    const w = compose(world, local);
    for (const c of node.children ?? []) {
      if (!Number.isInteger(c)) throw new GltfError("children 里有非整数下标");
      queue.push([c, w, depth + 1]);
    }
    if (typeof node.mesh !== "number") continue;
    if (node.skin !== undefined) throw new GltfError("蒙皮网格暂不支持");
    const mesh = doc.meshes?.[node.mesh];
    if (!mesh) throw new GltfError(`mesh ${node.mesh} 缺失`);
    for (const primitive of mesh.primitives ?? []) {
      if ((primitive.mode ?? 4) !== 4) throw new GltfError("仅支持三角形图元(mode=4)");
      if (primitive.extensions?.KHR_draco_mesh_compression) {
        throw new GltfError("Draco 压缩网格暂不支持");
      }
      const attrs = primitive.attributes ?? {};
      if (typeof attrs.POSITION !== "number") throw new GltfError("primitive 缺 POSITION");
      const positions = readVec(attrs.POSITION, COMPONENT_FLOAT, 4, 3);
      const normals = typeof attrs.NORMAL === "number"
        ? readVec(attrs.NORMAL, COMPONENT_FLOAT, 4, 3)
        : null;
      if (normals && normals.length !== positions.length) {
        throw new GltfError("NORMAL 数量与 POSITION 不一致");
      }
      const uvs = typeof attrs.TEXCOORD_0 === "number"
        ? readVec(attrs.TEXCOORD_0, COMPONENT_FLOAT, 4, 2)
        : null;
      const indices = typeof primitive.indices === "number" ? readIndices(primitive.indices) : [];
      for (const bad of indices) {
        if (bad >= positions.length) {
          throw new GltfError(`索引 ${bad} 超出顶点数 ${positions.length}`);
        }
      }
      // 材质摊位(默认值同规范;扩展只拒替换型)
      let baseColor = null, metallic = 1, roughness = 1, texture = null;
      if (typeof primitive.material === "number") {
        const mat = doc.materials?.[primitive.material];
        if (!mat) throw new GltfError(`材质 ${primitive.material} 缺失`);
        if (mat.extensions?.KHR_materials_pbrSpecularGlossiness) {
          throw new GltfError("KHR_materials_pbrSpecularGlossiness 替换了 baseColor 模型,暂不支持");
        }
        const pbr = mat.pbrMetallicRoughness ?? {};
        if (Array.isArray(pbr.baseColorFactor) && pbr.baseColorFactor.length === 4) {
          baseColor = pbr.baseColorFactor;
        }
        metallic = typeof pbr.metallicFactor === "number" ? pbr.metallicFactor : 1;
        roughness = typeof pbr.roughnessFactor === "number" ? pbr.roughnessFactor : 1;
        const texIdx = pbr.baseColorTexture?.index;
        if (typeof texIdx === "number") {
          const src = doc.textures?.[texIdx]?.source;
          const image = doc.images?.[src];
          if (image === undefined || image === null) throw new GltfError("纹理引用的 image 缺失");
          if (image.uri !== undefined) throw new GltfError("外部图片文件暂不支持(GLB 内嵌才行)");
          if (image.mimeType !== "image/png") throw new GltfError("只支持 PNG 纹理");
          const view = doc.bufferViews?.[image.bufferView];
          if (!view) throw new GltfError("纹理引用的 bufferView 缺失");
          texture = { mime: image.mimeType, bytes: bin.subarray(view.byteOffset ?? 0, (view.byteOffset ?? 0) + (view.byteLength ?? 0)) };
        }
      }
      // 世界变换
      const nmat = normals ? normalMatrix(w) : IDENT3_COPY();
      if (normals && !nmat) throw new GltfError("节点变换退化(行列式为零)");
      const nm = nmat ?? IDENT3_COPY();
      const wpos = positions.map((p) => transform(w, p));
      const wnorm = normals
        ? normals.map((n) => {
            const t = [
              nm[0][0] * n[0] + nm[0][1] * n[1] + nm[0][2] * n[2],
              nm[1][0] * n[0] + nm[1][1] * n[1] + nm[1][2] * n[2],
              nm[2][0] * n[0] + nm[2][1] * n[1] + nm[2][2] * n[2],
            ];
            const len = Math.hypot(t[0], t[1], t[2]);
            return len > 2.8e-45 ? [t[0] / len, t[1] / len, t[2] / len] : [0, 0, 1];
          })
        : null;
      primitives.push({
        positions: wpos,
        normals: wnorm,
        uvs,
        indices,
        baseColor,
        metallic,
        roughness,
        texture,
      });
    }
  }
  return { primitives };
}

const IDENT3_COPY = () => [[1, 0, 0], [0, 1, 0], [0, 0, 1]];

/** DC 项基色(仅在有 baseColorFactor 时;着色器里再乘纹理)。 */
export function primitiveBaseColor(primitive) {
  return primitive.baseColor ?? [1, 1, 1, 1];
}
