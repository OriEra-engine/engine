#!/usr/bin/env node
/**
 * resolved spec 同步器（engine 侧 · P1-B2）。
 *
 * Studio 的 scripts/sync-engine-snapshot.mjs 在快照侧生成 specs/resolved/；
 * 独立浏览器页（examples/15-embodied-sim/）不吃快照，需要仓内自持一份
 * resolved/（浏览器里 wasm 没有文件系统，读不到 extends 的基 spec，
 * spec.rs 的 resolve_extends_in 走 std::fs）。本脚本把官方 spec 库展开成
 * 本目录的 resolved/<name>.json——展开不改身份：extends 键不进 hash，
 * spec_hash 仍以 crates/oriera-embodied/specs/INDEX.json 为准。
 *
 * 展开规则逐条移植 spec.rs merge_overlay：对象逐键合并、数组逐下标合并
 * （多出的追加）、其余替换；`kind` 标签不同 / 单键外部标签变体换了键
 * （{"fixed":…} 盖 {"range":…}）= 整体替换。EXTENDS_MAX_DEPTH = 8。
 *
 * 用法：node specs/sync-resolved.mjs           # 实生成（覆盖 resolved/）
 *       node specs/sync-resolved.mjs --check   # 只报告漂移，exit 1 = 漂移
 * （官方库变动后跑一次；CI 卡片墙数据自洽性由 --check 钉。）
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..", "..");
const officialDir = path.join(repoRoot, "crates", "oriera-embodied", "specs", "official");
const outDir = path.join(here, "resolved");

const check = process.argv.includes("--check");
const EXTENDS_MAX_DEPTH = 8;

const isObj = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const deepEqual = (a, b) => {
  if (Array.isArray(a) && Array.isArray(b))
    return a.length === b.length && a.every((x, i) => deepEqual(x, b[i]));
  if (isObj(a) && isObj(b)) {
    const ka = Object.keys(a);
    return (
      ka.length === Object.keys(b).length &&
      ka.every((k) => Object.hasOwn(b, k) && deepEqual(a[k], b[k]))
    );
  }
  return a === b;
};

function mergeOverlay(base, overlay) {
  if (isObj(base) && isObj(overlay)) {
    const kindChanges =
      Object.hasOwn(base, "kind") &&
      Object.hasOwn(overlay, "kind") &&
      !deepEqual(base.kind, overlay.kind);
    const bk = Object.keys(base);
    const ok = Object.keys(overlay);
    const variantChanges = bk.length === 1 && ok.length === 1 && bk[0] !== ok[0];
    if (kindChanges || variantChanges) return overlay;
    for (const k of ok) base[k] = Object.hasOwn(base, k) ? mergeOverlay(base[k], overlay[k]) : overlay[k];
    return base;
  }
  if (Array.isArray(base) && Array.isArray(overlay)) {
    overlay.forEach((ov, i) => {
      if (i < base.length) base[i] = mergeOverlay(base[i], ov);
      else base.push(ov);
    });
    return base;
  }
  return overlay;
}

function resolveExtends(v, depth = 0, chain = []) {
  if (!isObj(v) || !Object.hasOwn(v, "extends")) return v;
  const ext = v.extends;
  delete v.extends;
  if (typeof ext !== "string")
    throw new Error(`extends 须为字符串（官方 spec 名），当前 ${JSON.stringify(ext)}`);
  const name = ext.trim().replace(/^(?:official\/)+/, "").replace(/(?:\.json)+$/, "");
  if (!name) throw new Error("extends：名字为空");
  if (depth >= EXTENDS_MAX_DEPTH)
    throw new Error(`extends：继承链过深（> ${EXTENDS_MAX_DEPTH}）：${[...chain, name].join(" → ")}`);
  if (chain.includes(name)) throw new Error(`extends：循环继承：${[...chain, name].join(" → ")}`);
  const file = path.join(officialDir, `${name}.json`);
  if (!fs.existsSync(file)) throw new Error(`extends = "${name}"：官方库里没有（${officialDir}）`);
  chain.push(name);
  const base = resolveExtends(JSON.parse(fs.readFileSync(file, "utf8")), depth + 1, chain);
  return mergeOverlay(base, v);
}

const names = fs
  .readdirSync(officialDir)
  .filter((f) => f.endsWith(".json"))
  .sort();

const drift = [];
const errors = [];
const generated = new Map(); // name -> string
for (const f of names) {
  try {
    const resolved = resolveExtends(JSON.parse(fs.readFileSync(path.join(officialDir, f), "utf8")));
    generated.set(f, JSON.stringify(resolved, null, 2) + "\n");
  } catch (e) {
    errors.push(`- specs/official/${f} 展开失败：${e.message}`);
  }
}

for (const [f, text] of generated) {
  const dst = path.join(outDir, f);
  if (!fs.existsSync(dst)) drift.push(`- 新文件：specs/resolved/${f}`);
  else if (fs.readFileSync(dst, "utf8").replace(/\r\n/g, "\n") !== text)
    drift.push(`- 内容不同：specs/resolved/${f}`);
}
const have = fs.existsSync(outDir) ? fs.readdirSync(outDir).filter((x) => x.endsWith(".json")) : [];
for (const f of have) if (!generated.has(f)) drift.push(`- 多余文件：specs/resolved/${f}`);

if (errors.length) {
  console.error("✗ 官方 spec 展开失败（resolved/ 保持原样，不做半同步）：\n" + errors.join("\n"));
  process.exit(1);
}
if (check) {
  if (drift.length) {
    console.log(`漂移 ${drift.length} 项：\n` + drift.join("\n"));
    process.exit(1);
  }
  console.log(`resolved/ 与官方库一致：${generated.size} 份，零漂移。`);
} else {
  for (const [f, text] of generated)
    fs.writeFileSync(path.join(outDir, f), Buffer.from(text, "utf8"));
  console.log(`✓ resolved/ 已重生成：${generated.size} 份（${[...generated.keys()].join(", ")}）`);
}
