#!/usr/bin/env node
// story_manifest.mjs — 构建期资产清单（manifest 协议，从 interfilm inject 抽象）
// 扫描 <root>/assets/{clips,vo,3d} → 生成 <root>/player/assets-manifest.json：
//   { clips: { "s001_0": "../assets/clips/s001_0.mp4", ... },
//     vos: { "s002a_0": "../assets/vo/s002a_0.mp3", ... },
//     meshes: { "umbrella": "../assets/3d/umbrella.glb", ... } }
// 播放器按清单决定真媒体 vs 氛围兜底（缺失回退语义见 POSTMORTEM P1/P5）。
// 用法: node story_manifest.mjs <项目根>   （缺省当前目录）
import { readdirSync, writeFileSync, existsSync, mkdirSync } from "node:fs";
import { join, resolve } from "node:path";

const root = resolve(process.argv[2] ?? ".");
const scan = (dir, ext) => {
  const p = join(root, "assets", dir);
  if (!existsSync(p)) return {};
  return Object.fromEntries(
    readdirSync(p).filter((f) => f.endsWith(ext)).map((f) => [
      f.slice(0, -ext.length), `../assets/${dir}/${f}`,
    ]),
  );
};
const manifest = {
  clips: scan("clips", ".mp4"),
  vos: scan("vo", ".mp3"),
  meshes: scan("3d", ".glb"),
};
const dir = join(root, "player");
mkdirSync(dir, { recursive: true });
const out = join(dir, "assets-manifest.json");
writeFileSync(out, JSON.stringify(manifest, null, 1));
const total = Object.values(manifest).reduce((a, m) => a + Object.keys(m).length, 0);
console.log(`✓ assets-manifest.json：${total} 项（clips ${Object.keys(manifest.clips).length} / vos ${Object.keys(manifest.vos).length} / meshes ${Object.keys(manifest.meshes).length}）→ ${out}`);
