# Third-party notices — examples/15-embodied-sim/vendor

Vendored browser assets for the embodied sim demo (not authored by
OriEra; versions pinned at vendoring time, 2026-10-29):

| File | Project | Version | License | Source |
|---|---|---|---|---|
| `three.module.js` | three.js | r160 | MIT © 2010-2023 three.js authors | https://github.com/mrdoob/three.js (SPDX header in file) |
| `oriera/oriera_wasm_bg.wasm` | OriEra engine (`oriera-wasm`, compiles `oriera-subsys`/rapier3d 0.36) | 0.10.0 | Apache-2.0 © 2026 OriEra; rapier3d Apache-2.0 © 2019-2024 Dimforge | built from this repo — rebuild: `cargo build -p oriera-wasm --target wasm32-unknown-unknown --release` + glue (docs/WASM-RENDERER.md §2) |

> 2026-11-02（M1）：rapier3d-compat 0.21 的 `rapier.mjs`/`rapier_wasm3d_bg.wasm`
> 已退役删除——物理改由仓库自身编译的 `oriera_wasm_bg.wasm`（同一份
> Rust multibody 代码，native 与浏览器同源）。Re-vendor three.js 用
> `npm pack three@0.160` 取 module build；wasm 重建见上。

## Vendored examples/jsm modules (three.js r160)

The following 15 files under `vendor/jsm/` are from the three.js r160
`examples/jsm/` directory (MIT © 2010-2023 three.js authors):

| Path | Purpose |
|---|---|
| `jsm/post/EffectComposer.js` | Post-processing pipeline |
| `jsm/post/RenderPass.js` | Scene render pass |
| `jsm/post/ShaderPass.js` | Shader pass |
| `jsm/post/Pass.js` | Base pass + FullScreenQuad |
| `jsm/post/MaskPass.js` | Clear/mask pass |
| `jsm/post/UnrealBloomPass.js` | Bloom post-processing |
| `jsm/post/OutputPass.js` | Tone-mapping + color-space output |
| `jsm/post/SSAOPass.js` | Screen-space ambient occlusion |
| `jsm/shaders/CopyShader.js` | Copy shader |
| `jsm/shaders/LuminosityHighPassShader.js` | Bloom luminance threshold |
| `jsm/shaders/OutputShader.js` | Output shader |
| `jsm/shaders/SSAOShader.js` | AO shader |
| `jsm/loaders/RGBELoader.js` | HDR environment loader |
| `jsm/math/SimplexNoise.js` | SSAO noise |
| `jsm/environments/RoomEnvironment.js` | Procedural environment |
