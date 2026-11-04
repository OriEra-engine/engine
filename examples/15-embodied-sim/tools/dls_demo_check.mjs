// dls_demo_check.mjs — P1-B2 浏览器 N 轴演示策略的 node 侧验收代理。
//
// 用 vendor/oriera/ 的 wasm 装载指定 resolved spec，跑 kernel.js 的
// N 轴 DLS 演示策略（heavyPhases + 标定/钉住/限幅，与浏览器同一份代码），
// 统计 success / 抬离地面。对应 PLAN-embodied-v3 P1 #16 验收：
// 「12 次演示 ≥6 次把方块带离地面」。
//
// 运行：node examples/15-embodied-sim/tools/dls_demo_check.mjs [--episodes 12]
//   [--seed0 1000] [--spec specs/resolved/panda-7dof-pick.json]
import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

const DEMO_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const argOf = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : dflt;
};
const EPISODES = parseInt(argOf("--episodes", "12"));
const SEED0 = parseInt(argOf("--seed0", "1000"));
const SPEC_PATH = path.resolve(DEMO_DIR, argOf("--spec", "specs/resolved/panda-7dof-pick.json"));
const LCG = (s) => Math.floor(s * 6364136223846793 % 4294967296) >>> 0;

// kernel.js import "three" → vendor 解析钩子（同 kernel.test.mjs）
const hookSrc = `
  import { pathToFileURL } from "node:url";
  const three = process.env.ORIERA_THREE_URL;
  export async function resolve(specifier, context, next) {
    if (specifier === "three") return { url: three, shortCircuit: true };
    return next(specifier, context);
  }
`;
process.env.ORIERA_THREE_URL = pathToFileURL(path.join(DEMO_DIR, "vendor/three.module.js")).href;
const { register } = await import("node:module");
register("data:text/javascript," + encodeURIComponent(hookSrc), import.meta.url);

const kernel = await import(pathToFileURL(path.join(DEMO_DIR, "engine/kernel.js")).href);
const above = kernel.above;
const wasmMod = await import(pathToFileURL(path.join(DEMO_DIR, "vendor/oriera/oriera_wasm.js")).href);
await wasmMod.default({
  module_or_path: readFileSync(path.join(DEMO_DIR, "vendor/oriera/oriera_wasm_bg.wasm")),
});

const specText = readFileSync(SPEC_PATH, "utf8");
const spec = JSON.parse(specText);
const geom = kernel.phaseGeom(spec);
const phases = kernel.heavyPhases(geom);
const totalTicks = phases.reduce((a, p) => a + p.ticks, 0);
console.log(
  `spec ${spec.meta.name} dof=${spec.robot.joints.length} heavy=${geom.heavy} ` +
  `grasp=${geom.grasp} place=${geom.place} schedule=${totalTicks} ticks`,
);

