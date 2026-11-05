// wasm_golden_check.mjs — M1 逐位一致验收（node 侧代理）。
//
// 用 vendor/oriera/ 的 oriera-wasm 跑 pick_place 脚本基线（collect 协议：
// seed0 + 退化重抽 displaced band），两遍（两个独立 sim 实例）逐位比对，
// 再与三个外部参照逐集比对：
//   1. contracts/golden/embodied_fingerprints.pre-m0a.json（绝对金标锚；
//      哈希口径 = env.fingerprint() = sys.fingerprint(world) = v1 折叠，
//      Rust 收集器收集）；
//   2. --rust-ref DIR：一次原生 `embodied_sim_collect --episodes N --out DIR`
//      的产物（lerobot-v21/meta/oriera_receipt.json 的指纹 + eval_report.json
//      的逐集 success/steps）——金标口径对不上时按 PLAN §3.2 降级：跑一条
//      Rust 命令取同机参照值逐值比对；
//   3. --golden-v2 FILE（默认 contracts/golden/embodied_fingerprints.v2.json，
//      PLAN-embodied-v3 #17）：scheme-v2 三口径金标——wasm 侧用
//      sim.fingerprint_v2()（与原生收集器同一 fold：state ⊕ robot identity
//      ⊕ spec_hash）对拍 v2 列，v1 列对拍 episode_fingerprints_v1。
// 口径说明见 docs/WASM-RENDERER.md：u64 哈希超出 JS Number 精度，参照
// JSON 一律把大整数按十进制字符串正则提取后逐字符比较。
//
// 运行：node examples/15-embodied-sim/tools/wasm_golden_check.mjs [--episodes 12]
//   [--seed0 1000] [--spec FILE] [--golden FILE] [--golden-v2 FILE]
//   [--rust-ref DIR] [--policy wasm|js] [--strict]

import { readFileSync, existsSync, statSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

const TOOL_DIR = path.dirname(fileURLToPath(import.meta.url));
const DEMO_DIR = path.resolve(TOOL_DIR, "..");            // examples/15-embodied-sim
const REPO_ROOT = path.resolve(DEMO_DIR, "..", "..");     // engine/

function arg(name, dflt) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : dflt;
}
const EPISODES = parseInt(arg("episodes", "12"), 10);
const SEED0 = parseInt(arg("seed0", "1000"), 10);
const POLICY = arg("policy", "wasm"); // wasm=同代码逐位口径；js=JS fround ik（渲染层）
const STRICT = process.argv.includes("--strict");
const SPEC = path.resolve(arg("spec", path.join(REPO_ROOT, "crates/oriera-embodied/specs/tabletop-4dof-pick.json")));
const GOLDEN = path.resolve(arg("golden", path.join(REPO_ROOT, "contracts/golden/embodied_fingerprints.pre-m0a.json")));
const GOLDEN_V2 = path.resolve(arg("golden-v2", path.join(REPO_ROOT, "contracts/golden/embodied_fingerprints.v2.json")));
const RUST_REF = arg("rust-ref", null);

// ── u64 安全的 JSON 提取（十进制字符串，绕开 Number 2^53 精度）──
function extractU64Array(jsonText, key) {
  const m = jsonText.match(new RegExp(`"${key}"\\s*:\\s*\\[([^\\]]*)\\]`));
  if (!m) return null;
  return m[1].split(",").map((s) => s.trim().replace(/^"|"$/g, "")).filter(Boolean);
}
// 大整数数组 → 字符串数组后正常 parse（金标文件读法）。
// keys：所有装 u64 指纹的数组键（pre-m0a 只有 episode_state_hashes；
// v2 金标 fingerprints 块里三口径三个数组都要保护）。
const U64_KEYS = [
  "episode_state_hashes",
  "episode_fingerprints_v1",
  "episode_fingerprints_v2",
];
function parseWithU64Strings(jsonText) {
  let patched = jsonText;
  for (const key of U64_KEYS) {
    patched = patched.replace(
      new RegExp(`("${key}"\\s*:\\s*\\[)([^\\]]*)(\\])`, "g"),
      (_m, a, body, c) =>
        a + body.split(",").map((s) => `"${s.trim()}"`).join(",") + c,
    );
  }
  return JSON.parse(patched);
}

// ── wasm 装载 ───────────────────────────────────────────────────
const VENDOR = path.join(DEMO_DIR, "vendor/oriera");
const mod = await import(pathToFileURL(path.join(VENDOR, "oriera_wasm.js")).href);
await mod.default({ module_or_path: readFileSync(path.join(VENDOR, "oriera_wasm_bg.wasm")) });

