//! urdf_mesh.js 测试——与 Rust meshres.rs 同一套判定规则的双端 parity 守卫
//! （URDF-IMPORT.md U3）。
//! 运行：node --test examples/15-embodied-sim/engine/urdf_mesh.test.mjs

import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { parseStl } from "./urdf_mesh.js";

// ── 合成工具：四面体二进制 STL（与 Rust meshres::tests 同一 fixture 语义）──

const TETRA = [
  [[0, 0, 0], [0, 1, 0], [1, 0, 0]],
  [[0, 0, 0], [1, 0, 0], [0, 0, 1]],
  [[0, 0, 0], [0, 0, 1], [0, 1, 0]],
  [[1, 0, 0], [0, 1, 0], [0, 0, 1]],
];

// 80B header（可选 "solid" 伪装字样）+ LE u32 数 + 50B/面（法线占位零）。
function buildBinaryStl(tris, { solidHeader = false } = {}) {
  const out = new Uint8Array(84 + 50 * tris.length);
  if (solidHeader) new TextEncoder().encodeInto("solid", out.subarray(0, 5));
  const dv = new DataView(out.buffer);
  dv.setUint32(80, tris.length, true);
  let off = 84;
  for (const t of tris) {
    off += 12; // facet normal 占位 (0,0,0)
    for (const v of t) {
      dv.setFloat32(off, v[0], true);
      dv.setFloat32(off + 4, v[1], true);
      dv.setFloat32(off + 8, v[2], true);
      off += 12;
    }
    dv.setUint16(off, 0, true); // attribute byte count
    off += 2;
  }
  return out.buffer;
}

describe("parseStl · 二进制", () => {
  it("四面体 roundtrip：36 位置浮点 + 面法线随行", () => {
    const { positions, normals } = parseStl(buildBinaryStl(TETRA));
    assert.equal(positions.length, 4 * 9);
    assert.equal(normals.length, 4 * 9);
    assert.deepEqual([...positions.slice(0, 9)], [...TETRA[0].flat()]);
  });
  it("接受 Uint8Array 视图（byteOffset 对齐）", () => {
    const padded = new Uint8Array(8 + 84 + 50 * 4);
    padded.set(new Uint8Array(buildBinaryStl(TETRA)), 8);
    const { positions } = parseStl(padded.subarray(8));
    assert.equal(positions.length, 36);
  });
  it("solid 开头的二进制按二进制走（ASCII 头伪装防线）", () => {
    const { positions } = parseStl(buildBinaryStl(TETRA, { solidHeader: true }));
    assert.equal(positions.length, 36, "长度精确命中 84+50×n 公式必须按二进制解析");
  });
  it("小端字节序：顶点坐标按 LE 读出原值", () => {
    const { positions } = parseStl(buildBinaryStl(TETRA));
    assert.equal(positions[1], 0); // (0,0,0)
    // TETRA[0] 第二顶点 (0,1,0)：LE 读出 1（按 BE 反读会是 ~1e-41 的垃圾）
    assert.equal(positions[3 + 1], 1);
  });
});

describe("parseStl · ASCII", () => {
  const enc = (s) => new TextEncoder().encode(s); // 字符串 → 字节（parseStl 只吃 buffer）
  const ASCII = `solid quad
  facet normal 0 0 -1
    outer loop
      vertex 0 0 0
      vertex 0 1 0
      vertex 1 0 0
    endloop
  endfacet
  facet normal 0.5 0.5 0.7
    outer loop
      vertex 1 1 0
      vertex 1 0 0
      vertex 0 0 0
    endloop
  endfacet
endsolid quad
`;
  it("两三角形展开为非索引 18 浮点，面法线重复 3 次", () => {
    const { positions, normals } = parseStl(enc(ASCII));
    assert.equal(positions.length, 18);
    assert.equal(normals.length, 18);
    // 第二面的法线 (0.5, 0.5, 0.7) 重复 3 次（f32 精度内逐项对照）
    for (const off of [9, 15]) {
      assert.ok(Math.abs(normals[off] - 0.5) < 1e-6);
      assert.ok(Math.abs(normals[off + 1] - 0.5) < 1e-6);
      assert.ok(Math.abs(normals[off + 2] - 0.7) < 1e-6);
    }
  });
  it("指数记数与缺 endsolid 均可（容错）", () => {
    const { positions } = parseStl(enc(`solid x
facet normal 0 0 1
vertex 1.0e-1 0.0 0.0
vertex 0.0 2.5E-1 0.0
vertex 0.0 0.0 3.0
`));
    assert.equal(positions.length, 9);
    assert.ok(Math.abs(positions[0] - 0.1) < 1e-6, `positions[0]=${positions[0]}`); // f32 精度
    assert.equal(positions[8], 3);
  });
  it("无 vertex / 顶点数不成面 → 显式抛错", () => {
    assert.throws(() => parseStl(enc("solid x\nendsolid x\n")), /无 vertex/);
    assert.throws(() => parseStl(enc("solid x\nvertex 1 2\n")), /3 的倍数|非法/);
  });
  it("非法数字字段 → 显式抛错", () => {
    assert.throws(() => parseStl(enc("solid x\nvertex 1 abc 3\nvertex 0 0 0\nvertex 0 0 0\n")), /非法/);
  });
});

describe("parseStl · 判定规则（双端同源）", () => {
  it("双判据皆失 → 抛错并给出两个判据", () => {
    assert.throws(() => parseStl(new Uint8Array([0, 1, 2, 3, 4])), /solid.*二进制|二进制.*solid/s);
  });
  it("截断的二进制（长度不吻合公式且非 solid）→ 抛错", () => {
    const full = new Uint8Array(buildBinaryStl(TETRA));
    assert.throws(() => parseStl(full.subarray(0, full.length - 10)), /不吻合二进制公式/);
  });
  it("空输入 → 抛错", () => {
    assert.throws(() => parseStl(new ArrayBuffer(0)));
  });
});
