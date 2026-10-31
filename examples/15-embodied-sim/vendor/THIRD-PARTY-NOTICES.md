# Third-party notices — examples/15-embodied-sim/vendor

Vendored browser assets for the embodied sim demo (not authored by
OriEra; versions pinned at vendoring time, 2026-10-29):

| File | Project | Version | License | Source |
|---|---|---|---|---|
| `three.module.js` | three.js | r160 | MIT © 2010-2023 three.js authors | https://github.com/mrdoob/three.js (SPDX header in file) |
| `rapier.mjs` | rapier3d-compat | 0.21.0 | Apache-2.0 © 2019-2024 Dimforge | https://github.com/dimforge/rapier (npm @dimforge/rapier3d-compat) |
| `rapier_wasm3d_bg.wasm` | rapier3d (wasm-bindgen build) | 0.21.0 | Apache-2.0 © 2019-2024 Dimforge | same npm package |

Re-vendor with: `npm pack @dimforge/rapier3d-compat@<ver>` (dist/rapier.mjs
+ dist/rapier_wasm3d_bg.wasm) and the matching three.js module build.

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