const specText = readFileSync(SPEC, "utf8");
const probe = mod.OrieraSim.from_spec_json(specText);
const specHash = probe.spec_hash();
const obsNames = JSON.parse(probe.layout()).obs;
const I_EE = obsNames.indexOf("ee.x");
const I_OBJ = obsNames.indexOf("obj_rel.x");
const I_GOAL = obsNames.indexOf("goal_rel.x");
probe.free();

// JS 渲染层策略口径（--policy js）：kernel.js 的 PHASES+ik（f32 逐位仿）
let kernel = null, jsArm = null;
if (POLICY === "js") {
  kernel = await import(pathToFileURL(path.join(DEMO_DIR, "engine", "kernel.js")).href);
  jsArm = kernel.ikArmFromSpec(JSON.parse(mod.OrieraSim.from_spec_json(specText).spec_json()));
}

// ── collect 协议镜像（对照 examples/embodied_sim_collect.rs）──────
function runCollection(label) {
  const sim = mod.OrieraSim.from_spec_json(specText);
  const maxSteps = sim.max_steps();
  const episodes = [];
  let kept = 0, draws = 0, redraws = 0, stepsTotal = 0, msTotal = 0;
  while (kept < EPISODES) {
    draws += 1;
    const seed = SEED0 + (draws - 1) + 65536 * redraws;
    let obs = sim.reset(seed);
    let step = 0, terminated = false, rewardSum = 0;
    const t0 = performance.now();
    while (step < maxSteps && !terminated) {
      const action = POLICY === "wasm"
        ? Array.from(sim.baseline_action(step))
        : jsPolicy(obs, step);
      const r = sim.step(action);
      rewardSum += r.reward;
      terminated = r.terminated;
      obs = r.obs;
      r.free();
      step += 1;
    }
    msTotal += performance.now() - t0;
    stepsTotal += step;
    // 退化过滤：生成即入区（<5 步终局）剔除重抽（displaced seed band）
    if (step < 5) {
      redraws += 1;
      continue;
    }
    kept += 1;
    episodes.push({
      draw: draws, seed, success: terminated, steps: step, rewardSum,
      fingerprint: sim.fingerprint(), fingerprintHex: sim.fingerprint_hex(),
      fingerprintV2: sim.fingerprint_v2(),
    });
  }
  const receipt = sim.randomization();
  sim.free();
  console.log(`# ${label}: ${episodes.length} 集 · 抽签 ${draws}（重抽 ${redraws}）· ${stepsTotal} 步 · ${(stepsTotal / (msTotal / 1000)).toFixed(0)} steps/s (node wasm)`);
  return { episodes, receipt, redraws };
}

function jsPolicy(obs, step) {
  const ee = [obs[I_EE], obs[I_EE + 1], obs[I_EE + 2]];
  const obj = [ee[0] + obs[I_OBJ], ee[1] + obs[I_OBJ + 1], ee[2] + obs[I_OBJ + 2]];
  const gol = [ee[0] + obs[I_GOAL], ee[1] + obs[I_GOAL + 1], ee[2] + obs[I_GOAL + 2]];
  const ph = kernel.policyTick(kernel.CONSTANTS.PHASES, step);
  const phase = ph >= 0 ? kernel.CONSTANTS.PHASES[ph] : null;
  return phase ? [...kernel.ik(jsArm, phase.target(obj, gol)), phase.grip]
               : [...kernel.ik(jsArm, kernel.above(gol, 0.22)), 0];
}

// ── 双跑 ───────────────────────────────────────────────────────
console.log(`# wasm_golden_check · spec=${path.basename(SPEC)} · spec_hash=${specHash} · policy=${POLICY} · episodes=${EPISODES} · seed0=${SEED0}`);
const runA = runCollection("跑 1");
const runB = runCollection("跑 2");

let bitIdentical = true;
runA.episodes.forEach((e, i) => {
  const b = runB.episodes[i];
  const same = e.fingerprint === b.fingerprint && e.steps === b.steps && e.success === b.success &&
    Object.is(e.rewardSum, b.rewardSum);
  if (!same) { bitIdentical = false; console.log(`#   ✗ ep${i + 1}: A=${e.fingerprint} B=${b.fingerprint}`); }
});
console.log(`# 双跑逐位一致: ${bitIdentical ? "PASS ✓" : "FAIL ✗"}`);

// ── 逐集表 ─────────────────────────────────────────────────────
console.log("\ndraw seed    success steps reward   fingerprint_v1(u64)                            fingerprint_v2(u64)");
for (const e of runA.episodes) {
  console.log(
    `${String(e.draw).padEnd(4)} ${String(e.seed).padEnd(8)} ${e.success ? "T" : "F"}       ${String(e.steps).padEnd(5)} ${e.rewardSum.toFixed(3).padEnd(8)} ${e.fingerprint.padEnd(46)} ${e.fingerprintV2}`,
  );
}

