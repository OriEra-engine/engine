# 13 · 生成式画廊 / Generative Gallery

小 3D AI 原生游戏的「表达」从哪来？这个 demo 给出引擎侧答案：
应用只说**一句主题**，`engine/genai.js`（`crates/oriera-genai` 的浏览器
镜像）里的 L8 生成系统产出天空穹顶 / 地面材质 / 三张海报（可选 Tripo
3D 道具），经校验后按**命名热替换**落进场景——不重建实体、不刷新页面。

## 运行

在仓库根目录执行：

```sh
python3 -m http.server 8000
```

打开 <http://localhost:8000/examples/13-genai-gallery/>。
ES module 需要 HTTP 服务，请不要双击 HTML 文件。

## 体验路径

1. 打开即见「引擎前 · 纯色」的世界，约半秒后 L8 自动装帧默认主题。
2. 点「表达对比」在纯色 / 生成两态间切换——这就是「渲染很差」的病因与解药。
3. 换预设主题或自己写一句（如「雪原极光科考站」），看五个槽位排队→落地。
4. ⚙ 里选智谱 CogView / OpenAI images / Tripo 并填 Key（只存 localStorage），
   再点「重新装帧」——真实文生图 / 文生 3D 热替换。没存过 Key 时会自动
   复用 demo 11 保存的智谱 Key。
5. 断网/坏 Key：槽位显示「失败→降级」，画面保持程序化/既有资产，不崩。

## 引擎边界（应用零模型代码）

| 角色 | 谁负责 |
|---|---|
| 提示词形状（天空该长什么样） | `ArtDirectorSystem`（引擎系统） |
| HTTP / Key / 厂商差异 | provider 适配器（CogView / Tripo / 程序化） |
| 字节校验（魔数 + 大小钳制） | `validateAsset`（↔ Rust `validate_asset`） |
| 落位（同名原位换芯） | `AssetStore.insertNamed` + `applyPending`（↔ L4） |
| 失败降级（占位也是表达） | `GenAiSystem`（↔ ADR 0003 判据 3） |

浏览器侧与 Rust 的唯一差异：fetch 的 Promise settling 取代
request/poll 轮询节拍；边界词汇（kind / capabilities / 校验 /
计量 / 占位降级）一一对照。契约见 `contracts/oriera/genai/v1`
（`TEXT_TO_IMAGE` 为 RFC 0001 加法）。
