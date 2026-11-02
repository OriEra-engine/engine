//! kernel.js 测试——M1 物理退役后的新测试集。
//! 旧 14 个 JS↔Rust 常量 parity 守卫随 JS 镜像物理一起退役（parity 由
//! "同一份代码"构造性保证：物理/奖励/判定全在 vendor/oriera 的 wasm 里，
//! 与 Rust CLI 同源）。本文件现在守的是：
//!   1. wasm 装载与预设 spec（preset == specs/tabletop-4dof-pick.json）；
//!   2. Gymnasium 接口（reset/step/obs/pose/layout）；
//!   3. 确定性（双跑逐位一致 + 指纹回执格式）；
//!   4. 渲染器 FK 自洽（poses 的 FK TCP ≈ 物理遥测 TCP）；
//!   5. kernel.js 渲染层策略纯函数（PHASES/ik/policyTick）。
//! 运行：node --test examples/15-embodied-sim/engine/kernel.test.mjs

import { strict as assert } from "node:assert";
import { describe, it, before, after } from "node:test";
import { readFileSync } from "node:fs";
import { register } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

const ENGINE_DIR = path.dirname(fileURLToPath(import.meta.url));
const DEMO_DIR = path.resolve(ENGINE_DIR, "..");
const REPO_ROOT = path.resolve(DEMO_DIR, "..", "..");
const SPEC_PATH = path.join(REPO_ROOT, "crates/oriera-agent/specs/tabletop-4dof-pick.json");
const SPEC_TEXT = readFileSync(SPEC_PATH, "utf8");

// kernel.js import 了 three 与后处理 jsm——node 侧用 resolve 钩子把
// 裸名 "three" 指到 vendor/three.module.js（渲染层策略纯函数测试直接
// import kernel.js 本体，不再像退役前那样拷贝函数）。
let deregister;
before(async () => {
  const hookSrc = `
    import { fileURLToPath, pathToFileURL } from "node:url";
    const three = process.env.ORIERA_THREE_URL;
    export async function resolve(specifier, context, next) {
      if (specifier === "three") return { url: three, shortCircuit: true };
      return next(specifier, context);
    }
  `;
  process.env.ORIERA_THREE_URL = pathToFileURL(path.join(DEMO_DIR, "vendor/three.module.js")).href;
  deregister = register("data:text/javascript," + encodeURIComponent(hookSrc), import.meta.url);
});
after(() => { try { deregister?.(); } catch {} });

// ── wasm 装载 ─────────────────────────────────────────────────
const mod = await import(pathToFileURL(path.join(DEMO_DIR, "vendor/oriera/oriera_wasm.js")).href);
await mod.default({ module_or_path: readFileSync(path.join(DEMO_DIR, "vendor/oriera/oriera_wasm_bg.wasm")) });

describe("oriera-wasm 装载与预设 spec", () => {
  it("from_spec_json 吃下官方 spec 文件（v2 直读，无迁移）", () => {
    const sim = mod.OrieraSim.from_spec_json(SPEC_TEXT);
    assert.equal(sim.migrated(), false);
    assert.equal(sim.task_id(), "pick_place@v1");
    assert.equal(sim.task_phrase(), "pick up the cube and place it at the target");
    assert.equal(sim.dof(), 4);
    assert.equal(sim.dt(), Math.fround(1 / 60)); // f32 dt 透传
    assert.equal(sim.max_steps(), 700);
    sim.free();
  });

  it("preset_spec_json 与 specs/tabletop-4dof-pick.json 同 hash（16 hex）", () => {
    const a = mod.OrieraSim.from_spec_json(mod.OrieraSim.preset_spec_json());
    const b = mod.OrieraSim.from_spec_json(SPEC_TEXT);
    assert.match(a.spec_hash(), /^[0-9a-f]{16}$/);
    assert.equal(a.spec_hash(), b.spec_hash());
    a.free(); b.free();
  });

  it("坏 spec → 可读错误（引擎字段级中文报错，不是 trap）", () => {
    assert.throws(() => mod.OrieraSim.from_spec_json('{"schema":"oriera.scene/2"}'),
      (e) => String(e?.message ?? e).includes("robot"));
    assert.throws(() => mod.OrieraSim.from_spec_json("not json"),
      (e) => String(e?.message ?? e).includes("JSON"));
  });
});

