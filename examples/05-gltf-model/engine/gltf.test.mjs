// 双端金样对拍(#152 第三刀):与 crates/oriera-asset/tests/cross_language_gltf.rs
// 同读 contracts/golden/gltf_golden.json——索引/材质摊位精确,变换后顶点/法线
// 容差 1e-4(金样 meta 有完整容差域约定)。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { decodeGlbScene, primitiveBaseColor } from "./gltf.js";

const here = dirname(fileURLToPath(import.meta.url));
const fixture = JSON.parse(
  readFileSync(join(here, "../../../contracts/golden/gltf_golden.json"), "utf8"),
);
const glb = Uint8Array.from(Buffer.from(fixture.glb_base64, "base64"));
const tol = fixture.meta.tolerances;

test("scene decode: primitive count, material stall, texture stall exact", () => {
  const { primitives } = decodeGlbScene(glb);
  assert.equal(primitives.length, fixture.expect.primitive_count);
  assert.equal(primitives.length, fixture.expect.primitives.length);
  for (let i = 0; i < primitives.length; i++) {
    const js = primitives[i];
    const want = fixture.expect.primitives[i];
    assert.deepEqual(js.indices, want.indices, `prim ${i} indices exact`);
    // base_color 经 f64(JS)→f32(Rust) 序列化,f32 恢复后容差对拍
    // (金样 meta 约定:位型精确仅限字节级透传字段如索引/纹理)
    const bc = js.baseColor ?? [1, 1, 1, 1];
    const bw = want.base_color ?? [1, 1, 1, 1];
    for (let k = 0; k < 4; k++) {
      assert.ok(Math.abs(bc[k] - bw[k]) <= 1e-6, `prim ${i} base_color[${k}]`);
    }
    assert.ok(Math.abs(js.metallic - want.metallic) <= 1e-6, `prim ${i} metallic`);
    assert.ok(Math.abs(js.roughness - want.roughness) <= 1e-6, `prim ${i} roughness`);
    assert.equal(
      js.texture ? js.texture.mime : null,
      want.texture_mime ?? null,
      `prim ${i} texture mime`,
    );
    assert.equal(
      js.texture ? js.texture.bytes.length : null,
      want.texture_len ?? null,
      `prim ${i} texture length`,
    );
  }
});

test("scene decode: transformed positions/normals within tolerance domain", () => {
  const { primitives } = decodeGlbScene(glb);
  const near = (a, b, t, what) =>
    assert.ok(Math.abs(a - b) <= t, `${what}: ${a} vs ${b} (tol ${t})`);
  for (let i = 0; i < primitives.length; i++) {
    const js = primitives[i];
    const want = fixture.expect.primitives[i];
    for (let v = 0; v < want.positions.length; v++) {
      for (let k = 0; k < 3; k++) {
        near(js.positions[v][k], want.positions[v][k], tol.transformed_abs, `prim${i} pos`);
      }
    }
    if (want.normals) {
      assert.ok(js.normals, `prim${i} should have normals`);
      for (let v = 0; v < want.normals.length; v++) {
        for (let k = 0; k < 3; k++) {
          near(js.normals[v][k], want.normals[v][k], tol.transformed_abs, `prim${i} normal`);
        }
      }
    }
    if (want.uvs) {
      assert.ok(js.uvs, `prim${i} should have uvs`);
      for (let v = 0; v < want.uvs.length; v++) {
        for (let k = 0; k < 2; k++) {
          near(js.uvs[v][k], want.uvs[v][k], 1e-6, `prim${i} uv`);
        }
      }
    }
  }
});

test("base color helper: falls back to spec default [1,1,1,1]", () => {
  const { primitives } = decodeGlbScene(glb);
  assert.deepEqual(primitiveBaseColor(primitives[0]), [1, 1, 1, 1]);
  assert.deepEqual(primitiveBaseColor(primitives[1]), [0.8, 0.2, 0.1, 1]);
});