// 与 kernel.js loop() 的 N 轴分支逐行同形（f64 演示口径）
function runEpisode(sim, seed) {
  sim.reset(seed);
  const layout = JSON.parse(sim.layout());
  const iEe = layout.obs.indexOf("ee.x");
  const iObj = layout.obs.indexOf("obj_rel.x");
  const iGoal = layout.obs.indexOf("goal_rel.x");
  const dof = spec.robot.joints.length;
  const na = {
    phases, sag: [0, 0, 0], pin: null, prevCmd: null,
    q: spec.robot.home.slice(), lastErr: 0,
  };
  const lifted = { yes: false, max: 0 };
  let step = 0, success = false;
  for (; step < sim.max_steps(); step++) {
    const obs = sim.obs();
    const ee = [obs[iEe], obs[iEe + 1], obs[iEe + 2]];
    const obj = [ee[0] + obs[iObj], ee[1] + obs[iObj + 1], ee[2] + obs[iObj + 2]];
    const gol = [ee[0] + obs[iGoal], ee[1] + obs[iGoal + 1], ee[2] + obs[iGoal + 2]];
    const ph = kernel.policyTick(na.phases, step);
    const phase = ph >= 0 ? na.phases[ph] : null;
    let raw = phase ? phase.target(obj, gol) : above(gol, 0.22);
    const demand = phase ? phase.grip : 0;
    let ticksBefore = 0;
    for (let i = 0; i < (ph >= 0 ? ph : na.phases.length); i++) ticksBefore += na.phases[i].ticks;
    if (phase?.pin) {
      if (!na.pin) na.pin = obj.slice();
      raw = [na.pin[0] + (raw[0] - obj[0]), na.pin[1] + (raw[1] - obj[1]), na.pin[2] + (raw[2] - obj[2])];
    } else na.pin = null;
    const hoverEnd = na.phases[0].ticks - 1;
    if (step === hoverEnd) na.sag = [raw[0] - ee[0], raw[1] - ee[1], raw[2] - ee[2]];
    if (phase && na.phases[1] === phase && step === ticksBefore + phase.ticks - 1) {
      const ws = [raw[0] + na.sag[0], raw[1] + na.sag[1], raw[2] + na.sag[2]];
      na.sag = [ws[0] - ee[0], ws[1] - ee[1], ws[2] - ee[2]];
    }
    const target = step > hoverEnd
      ? [raw[0] + na.sag[0], raw[1] + na.sag[1], raw[2] + na.sag[2]]
      : raw;
    if (step > 0 && na.lastErr > 0.02) na.q = obs.slice(0, dof);
    na.lastErr = kernel.dlsSolve(spec, na, target, [0, -1, 0], [0, 0, 1]);
    const limited = [];
    for (let i = 0; i < dof; i++) {
      let v = na.q[i];
      if (na.prevCmd) v = na.prevCmd[i] + Math.min(Math.max(v - na.prevCmd[i], -0.028), 0.028);
      limited.push(v);
    }
    na.prevCmd = limited;
    if (process.env.DBG && step % 20 === 0)
      console.log(`    t=${step} grip=${demand.toFixed(1)} obj=(${obj[0].toFixed(3)},${obj[1].toFixed(3)},${obj[2].toFixed(3)}) ee=(${ee[0].toFixed(3)},${ee[1].toFixed(3)},${ee[2].toFixed(3)}) err=${na.lastErr.toFixed(4)} q0=${na.q[0]?.toFixed(2)} q1=${na.q[1]?.toFixed(2)} tgt=(${target[0].toFixed(3)},${target[1].toFixed(3)},${target[2].toFixed(3)})`);
    const r = sim.step([...limited, demand]);
    const props = JSON.parse(sim.poses()).props;
    if (props[0]) lifted.max = Math.max(lifted.max, props[0].pos[1]);
    if (props[0] && props[0].pos[1] > 0.56) lifted.yes = true;
    const term = r.terminated, trunc = r.truncated;
    success = success || r.success;
    r.free();
    if (term || trunc) break;
  }
  return { success, steps: step + 1, lifted };
}

// seed 带：与 CLI eval 的 LCG 链同口径（course/paper 的种子链在 Rust 侧
// eval::evaluate——这里用简化链做**演示**统计，不冒充协议口径）
let ok = 0, liftedOk = 0;
let seed = SEED0;
const rows = [];
for (let i = 0; i < EPISODES; i++) {
  const sim = wasmMod.OrieraSim.from_spec_json(specText);
  const r = runEpisode(sim, seed);
  sim.free();
  rows.push(r);
  if (r.success) ok++;
  if (r.lifted.yes) liftedOk++;
  console.log(
    `  ep${i + 1} seed=${seed} success=${r.success} steps=${r.steps} maxLift=${r.lifted.max.toFixed(3)}`,
  );
  seed = (Math.floor(seed * 6364136223846793 % 4294967296) >>> 0) % 1000000 + 100000;
}
console.log(`\n结果：${ok}/${EPISODES} 成功 · ${liftedOk}/${EPISODES} 带离地面（门槛 ≥6/12）`);