describe("Gymnasium 接口", () => {
  it("reset(1000) → 21 维有限观测", () => {
    const sim = mod.OrieraSim.from_spec_json(SPEC_TEXT);
    const obs = sim.reset(1000);
    assert.ok(obs instanceof Float32Array);
    assert.equal(obs.length, 21);
    for (const v of obs) assert.ok(Number.isFinite(v));
    sim.free();
  });

  it("layout：obs/action 列名是关节名（M0a 布局），分组偏移合法", () => {
    const sim = mod.OrieraSim.from_spec_json(SPEC_TEXT);
    const l = JSON.parse(sim.layout());
    assert.equal(l.obs_dim, 21);
    assert.equal(l.action_dim, 5);
    assert.deepEqual(l.obs.slice(0, 4), ["base_yaw.pos", "shoulder.pos", "elbow.pos", "wrist_pitch.pos"]);
    assert.deepEqual(l.action, ["base_yaw.tgt", "shoulder.tgt", "elbow.tgt", "wrist_pitch.tgt", "gripper"]);
    for (const [name, s, e] of l.obs_slices) {
      assert.ok(Number.isInteger(s) && Number.isInteger(e) && s < e, `slice ${name}`);
    }
    sim.free();
  });

  it("step：StepResult 五元组语义（零动作推进，不终止）", () => {
    const sim = mod.OrieraSim.from_spec_json(SPEC_TEXT);
    sim.reset(1000); // 非退化 seed（seed 7 抽签生成即入区，一步即终局）
    const r = sim.step([0, 0, 0, 0, 0]);
    assert.equal(r.obs.length, 21);
    assert.ok(Number.isFinite(r.reward));
    assert.equal(r.terminated, false);
    assert.equal(r.truncated, false);
    assert.equal(r.success, false);
    r.free();
    sim.free();
  });

  it("非有限动作被拒绝（NaN 防线在 Rust 侧）", () => {
    const sim = mod.OrieraSim.from_spec_json(SPEC_TEXT);
    sim.reset(1000);
    const r = sim.step([NaN, 0, 0, 0, 0]);
    assert.equal(r.reward, 0);
    assert.equal(r.terminated, false);
    r.free();
    sim.free();
  });
});

describe("确定性回执", () => {
  const run120 = (seed) => {
    const sim = mod.OrieraSim.from_spec_json(SPEC_TEXT);
    sim.reset(seed);
    let term = false;
    for (let step = 0; step < 120 && !term; step++) {
      const r = sim.step(Array.from(sim.baseline_action(step)));
      term = r.terminated;
      r.free();
    }
    const out = { fp: sim.fingerprint(), fph: sim.fingerprint_hex(), steps: sim.steps(), obs: Array.from(sim.obs()) };
    sim.free();
    return out;
  };
  it("双跑逐位一致（同 sim 实例重复 reset）", () => {
    assert.deepEqual(run120(1000), run120(1000));
  });
  it("双 sim 实例逐位一致", () => {
    const a = run120(1001), b = run120(1001);
    assert.equal(a.fp, b.fp);
    assert.deepEqual(a.obs, b.obs);
  });
  it("fingerprint 十进制串与 hex 串是同一个 u64", () => {
    const { fp, fph } = run120(1002);
    assert.equal(BigInt(fp).toString(16).padStart(16, "0"), fph);
  });
  it("不同 seed 不同回执", () => {
    assert.notEqual(run120(1000).fp, run120(1003).fp);
  });
});

