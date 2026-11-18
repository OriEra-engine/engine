// 双端金样对拍(#153 M1):与 crates/oriera-asset/tests/cross_language_skin.rs
// 同读 contracts/golden/skin_golden.json——palette 容差 1e-5(JS f64 vs Rust
// 混合 f32/f64 的超越函数边界,金样 meta 有完整约定)、蒙皮顶点 1e-4。
// 覆盖:合成双关节 GLB(内嵌 base64,LINEAR T/R 通道)+ CesiumMan 真模型
// (19 关节/57 通道,读 oriera-asset fixtures,不重复入库)。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { decodeGlbRig, jointMatricesAt, skinVertices } from "./skin.js";

const here = dirname(fileURLToPath(import.meta.url));
const fixture = JSON.parse(
  readFileSync(join(here, "../../../contracts/golden/skin_golden.json"), "utf8"),
);
const tolP = fixture.meta.tolerances.palette_abs;
const tolS = fixture.meta.tolerances.skinned_pos_abs;

const near = (a, b, t, what) =>
  assert.ok(Math.abs(a - b) <= t, `${what}: ${a} vs ${b} (tol ${t})`);

function comparePalettes(gotTimes, wantTimes, tol, what) {
  assert.equal(gotTimes.length, wantTimes.length, `${what}: 时间点数`);
  for (let t = 0; t < gotTimes.length; t++) {
    const got = gotTimes[t], want = wantTimes[t];
    assert.equal(got.length, want.length, `${what} t=${t}: skin 数`);
    for (let s = 0; s < got.length; s++) {
      assert.equal(got[s].length, want[s].length, `${what} t=${t}: joint 数`);
      for (let j = 0; j < got[s].length; j++) {
        for (let k = 0; k < 16; k++) {
          near(got[s][j][k], want[s][j][k], tol, `${what} t=${t} joint${j}[${k}]`);
        }
      }
    }
  }
}

test("synthetic rig: decode stall + palette at 4 times", () => {
  const glb = Uint8Array.from(Buffer.from(fixture.synthetic.glb_base64, "base64"));
  const rig = decodeGlbRig(glb);
  assert.equal(rig.skins.length, 1);
  assert.equal(rig.skins[0].joints.length, fixture.synthetic.joint_count);
  assert.ok(Math.abs(rig.animations[0].duration - fixture.synthetic.duration) < 1e-6);
  const got = fixture.synthetic.sample_times.map((t) => jointMatricesAt(rig, 0, t));
  comparePalettes(got, fixture.synthetic.palettes, tolP, "synthetic palette");
});

test("synthetic rig: CPU skin vertices at half", () => {
  const glb = Uint8Array.from(Buffer.from(fixture.synthetic.glb_base64, "base64"));
  const rig = decodeGlbRig(glb);
  const t = fixture.synthetic.sample_times[2]; // 0.5
  const palette = jointMatricesAt(rig, 0, t)[0];
  const got = skinVertices(rig.primitives[0], palette);
  const want = fixture.synthetic.skinned_positions_at_half;
  assert.equal(got.length, want.length);
  for (let i = 0; i < got.length; i++) {
    for (let k = 0; k < 3; k++) near(got[i][k], want[i][k], tolS, `skin v${i}[${k}]`);
  }
});

test("cesiumman real model: palette at 2 times (19 joints)", () => {
  const path = join(here, "../../../", fixture.cesiumman.fixture);
  const glb = new Uint8Array(readFileSync(path));
  const rig = decodeGlbRig(glb);
  assert.equal(rig.skins[0].joints.length, fixture.cesiumman.joint_count);
  assert.ok(Math.abs(rig.animations[0].duration - fixture.cesiumman.duration) < 1e-3);
  const got = fixture.cesiumman.sample_times.map((t) => jointMatricesAt(rig, 0, t));
  comparePalettes(got, fixture.cesiumman.palettes, tolP, "cesiumman palette");
});
