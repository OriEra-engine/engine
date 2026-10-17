# RFC 0001 — L8 契约增补 `TEXT_TO_IMAGE`（文生图）

- 状态 / Status: **已接受 / Accepted**（2026-10-17）
- 影响契约 / Contract: `oriera.genai.v1`（`GenerationKind` 枚举、`ProviderCapabilities` 消息）
- 性质 / Nature: **加法式变更，停留在 v1，不 bump major**
- 关联 / See also: ADR 0003、PR（feat(genai): text-to-image + CogView adapter + JS kernel mirror）

## 动机 / Motivation

L8 落地时（ADR 0003，v0.3.0）只定义了 mesh 与 video 两族生成种
类。做小 3D AI 原生游戏的反馈表明：**对渲染表达影响最大的生成
能力是"文生图"**——天空穹顶、地面反照率、墙面海报、招牌、
sprite 图集全部是 still 图。缺这个种类时，上层只能把生成结果
绕过 L8 边界自行拼接（违反"生成是引擎系统"的定位），或干脆
用纯色材质（渲染观感差的直接原因）。

## 变更内容 / Proposed Change

1. `enum GenerationKind` 新增 `TEXT_TO_IMAGE = 5;`
2. `message ProviderCapabilities` 新增 `bool text_to_image = 5;`
3. `AssetFormat::IMAGE`（PNG/JPEG，v1 已有）作为该种类的产出格式，
   不新增格式值。
4. 降级占位语义随之细化（实现层，不动契约）：占位资产必须与请求
   种类的期望格式匹配——图片槽位失败落一张真实可解码的最小 PNG，
   而不是空网格 GLB。

## 兼容性分析 / Compatibility

proto3 语义下均为**旧读取方可安全忽略**的加法：

- 旧读取方收到 `TEXT_TO_IMAGE = 5` 的枚举字段：proto3 保留未知
  枚举数值（open enum 语义），不会丢消息；
- 旧读取方收到 `text_to_image` 未知字段号：按 unknown field 忽略；
- 新读取方对旧写入方：字段缺省即 `false` / `UNSPECIFIED`，
  `GenAiSystem` 对 `UNSPECIFIED` 按不支持处理（fail-fast，
  现有行为）。

无字段号重排、无语义改写、无删除 → **不需要 bump major**。

## 备选方案 / Alternatives Considered

- **开 v2 契约**：为一个加法付出双契约维护成本，收益为零。
- **用 `TEXT_TO_VIDEO` 复用 + 上层抽帧**：语义谎言（把图说成
  视频），且把抽帧成本推给每个上层。
- **不动契约、只在实现层支持**：违反「实现与契约一一对照」的
  仓库纪律（manifest 引用同一 id）。

## 后果 / Consequences

- `ProviderKind::CogView`（OpenAI images 方言，默认智谱开放平台）
  作为第一个 `text_to_image` 适配器进入 `providers` feature；
- 浏览器内核镜像（demo `engine/genai.js`）与 Rust 同步获得该
  种类——双侧对照是仓库的既定纪律；
- 未来若加 `IMAGE_TO_IMAGE`（垫图重绘），按同一加法流程另行 RFC。