// ── 参照 1：绝对金标（pre-m0a golden）───────────────────────────
let goldenAligned = null;
if (existsSync(GOLDEN)) {
  const g = parseWithU64Strings(readFileSync(GOLDEN, "utf8"));
  const gSeeds = g.seeds.map(String);
  const gHashes = g.episode_state_hashes;
  const seedBandSame = JSON.stringify(runA.episodes.map((e) => String(e.seed))) === JSON.stringify(gSeeds);
  const hashSame = JSON.stringify(runA.episodes.map((e) => e.fingerprint)) === JSON.stringify(gHashes);
  const succSame = runA.episodes.filter((e) => e.success).length === g.successes;
  goldenAligned = seedBandSame && hashSame && succSame;
  console.log(`\n# 参照1 绝对金标（${path.basename(GOLDEN)} · engine_commit ${g.engine_commit}）:`);
  console.log(`#   seed band 逐集一致: ${seedBandSame ? "PASS ✓" : "FAIL ✗"}`);
  console.log(`#   逐集指纹逐位一致:   ${hashSame ? "PASS ✓" : "FAIL ✗"}`);
  console.log(`#   成功率一致:         ${succSame ? "PASS ✓" : "FAIL ✗"}（${runA.episodes.filter((e) => e.success).length}/${EPISODES} vs ${g.successes}/12 = ${(g.success_rate * 100).toFixed(1)}%）`);
  if (!hashSame) {
    runA.episodes.forEach((e, i) => {
      const same = e.fingerprint === gHashes[i];
      if (!same) console.log(`#     ep${i + 1} seed ${e.seed}: wasm ${e.fingerprint} golden ${gHashes[i]} ✗`);
    });
  }
}

// ── 参照 2：--rust-ref（同机原生 collect 产物）──────────────────
let rustAligned = null;
if (RUST_REF) {
  const receiptPath = path.join(RUST_REF, "lerobot-v21", "meta", "oriera_receipt.json");
  const reportPath = path.join(RUST_REF, "eval_report.json");
  if (!existsSync(receiptPath)) {
    console.log(`\n# 参照2 Rust 原生：找不到 ${receiptPath}（先跑 embodied_sim_collect --episodes ${EPISODES} --out ${RUST_REF}）`);
  } else {
    const rHashes = extractU64Array(readFileSync(receiptPath, "utf8"), "episode_state_hashes");
    const rep = JSON.parse(readFileSync(reportPath, "utf8"));
    const rSeedList = (rep.runs ?? []).map((r) => String(r.seed));
    const rSucc = (rep.runs ?? []).map((r) => r.success);
    const rSteps = (rep.runs ?? []).map((r) => r.steps);
    const seedSame = JSON.stringify(runA.episodes.map((e) => String(e.seed))) === JSON.stringify(rSeedList);
    const succSame = JSON.stringify(runA.episodes.map((e) => e.success)) === JSON.stringify(rSucc);
    const stepsSame = JSON.stringify(runA.episodes.map((e) => e.steps)) === JSON.stringify(rSteps);
    const hashSame = JSON.stringify(runA.episodes.map((e) => e.fingerprint)) === JSON.stringify(rHashes ?? []);
    rustAligned = seedSame && succSame && stepsSame && hashSame;
    console.log(`\n# 参照2 Rust 原生（${path.basename(RUST_REF)} · spec_hash ${rep.spec_hash}）:`);
    console.log(`#   seed band 一致:    ${seedSame ? "PASS ✓" : "FAIL ✗"}`);
    console.log(`#   逐集 success 一致: ${succSame ? "PASS ✓" : "FAIL ✗"}`);
    console.log(`#   逐集 steps 一致:   ${stepsSame ? "PASS ✓" : "FAIL ✗"}`);
    console.log(`#   逐集指纹逐位一致:  ${hashSame ? "PASS ✓" : "FAIL ✗"}`);
    if (!hashSame && rHashes) {
      console.log("#   逐值对照（wasm vs native）：");
      runA.episodes.forEach((e, i) => {
        const same = e.fingerprint === rHashes[i];
        console.log(`#     ep${i + 1} seed ${e.seed}: wasm ${e.fingerprint} native ${rHashes[i]} ${same ? "✓" : "✗"}`);
      });
    }
  }
}