describe("poses（渲染器全部位姿）", () => {
  it("links 全量、FK TCP ≈ 物理遥测 TCP、指板/prop/goal 就位", () => {
    const sim = mod.OrieraSim.from_spec_json(SPEC_TEXT);
    sim.reset(1000);
    // 家位姿（identity 朝向）下指板 z 间距 = 当前 gap
    const p0 = JSON.parse(sim.poses());
    assert.ok(p0.fingers, "有夹爪 spec 必出指板");
    assert.ok(Math.abs(p0.fingers.gap - (p0.fingers.left.pos[2] - p0.fingers.right.pos[2])) < 1e-5);
    let term = false;
    for (let step = 0; step < 200 && !term; step++) {
      const r = sim.step(Array.from(sim.baseline_action(step)));
      term = r.terminated;
      r.free();
    }
    const p = JSON.parse(sim.poses());
    assert.deepEqual(p.links.map((l) => l.name),
      ["base", "yaw_carrier", "upper_arm", "forearm", "wrist"]);
    for (const l of p.links) {
      assert.equal(l.pos.length, 3);
      assert.equal(l.rot.length, 4);
    }
    // FK TCP vs 物理遥测（渲染层正确性的锚）
    const d = Math.hypot(p.ee.pos[0] - p.ee_telemetry[0], p.ee.pos[1] - p.ee_telemetry[1], p.ee.pos[2] - p.ee_telemetry[2]);
    assert.ok(d < 2e-3, `FK tip vs telemetry drift ${d} m`);
    assert.equal(p.props.length, 1);
    for (const v of p.props[0].pos) assert.ok(Number.isFinite(v));
    assert.deepEqual(p.goal.map((v) => Number.isFinite(v)), [true, true, true]);
    assert.equal(typeof p.holding, "boolean");
    sim.free();
  });
});

describe("kernel.js 渲染层策略（纯函数，物理已退役）", () => {
  let kernel;
  before(async () => { kernel = await import(pathToFileURL(path.join(ENGINE_DIR, "kernel.js")).href); });
  const DESC = {
    base: [0.02, 0.5, 0.0],
    shoulderY: 0.62,
    links: [0.20, 0.18, 0.09],
    limits: [[-Math.PI, Math.PI], [-0.35, 1.45], [-2.5, 0.35], [-2.0, 2.0]],
  };

  it("PHASES 是 collector round-4 开环排程（480+ 步工作段）", () => {
    const total = kernel.CONSTANTS.PHASES.reduce((a, p) => a + p.ticks, 0);
    assert.equal(total, 485);
    assert.equal(kernel.policyTick(kernel.CONSTANTS.PHASES, 0), 0);
    assert.equal(kernel.policyTick(kernel.CONSTANTS.PHASES, 69), 0);
    assert.equal(kernel.policyTick(kernel.CONSTANTS.PHASES, 70), 1);
    assert.equal(kernel.policyTick(kernel.CONSTANTS.PHASES, 10_000), -1, "超程后 -1（悬停兜底）");
  });

  it("ik：可达点给出有限且在限内的角", () => {
    for (const target of [[0.15, 0.55, 0.05], [0.30, 0.60, -0.10], [0.25, 0.50, 0.08]]) {
      const a = kernel.ik(DESC, target);
      assert.equal(a.length, 4);
      for (let i = 0; i < 4; i++) {
        assert.ok(Number.isFinite(a[i]));
        assert.ok(a[i] >= DESC.limits[i][0] - 1e-6 && a[i] <= DESC.limits[i][1] + 1e-6,
          `joint ${i} ${a[i]} 超限 [${DESC.limits[i]}]`);
      }
    }
  });

  it("ikArmFromSpec：从 spec 提取的臂参数与旧常量表一致（4-DoF 预设）", () => {
    const spec = JSON.parse(mod.OrieraSim.from_spec_json(SPEC_TEXT).spec_json());
    const arm = kernel.ikArmFromSpec(spec);
    assert.deepEqual(arm.base, [0.02, 0.5, 0.0]);
    assert.equal(arm.shoulderY, 0.62);
    assert.deepEqual(arm.links, [0.2, 0.18, 0.09]);
    assert.equal(arm.limits[2][0], -2.5);
  });

  it("boot 存在且是 async（node 里不执行——three 需 DOM）", async () => {
    assert.equal(typeof kernel.boot, "function");
    assert.equal(kernel.boot.constructor.name, "AsyncFunction");
  });
});
