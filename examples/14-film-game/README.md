# Demo 14 · 影游内核（film-game kernel）

L9 叙事原语（`oriera-agent/story.rs`）的浏览器镜像 + 构建期资产清单协议。
源头实证：互动影游《末班站台》（`interfilm/`，M0→M2.1 已交付，¥90/一天半）。

- `engine/story.js` —— 与 Rust 逐句对照：schema / `applySets` / `evalWhen` /
  `resolveHub`（first-true-else-default）/ `validate`（同一发现集）/
  `saveFingerprint`（**跨语言黄金值 `14559479683769148579` 双侧锁定**）/
  `atmosphere` / `choiceMemories`
- `index.html` —— 内核契约自检（双击即跑，全绿 = Rust↔JS 零漂移）
- `tools/story_manifest.mjs` —— 资产清单协议：扫描 `assets/{clips,vo,3d}` →
  `player/assets-manifest.json`；播放器按清单真媒体渲染，缺失走氛围兜底

真播放器（视频/配音/倒计时/LLM 彩蛋电话）在 interfilm 仓外实现，
批 3 时回收进 Studio 的「影游模板」运行时。
