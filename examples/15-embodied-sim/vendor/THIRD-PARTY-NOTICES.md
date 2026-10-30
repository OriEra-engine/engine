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
