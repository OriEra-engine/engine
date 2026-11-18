// 双端金样对拍(PLAN-splat v2 M2,#188 评审 04/09 号门槛):
// 本测试与 crates/oriera-splat/tests/cross_language.rs 同读
// contracts/golden/splat_golden.json——原始透传字段位型精确、激活值 1e-6、
// 投影 5e-3/1px、排序精确(金样深度两两分离 0.5)。容差域的来龙去脉见金样 meta。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { decodeSplatPly, makeCamera, projectSplats, depthOrder, baseColor } from "./splat.js";

const here = dirname(fileURLToPath(import.meta.url));
const fixture = JSON.parse(
  readFileSync(join(here, "../../../contracts/golden/splat_golden.json"), "utf8"),
);
const ply = Uint8Array.from(Buffer.from(fixture.ply_base64, "base64"));
const cam = fixture.meta.camera;
const tol = fixture.meta.tolerances;

test("decode: raw passthrough fields are bit-exact with the Rust side", () => {
  const cloud = decodeSplatPly(ply);
  assert.equal(cloud.count, fixture.expect.count);
  assert.equal(cloud.shDegree, fixture.expect.sh_degree);
  assert.deepEqual(cloud.comments, fixture.expect.comments);
  const bitsOf = (f32) => Array.from(new Uint32Array(f32.buffer, f32.byteOffset, f32.length));
  assert.deepEqual(bitsOf(cloud.positions), fixture.expect.positions_bits.flat(), "positions must be bit-exact");
  assert.deepEqual(bitsOf(cloud.shDc), fixture.expect.sh_dc_bits.flat(), "sh_dc must be bit-exact");
});

test("decode: activated fields stay within the 1 ulp tolerance domain", () => {
  const cloud = decodeSplatPly(ply);
  const near = (a, b, t) => assert.ok(Math.abs(a - b) <= t, `${a} vs ${b} (tol ${t})`);
  for (let i = 0; i < cloud.count; i++) {
    for (let k = 0; k < 3; k++) near(cloud.scales[3 * i + k], fixture.expect.scales[i][k], tol.activated_abs);
    for (let k = 0; k < 4; k++) near(cloud.rotations[4 * i + k], fixture.expect.rotations[i][k], tol.activated_abs);
    near(cloud.opacities[i], fixture.expect.opacities[i], tol.activated_abs);
  }
});

test("project+order: matches the Rust projection within the documented drift", () => {
  const cloud = decodeSplatPly(ply);
  const camera = makeCamera(cam.eye, cam.target, cam.up, cam.fov_y_deg, cam.width, cam.height);
  const projected = projectSplats(cloud, camera);
  assert.deepEqual(
    projected.map((p) => p.index),
    fixture.expect.project.surviving,
    "surviving set (compaction) must be exact",
  );
  assert.deepEqual(depthOrder(projected), fixture.expect.project.depth_order, "front-to-back order must be exact");
  const byIndex = new Map(projected.map((p) => [p.index, p]));
  for (const want of fixture.expect.project.means_and_extents_and_conics_by_index()) {
    const p = byIndex.get(want.index);
    assert.ok(p, `splat ${want.index} should have survived`);
    want.mean.forEach((v, k) => assert.ok(Math.abs(p.mean[k] - v) <= tol.means_abs, `mean[${k}] of ${want.index}`));
    want.extent.forEach((v, k) => assert.ok(Math.abs(p.extent[k] - v) <= tol.extents_abs, `extent[${k}] of ${want.index}`));
    want.conic.forEach((v, k) => assert.ok(Math.abs(p.conic[k] - v) <= tol.conics_abs, `conic[${k}] of ${want.index}`));
  }
});

// fixture 里 means/extents/conics 是按幸存顺序存的;给测试一个按下标取数的辅助视图,
// 保持 JSON 紧凑(数组)同时让断言可读。
fixture.expect.project.means_and_extents_and_conics_by_index = function () {
  const surviving = this === fixture.expect.project ? fixture.expect.project.surviving : [];
  return surviving.map((index, slot) => ({
    index,
    mean: fixture.expect.project.means[slot],
    extent: fixture.expect.project.extents[slot],
    conic: fixture.expect.project.conics[slot],
  }));
};

test("base color: DC formula agrees with the Rust closed form", () => {
  const cloud = decodeSplatPly(ply);
  for (let i = 0; i < cloud.count; i++) {
    const col = baseColor(cloud, i);
    for (let k = 0; k < 3; k++) {
      const dc = fixture.expect.sh_dc_bits[i][k];
      const raw = new Float32Array(new Uint32Array([dc]).buffer)[0];
      const want = Math.max(0, 0.5 + 0.2820947917738781 * raw);
      assert.ok(Math.abs(col[k] - want) <= 1e-6);
    }
  }
});
