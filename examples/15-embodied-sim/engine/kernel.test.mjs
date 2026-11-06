//! kernel.js 测试——M1 物理退役后的新测试集。
//! 旧 14 个 JS↔Rust 常量 parity 守卫随 JS 镜像物理一起退役（parity 由
//! "同一份代码"构造性保证：物理/奖励/判定全在 vendor/oriera 的 wasm 里，
//! 与 Rust CLI 同源）。本文件现在守的是：
//!   1. wasm 装载与预设 spec（preset == specs/tabletop-4dof-pick.json）；
//!   2. Gymnasium 接口（reset/step/obs/pose/layout）；
//!   3. 确定性（双跑逐位一致 + 指纹回执格式）；
//!   4. 渲染器 FK 自洽（poses 的 FK TCP ≈ 物理遥测 TCP）；
//!   5. kernel.js 渲染层策略纯函数（PHASES/ik/policyTick）；
//!   6. P2-R 渲染全入口一致：props[1..] 全量位姿、prop 形状（内部标签）、
//!      目标标记贴支撑面（goal_marker）；
//!   7. Wave-2 B 包 drawer 相位机：任务分流、排程与 Rust drawer_phases 同表、
//!      滑轨轴胡萝卜、真 wasm 拉 drawer@v2（max_pull ≥ 0.08 + 面板单轴位移）。
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
const SPEC_PATH = path.join(REPO_ROOT, "crates/oriera-embodied/specs/tabletop-4dof-pick.json");
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

