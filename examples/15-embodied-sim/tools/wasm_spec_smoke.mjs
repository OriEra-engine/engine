//! 新 wasm 冒烟：金标 spec 加载 + reset + spec_hash 对拍（node 直跑）。
import init, { OrieraSim } from "../vendor/oriera/oriera_wasm.js";
import { readFileSync } from "node:fs";

await init({ module_or_path: readFileSync(new URL("../vendor/oriera/oriera_wasm_bg.wasm", import.meta.url)) });
const spec = readFileSync(new URL("../../../crates/oriera-agent/specs/tabletop-4dof-pick.json", import.meta.url), "utf8");
const sim = OrieraSim.from_spec_json(spec);
const obs = sim.reset(1000);
console.log("loaded ok | spec_hash", sim.spec_hash(), "| obs", obs.length, "| task:", sim.task_phrase());
if (sim.spec_hash() !== "c90869dcf6a04875") throw new Error("spec_hash mismatch");
if (obs.length !== 21) throw new Error("obs dim mismatch");
console.log("SMOKE PASS");