// ── 参照 3：v2 金标锚（scheme-v2 三口径，PLAN-embodied-v3 §二 #17）──
// wasm fingerprint_v2() 与原生收集器同一 fold（state ⊕ robot identity ⊕
// spec_hash，经 eval::fingerprint_set）——对拍 v2 列，v1 列对拍锚内
// episode_fingerprints_v1（= 参照1 的口径，跨锚复核）。
let goldenV2Aligned = null;
if (existsSync(GOLDEN_V2)) {
  const g = parseWithU64Strings(readFileSync(GOLDEN_V2, "utf8"));
  const gSeeds = (g.seeds ?? []).map(String);
  const fp = g.fingerprints ?? {};
  const gV2 = (fp.episode_fingerprints_v2 ?? []).map(String);
  const gV1 = (fp.episode_fingerprints_v1 ?? []).map(String);
  const seedBandSame =
    gSeeds.length > 0 &&
    JSON.stringify(runA.episodes.map((e) => String(e.seed))) === JSON.stringify(gSeeds);
  const v2Same = JSON.stringify(runA.episodes.map((e) => e.fingerprintV2)) === JSON.stringify(gV2);
  const v1Same = JSON.stringify(runA.episodes.map((e) => e.fingerprint)) === JSON.stringify(gV1);
  const succSame = runA.episodes.filter((e) => e.success).length === g.successes;
  goldenV2Aligned = seedBandSame && v2Same && succSame;
  console.log(`\n# 参照3 v2 金标锚（${path.basename(GOLDEN_V2)} · engine_commit ${g.engine_commit} · scheme ${g.scheme}）:`);
  console.log(`#   seed band 逐集一致:   ${seedBandSame ? "PASS ✓" : "FAIL ✗"}`);
  console.log(`#   逐集 v2 指纹逐位一致: ${v2Same ? "PASS ✓" : "FAIL ✗"}`);
  console.log(`#   逐集 v1 指纹逐位一致: ${v1Same ? "PASS ✓" : "FAIL ✗"}（v2 锚内三口径的 v1 列）`);
  console.log(`#   成功率一致:           ${succSame ? "PASS ✓" : "FAIL ✗"}（${runA.episodes.filter((e) => e.success).length}/${EPISODES} vs ${g.successes}/12 = ${((g.success_rate ?? 0) * 100).toFixed(1)}%）`);
  if (!v2Same) {
    runA.episodes.forEach((e, i) => {
      const same = String(e.fingerprintV2) === gV2[i];
      if (!same) console.log(`#     ep${i + 1} seed ${e.seed}: wasm_v2 ${e.fingerprintV2} golden_v2 ${gV2[i]} ✗`);
    });
  }
}

// ── bundle 尺寸 ────────────────────────────────────────────────
const jsSize = statSync(path.join(VENDOR, "oriera_wasm.js")).size;
const wasmSize = statSync(path.join(VENDOR, "oriera_wasm_bg.wasm")).size;
const retired = ["rapier.mjs", "rapier_wasm3d_bg.wasm"].map((f) => path.join(DEMO_DIR, "vendor", f))
  .filter((p) => existsSync(p)).map((p) => statSync(p).size);
const retiredSum = retired.reduce((a, b) => a + b, 0);
const delta = wasmSize + jsSize - retiredSum;
console.log(`\n# bundle: vendor/oriera = ${(wasmSize / 1024).toFixed(0)} KiB wasm + ${(jsSize / 1024).toFixed(1)} KiB glue`);
console.log(retiredSum
  ? `# 退役: rapier.mjs+rapier_wasm3d_bg.wasm = ${(retiredSum / 1024 / 1024).toFixed(2)} MiB（demo 静态资源净变化 ${delta <= 0 ? "" : "+"}${(delta / 1024 / 1024).toFixed(2)} MiB）`
  : `# 退役: rapier 资产已删除（净变化 ${(wasmSize / 1024).toFixed(0)} KiB wasm + ${(jsSize / 1024).toFixed(1)} KiB glue）`);

// ── 结论 ───────────────────────────────────────────────────────
const alignedAny = goldenAligned === true || rustAligned === true || goldenV2Aligned === true;
const comparedAny = goldenAligned !== null || rustAligned !== null || goldenV2Aligned !== null;
console.log("\n# 结论:");
console.log(`#   wasm 内双跑逐位一致:  ${bitIdentical ? "PASS" : "FAIL"}`);
console.log(`#   与 Rust 参照逐位对齐: ${alignedAny ? "PASS（逐位）" : comparedAny ? "未逐位（逐值对照见上；口径说明 docs/WASM-RENDERER.md）" : "未比对（传 --rust-ref 或放置金标）"}`);
if (goldenV2Aligned !== null) {
  console.log(`#   v2 金标锚对拍（v1+v2 双列）: ${goldenV2Aligned ? "PASS（逐位）" : "FAIL（见参照3 逐值对照）"}`);
}
if (STRICT && !alignedAny) {
  console.error("# --strict：要求与 Rust 参照逐位对齐，未达标。");
  process.exit(1);
}
if (!bitIdentical) process.exit(1);
console.log("# OK（wasm 双跑逐位一致；参照比对口径见上）");