// ── P2-R：props[1..] 全入口渲染守卫（浏览器入口：wasm poses + kernel 几何）──
// 现状核对（caa207c）：poses() 已全量，但 kernel.js 按外部标签读 prop 形状
// （s.Cuboid…），spec_json 实际是内部标签 {"kind":"cuboid",…}——所有 prop
// 一律画成 5 cm 方块（drawer-v2 12 cm 面板、旧 5-prop 料箱四壁 props[1..4]
// 全画错）；目标圈写死 0.501（mobile-arm 地面级目标悬空半米）。
describe("P2-R：props[1..] / 形状 / 目标标记（渲染全入口一致）", () => {
  let kernel;
  before(async () => { kernel = await import(pathToFileURL(path.join(ENGINE_DIR, "kernel.js")).href); });
  const resolved = (n) => readFileSync(path.join(DEMO_DIR, "specs/resolved", `${n}.json`), "utf8");
  const dist = (a, b) => Math.hypot(a[0] - b[0], a[1] - b[1], a[2] - b[2]);
  // extends 展开（同 specs/sync-resolved.mjs 的 merge_overlay：对象逐键、数组逐下标、换变体整体替换）
  const isObj = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
  const merge = (base, ov) => {
    if (isObj(base) && isObj(ov)) {
      const bk = Object.keys(base), ok = Object.keys(ov);
      const kindChanges = "kind" in base && "kind" in ov && base.kind !== ov.kind;
      if (kindChanges || (bk.length === 1 && ok.length === 1 && bk[0] !== ok[0])) return ov;
      for (const k of ok) base[k] = k in base ? merge(base[k], ov[k]) : ov[k];
      return base;
    }
    if (Array.isArray(base) && Array.isArray(ov)) {
      ov.forEach((x, i) => { if (i < base.length) base[i] = merge(base[i], x); else base.push(x); });
      return base;
    }
    return ov;
  };

  it("stack：poses 的 props[1]（底座 B）在其世界位姿，不在原点", () => {
    const sim = mod.OrieraSim.from_spec_json(resolved("stack-4dof"));
    sim.reset(1000);
    const p = JSON.parse(sim.poses());
    assert.equal(p.props.length, 2);
    assert.equal(p.props[1].name, "block_b");
    assert.equal(p.props[1].rot_known, true);
    assert.ok(dist(p.props[1].pos, [0.30, 0.525, 0]) < 2e-3, `B at ${p.props[1].pos}`);
    sim.free();
  });

  it("旧 5-prop 料箱（14fc9bf8）：四壁 props[1..4] 全量位姿 + kernel 按 spec 形状建几何", () => {
    const legacy = JSON.parse(readFileSync(path.join(REPO_ROOT,
      "crates/oriera-embodied/tests/fixtures/bin-pick-4dof-legacy-5prop.json"), "utf8"));
    assert.equal(legacy.extends, "tabletop-4dof-pick");
    delete legacy.extends;
    const spec = merge(JSON.parse(resolved("tabletop-4dof-pick")), legacy);
    const sim = mod.OrieraSim.from_spec_json(JSON.stringify(spec));
    assert.equal(sim.spec_hash(), "14fc9bf8c107e5da", "与 results/reference/14fc9bf8 档案同一身份");
    sim.reset(1000);
    const p = JSON.parse(sim.poses());
    const s = JSON.parse(sim.spec_json());
    assert.equal(p.props.length, 5);
    for (let i = 1; i < 5; i++) {
      const sp = s.scene.props[i];
      assert.ok(dist(p.props[i].pos, sp.placement.fixed) < 0.02, `${sp.name} at ${p.props[i].pos}`);
      assert.ok(Math.hypot(...p.props[i].pos) > 0.05, `${sp.name} 不得画在原点`);
      assert.equal(sp.shape.kind, "cuboid", "spec_json 的 prop 形状是内部标签");
      assert.deepEqual(kernel.propGeom(sp.shape).size, sp.shape.half.map((h) => h * 2), sp.name);
    }
    sim.free();
  });

  it("propGeom：内部标签三形状 + 外部标签兼容 + 缺省 5 cm 方块", () => {
    assert.deepEqual(kernel.propGeom({ kind: "cuboid", half: [0.06, 0.025, 0.025] }),
      { kind: "cuboid", size: [0.12, 0.05, 0.05] });
    assert.deepEqual(kernel.propGeom({ kind: "cylinder", half_h: 0.03, radius: 0.02 }),
      { kind: "cylinder", radius: 0.02, height: 0.06 });
    assert.deepEqual(kernel.propGeom({ kind: "sphere", r: 0.03 }), { kind: "sphere", radius: 0.03 });
    assert.deepEqual(kernel.propGeom({ Cuboid: { half: [0.01, 0.03, 0.11] } }).size, [0.02, 0.06, 0.22]);
    assert.deepEqual(kernel.propGeom({ Cylinder: { half_h: 0.02, radius: 0.01 } }),
      { kind: "cylinder", radius: 0.01, height: 0.04 });
    assert.deepEqual(kernel.propGeom(undefined).size, [0.05, 0.05, 0.05]);
  });

  it("drawer-v2：spec_json 面板形状 → 120×50×50 mm 几何（旧读法落空成 5 cm 方块）", () => {
    const sim = mod.OrieraSim.from_spec_json(resolved("drawer-4dof-v2"));
    const s = JSON.parse(sim.spec_json());
    assert.deepEqual(kernel.propGeom(s.scene.props[0].shape).size, [0.12, 0.05, 0.05]);
    sim.free();
  });

  it("goal_marker：目标标记贴支撑面（stack/drawer 悬空目标落桌面、mobile-arm 地面、金标原样）", () => {
    for (const [name, y, verbatim] of [
      ["tabletop-4dof-pick", 0.5, true], ["mobile-arm-4dof", 0, true],
      ["stack-4dof", 0.5, false], ["drawer-4dof-v2", 0.5, false],
    ]) {
      const sim = mod.OrieraSim.from_spec_json(resolved(name));
      sim.reset(1000);
      const p = JSON.parse(sim.poses());
      assert.ok(Array.isArray(p.goal_marker), `${name}: poses 带 goal_marker`);
      assert.equal(p.goal_marker[0], p.goal[0]);
      assert.equal(p.goal_marker[2], p.goal[2]);
      assert.equal(p.goal_marker[1], y, name);
      if (verbatim) assert.deepEqual(p.goal_marker, p.goal, `${name}: 贴面目标原样`);
      else assert.ok(p.goal[1] > y + 0.01, `${name}: 悬空目标 ${p.goal}`);
      assert.deepEqual(kernel.goalMarkerOf(p), p.goal_marker);
      sim.free();
    }
    // 旧 wasm（无 goal_marker 字段）回退旧写死高度
    assert.deepEqual(kernel.goalMarkerOf({ goal: [0.3, 0.575, 0.1] }), [0.3, 0.5, 0.1]);
  });
});

