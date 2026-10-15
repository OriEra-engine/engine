# Demo 模板（复制我，10 分钟做出一个新 demo）

把这个目录复制成 `examples/NN-your-demo/`，改三处标记了 `TODO` 的地方，
然后在根目录 `index.html` 加一张卡片，PR 就可以开了。

## 一个 demo 的自我修养

1. **讲一个层的本事**：标题写清演示哪一层（L1–L9）的什么能力
2. **零依赖零构建**：单个 `index.html`，浏览器直接打开（原生 WebGPU/Canvas/JS，可 CDN 引 Three.js）
3. **可交互**：至少一个滑块/按钮/输入框，不是录像
4. **可降级**：不支持 WebGPU 的浏览器给一句明确提示，不白屏（参考 01 的做法）

## 完成后的检查单

- [ ] `npx serve .` 打开 `examples/NN-your-demo/` 可玩，console 无报错
- [ ] 标题、说明、层徽章（`L? xxx`）齐全
- [ ] 根 `index.html` 加了卡片（复制现有卡片改字段）
- [ ] README demo 表和总数同步更新

## 目录约定

```
examples/NN-name/
├── index.html    # 全部代码都在这一个文件里
└── README.md     # 可选：超过 300 行 JS 时写思路说明
```
