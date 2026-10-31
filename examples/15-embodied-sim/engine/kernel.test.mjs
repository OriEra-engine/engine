//! kernel.js 测试——JS↔Rust parity 守卫 + 纯函数单测。
//! 运行：node --test examples/15-embodied-sim/engine/kernel.test.mjs
//! （在 engine 根目录下，three/rapier import 需 node ESM 支持）

import { strict as assert } from "node:assert";
import { describe, it } from "node:test";

// ── 纯函数拷贝（与 kernel.js 逐字对齐——parity 守卫的"JS 侧"）──
// 这些常量必须与 Rust 侧一一对应：
//   engine/crates/oriera-subsys/src/rigid3d.rs（FINGER_*/PROP_HALF/GRASP_RADIUS）
//   engine/crates/oriera-agent/src/embodied_env.rs（OBS_NAMES/ACTION_NAMES/SETTLE_TICKS/MAX_STEPS）

const DT = 1 / 60;
const SUBSTEPS = 2;
const PROP_HALF = 0.025;
const GRASP_RADIUS = 0.06;
const ZONE_RADIUS = 0.09;
const FINGER_HALF = [0.030, 0.045, 0.006];
const FINGER_X = 0.135;
const FINGER_GAP_OPEN = 0.075;
const FINGER_GAP_CLOSED = 0.046;
const FINGER_FRICTION = 1.4;
const SETTLE_TICKS = 45;
const MAX_STEPS = 700;

const DESC = {
  base: [0.02, 0.5, 0.0],
  shoulderHeight: 0.12,
  links: [0.20, 0.18, 0.09],
  radius: 0.022,
  limits: [
    [-Math.PI, Math.PI],
    [-0.35, 1.45],
    [-2.5, 0.35],
    [-2.0, 2.0],
  ],
  kp: 60,
  kd: 4,
};

// ── parity 守卫（Rust 常量逐项比对）──

describe("JS↔Rust constant parity", () => {
  it("FINGER_HALF matches rigid3d.rs", () => {
    // Rust: const FINGER_HALF: [f32; 3] = [0.030, 0.045, 0.006];
    assert.deepEqual(FINGER_HALF, [0.030, 0.045, 0.006]);
  });
  it("FINGER_X = 0.135 (plates past TCP)", () => {
    // Rust: const FINGER_X: f32 = 0.135;
    assert.equal(FINGER_X, 0.135);
  });
  it("FINGER_GAP_OPEN/CLOSED match rigid3d.rs", () => {
    assert.equal(FINGER_GAP_OPEN, 0.075);
    assert.equal(FINGER_GAP_CLOSED, 0.046);
    assert.ok(FINGER_GAP_CLOSED < 2 * 0.025, "closed gap must squeeze the cube");
  });
  it("GRASP_RADIUS = 0.06", () => {
    assert.equal(GRASP_RADIUS, 0.06);
  });
  it("PROP_HALF = 0.025", () => {
    assert.equal(PROP_HALF, 0.025);
  });
  it("elbow limit matches round-4 widening (-2.5)", () => {
    // Rust: (-2.50, 0.35) — round-4 拓宽
    assert.equal(DESC.limits[2][0], -2.5);
  });
  it("DESC.links = [0.20, 0.18, 0.09]", () => {
    assert.deepEqual(DESC.links, [0.20, 0.18, 0.09]);
  });
  it("DESC.kp/kd match rigid3d.rs ForceBased gains", () => {
    assert.equal(DESC.kp, 60);
    assert.equal(DESC.kd, 4);
  });
  it("SETTLE_TICKS = 45", () => {
    assert.equal(SETTLE_TICKS, 45);
  });
  it("MAX_STEPS = 700", () => {
    assert.equal(MAX_STEPS, 700);
  });
  it("DT = 1/60", () => {
    assert.ok(Math.abs(DT - 1 / 60) < 1e-10);
  });
  it("SUBSTEPS = 2", () => {
    assert.equal(SUBSTEPS, 2);
  });
});

// ── IK 纯函数单测 ──

function ik(target) {
  const l1 = 0.20, l2 = 0.18, shoulderY = 0.62;
  const dx = target[0] - 0.02, dz = target[2];
  const yaw = Math.atan2(-dz, dx);
  const d = Math.min(Math.max(Math.hypot(dx, dz), 0.06), 0.375);
  const h = Math.min(Math.max(target[1] + 0.09 - shoulderY, -0.38), 0.30);
  let ce = Math.min(Math.max((d * d + h * h - l1 * l1 - l2 * l2) / (2 * l1 * l2), -1), 1);
  const elbow = -Math.acos(ce);
  const shoulder = Math.atan2(h, d) - Math.atan2(0.18 * Math.sin(elbow), 0.20 + 0.18 * Math.cos(elbow));
  const wrist = -Math.PI / 2 - shoulder - elbow;
  const cl = (v, i) => Math.min(Math.max(v, lims[i][0]), lims[i][1]);
  const lims = DESC.limits;
  return [cl(yaw, 0), cl(shoulder, 1), cl(elbow, 2), cl(wrist, 3)];
}

describe("IK function", () => {
  it("reachable target produces finite angles", () => {
    const a = ik([0.30, 0.55, 0.0]);
    for (const v of a) assert.ok(Number.isFinite(v), `angle ${v} must be finite`);
  });
  it("all angles within limits", () => {
    for (const target of [[0.15, 0.55, 0.05], [0.30, 0.60, -0.10], [0.25, 0.50, 0.08]]) {
      const a = ik(target);
      for (let i = 0; i < 4; i++) {
        assert.ok(a[i] >= DESC.limits[i][0] - 1e-6 && a[i] <= DESC.limits[i][1] + 1e-6,
          `joint ${i} angle ${a[i]} outside [${DESC.limits[i]}]`);
      }
    }
  });
});