// ── Wave-2 B 包：drawer 相位机（移植 embodied_sim_baseline.rs drawer_phases）──
// S 包验收缺口：drawer@v2 卡「▶ 脚本策略」走抓放节拍，合爪后上提把滑轨面板
// 拽向限位、板间打滑（wasm baseline_action 对 drawer 返回空数组，无替补）。
// 这里守：任务分流（只认 task.id）、排程与 Rust 同表、滑轨轴胡萝卜与 Rust
// goal 原式同解、真 wasm 拉 drawer@v2（max_pull ≥ 0.08 + 面板只沿滑轨轴动）。
describe("drawer 相位机（B 包：drawer@v2 滑轨真跑）", () => {
  let kernel;
  before(async () => { kernel = await import(pathToFileURL(path.join(ENGINE_DIR, "kernel.js")).href); });
  const resolved = (n) => readFileSync(path.join(DEMO_DIR, "specs/resolved", `${n}.json`), "utf8");
  const specOf = (n) => {
    const sim = mod.OrieraSim.from_spec_json(resolved(n));
    const s = JSON.parse(sim.spec_json()); // 页面 K.spec 同源（wasm 解析后的 spec）
    sim.free();
    return s;
  };

  it("分流只认 task.id：drawer@v1/@v2 进相位机，五张非 drawer 卡不进（panda 的 params 带 pull_axis 默认值也不误伤）", () => {
    for (const n of ["drawer-4dof-v2", "drawer-4dof"]) assert.equal(kernel.isDrawerTask(specOf(n)), true, n);
    for (const n of ["tabletop-4dof-pick", "panda-7dof-pick", "stack-4dof", "bin-pick-4dof", "mobile-arm-4dof"]) {
      assert.equal(kernel.isDrawerTask(specOf(n)), false, n);
    }
    assert.ok(Array.isArray(specOf("panda-7dof-pick").task.params.pull_axis), "按字段判别会误伤 panda");
    assert.equal(kernel.isDrawerTask({}), false);
  });

  it("排程与 Rust drawer_phases 同表：悬停 70 → 下探 60 → 合爪 40 → 拖拽 240 → 松爪 30 → 退 45", () => {
    const spec = specOf("drawer-4dof-v2");
    assert.deepEqual(kernel.drawerAxisOf(spec), [0, 0, -1], "滑轨轴读 scene.props[0].slide.axis");
    assert.equal(kernel.drawerAxisOf(specOf("drawer-4dof")), null, "v1 无滑轨 → Rust 原式");
    const geom = kernel.phaseGeom(spec);
    assert.equal(geom.grasp, 0.03, "抓取高度 = Rust m.grasp()（预设夹爪 0.030）");
    const ph = kernel.drawerPhases(geom, kernel.drawerAxisOf(spec));
    assert.deepEqual(ph.map((p) => p.ticks), [70, 60, 40, 240, 30, 45]);
    assert.deepEqual(ph.map((p) => p.grip), [0, 0, 1, 1, 0, 0]);
    const o = [0.28, 0.525, 0];
    assert.deepEqual(ph[0].target(o), [0.28, 0.645, 0]);
    for (const i of [1, 2, 4]) assert.deepEqual(ph[i].target(o), [0.28, 0.555, 0], `phase ${i}`);
    assert.deepEqual(ph[5].target(o), [0.28, 0.745, 0]);
    assert.equal(kernel.policyTick(ph, 169), 2);
    assert.equal(kernel.policyTick(ph, 170), 3, "第 170 拍（70+60+40）进拖拽段");
    assert.equal(kernel.policyTick(ph, 410), 4, "第 410 拍松爪");
    assert.equal(kernel.policyTick(ph, 485), -1, "超程 → 悬停兜底");
  });

  it("滑轨轴胡萝卜 ≡ Rust goal 原式（面板在轴线上逐值同解，含瞄过 1 cm 与越程回拉）", () => {
    const spec = specOf("drawer-4dof-v2");
    const geom = kernel.phaseGeom(spec);
    const axisDrag = kernel.drawerPhases(geom, kernel.drawerAxisOf(spec))[3].target;
    const rustDrag = kernel.drawerPhases(geom, null)[3].target;
    const birth = [0.28, 0.525, 0], goal = [0.28, 0.525, -0.08];
    for (const pulled of [0, 0.02, 0.05, 0.075, 0.079, 0.0805, 0.085, 0.095]) {
      const o = [birth[0], birth[1], birth[2] - pulled];
      const a = axisDrag(o, goal), r = rustDrag(o, goal);
      for (let k = 0; k < 3; k++) assert.ok(Math.abs(a[k] - r[k]) < 1e-12, `pulled ${pulled}: ${a} vs ${r}`);
      const lead = o[2] - a[2]; // 正 = 沿 −Z 领先
      const want = Math.min(Math.abs(0.08 - pulled) + 0.01, 0.03) * Math.sign(0.08 - pulled);
      assert.ok(Math.abs(lead - want) < 1e-12, `pulled ${pulled}: lead ${lead} want ${want}`);
      assert.equal(a[0], o[0], "胡萝卜不偏离滑轨轴（x 不动）");
      assert.equal(a[1], o[1] + 0.03, "拖拽保持抓取高度");
    }
  });

  it("真跑 wasm：drawer@v2 max_pull ≥ 0.08 m、面板位移只沿滑轨轴、判据成功（3 seeds）", () => {
    const text = resolved("drawer-4dof-v2");
    const sim = mod.OrieraSim.from_spec_json(text);
    const spec = JSON.parse(sim.spec_json());
    const L = JSON.parse(sim.layout());
    const idx = { ee: L.obs.indexOf("ee.x"), obj: L.obs.indexOf("obj_rel.x"), goal: L.obs.indexOf("goal_rel.x") };
    const arm = kernel.ikArmFromSpec(spec);
    const slide = spec.scene.props[0].slide;
    const u = kernel.drawerAxisOf(spec);
    for (const seed of [1000, 1001, 1002]) {
      sim.reset(seed);
      const dr = kernel.drawerPolicy(spec, kernel.phaseGeom(spec), idx);
      const birth = JSON.parse(sim.poses()).props[0].pos.slice();
      let maxPull = -Infinity, maxPerp = 0, minPull = Infinity, worstTel = 0, success = false, at = -1;
      for (let step = 0; step < sim.max_steps() && !success; step++) {
        const r = sim.step(kernel.drawerAction(dr, arm, sim.obs(), step));
        success = r.success; const done = r.terminated || r.truncated;
        r.free();
        const p = JSON.parse(sim.poses()).props[0].pos;
        const d = [p[0] - birth[0], p[1] - birth[1], p[2] - birth[2]];
        const pull = d[0] * u[0] + d[1] * u[1] + d[2] * u[2];
        maxPull = Math.max(maxPull, pull); minPull = Math.min(minPull, pull);
        maxPerp = Math.max(maxPerp, Math.hypot(d[0] - pull * u[0], d[1] - pull * u[1], d[2] - pull * u[2]));
        worstTel = Math.max(worstTel, Math.abs(kernel.drawerPull(dr, sim.obs()) - pull));
        if (success) at = step;
        if (done) break;
      }
      assert.equal(success, true, `seed ${seed}: drawer@v2 判据未成（max_pull ${maxPull}）`);
      assert.ok(maxPull >= 0.08, `seed ${seed}: max_pull ${maxPull} < 0.08`);
      assert.ok(maxPerp < 1e-4, `seed ${seed}: 面板离轴 ${maxPerp} m（应只沿滑轨轴动）`);
      // 行程内无求解器瞬态（抓放节拍上提时实测出现过一帧 −10.6 m）
      assert.ok(minPull > -0.01 && maxPull < slide.limits[1] + 0.005, `seed ${seed}: pull ∈ [${minPull}, ${maxPull}]`);
      assert.ok(worstTel < 1e-6, `seed ${seed}: 遥测拉出量偏差 ${worstTel}`);
      // 判据含「松爪」：成功只可能落在松爪段（第 410 拍）之后
      assert.ok(dr.maxPull >= 0.08 && at >= 410, `seed ${seed}: 遥测 max_pull ${dr.maxPull} / 成功拍 ${at}`);
    }
    sim.free();
  });

  it("超程兜底同 Rust：goal 上方 0.22 悬停、开爪", () => {
    const sim = mod.OrieraSim.from_spec_json(resolved("drawer-4dof-v2"));
    const spec = JSON.parse(sim.spec_json());
    const L = JSON.parse(sim.layout());
    const obs = sim.reset(1000);
    const iEe = L.obs.indexOf("ee.x"), iGoal = L.obs.indexOf("goal_rel.x");
    const dr = kernel.drawerPolicy(spec, kernel.phaseGeom(spec), { ee: iEe, obj: L.obs.indexOf("obj_rel.x"), goal: iGoal });
    const arm = kernel.ikArmFromSpec(spec);
    const gol = [0, 1, 2].map((k) => obs[iEe + k] + obs[iGoal + k]);
    const truth = Array.from(sim.goal());
    for (let k = 0; k < 3; k++) assert.ok(Math.abs(gol[k] - truth[k]) < 1e-6, "观测推出的 goal = sim.goal()");
    assert.deepEqual(kernel.drawerAction(dr, arm, obs, 10_000), [...kernel.ik(arm, [gol[0], gol[1] + 0.22, gol[2]]), 0]);
    sim.free();
  });
});
