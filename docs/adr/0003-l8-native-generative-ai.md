# ADR 0003: L8 生成式 AI 内生化——供应商中立的资产生成边界

- 状态：已接受（骨架先行）
- 日期：2026-10-15
- 决策者：架构组 + 社区治理组
- 关联 RFC：Ideas 区《L8 接口边界与"内生"定义》（7 天评论期）

## 背景

ROADMAP 2028 原定"L8 文生 3D 管线：Tripo/Meshy 适配器 + 几何修复"。提前
启动的直接动因：L4 `insert_named` 命名热替换（v0.2 润滑剂）已把落地点
铺好，`ARCHITECTURE.md` 明文写着它是"L8 落地点"；而 L7 的
`ModelProvider` 已确立"**引擎拥有模型边界，应用永不接触密钥与 HTTP**"
的哲学（AI-NATIVE.md）。缺的只是把同一哲学推广到"生成资产"这一品类。

同时有两条红线必须遵守：

1. ROADMAP"不做"清单：**不绑定单一云厂商的私有 API**；
2. 治理共识：单人仓库不接受千行级大 PR，骨架必须小步落地、离线可测。

## 决策

### 1. 新建 `crates/oriera-genai`（L8），只做骨架

- **`GenerationProvider` 边界**：`request_generation` / `poll_task` /
  `capabilities`，镜像 L7 `ModelProvider` 的形态（含确定性离线后端
  `MockProvider`，同 `RuleProvider` 定位）。生成是异步任务制（Tripo 与
  Seedance 均为"提交任务 + 轮询"），接口按任务语义设计。
- **`validate_asset` 校验**：GLB/USDZ/MP4/PNG/JPEG 魔数嗅探 + 尺寸上限
  + GLB chunk 长度一致性。深度校验（网格合法性、zip 嵌套深度）随 v0.2
  资产解码落地——**在解码器存在之前，校验只承诺它已实现的深度**。
- **引擎环集成**：`GenAiSystem`（`System<World>`）帧内限量的任务轮询 →
  校验 → 写入 `PendingSwaps` 资源 + `AssetLanded`/`AssetFailed` 事件；
  平台壳（持有 `AssetStore` 的一方）调用 `hot_swap::apply_pending` 完成落地。
  落地路径不断供：任务失败时按配置回退**占位网格**（合法的最小 GLB），
  计入 `degraded` 计量——断供降级，不崩溃。
- **计量与限流**：`GenAiMeter`（submitted/succeeded/failed/degraded/
  bytes_in）与 `GenAiLimits`（max_bytes、max_in_flight、poll_interval_ticks，
  一律钳制——幻觉参数不得炸世界，同 `TimeScale` 哲学）。

### 2. "内生"的验收标准（五条，全部满足才算内生）

1. 生成物经 **L4 命名资产**落地（`insert_named`，同句柄原位换芯）；
2. **L9 Agent 可调用**（规则版 `parse_generation` 意图解析，LLM 版随
   v0.5–v0.7）；
3. **能力位可降级**：无供应商/断供/超限 → 占位资产或排队，引擎不崩；
4. **契约进 `contracts/`**：`oriera.genai.v1` proto 可被跨端校验；
5. **引擎核心零 vendor SDK**：适配器只是协议实现，传输经 `Transport`
   trait 注入，密钥只存在于平台壳（浏览器 localStorage / 环境变量），
   永不进入引擎层、世界状态或仓库。

### 3. 供应商适配 = 协议 adapter，feature 门控

`tripo.rs` / `seedance.rs` 为单文件协议适配（请求构造 + 响应解析 + 任务
状态机），置于 `providers` feature 之后，**不进默认构建**（Wasm 体积预算，
见 L08 manifest）。端点/版本全部配置化：

- Tripo：现行 `POST /v2/openapi/task` + 轮询。官方已公告 V2 端点
  **2026-11-01 退役**；端点版本是配置项，V3 迁移应当是一次 diff。
- Seedance（火山 Ark 兼容）：`POST /api/v3/contents/generations/tasks`
  （content 数组）+ `GET .../tasks/{id}` 取视频 URL。定位为**视频资产**
  provider 与未来图生 3D 的素材源（Seedance 是视频生成模型，非 3D）。

### 4. 合规与披露

Tripo 与 Seedance 均为**按量计费的云端服务**：费用、配额与 ToS 责任在
使用者；引擎不代管密钥、不默认启用任何远程供应商（默认 `mock`）。示例
与文档必须在体验路径上明示这一点。

## 后果

- ✅ ROADMAP 2028 的 L8 项提前获得可测试骨架，2028 只剩"做深"（几何修复、
  六段式流水线、Meshy 等 adapter）。
- ✅ 与 L7 完全同构的心智模型：`create_provider` ↔ `create_generation_provider`，
  `RuleProvider` ↔ `MockProvider`，学习成本为零。
- ⚠️ 骨架期的 `GenAiSystem` 轮询是同步 tick 内的有限工作（帧预算切片），
  真正的异步执行位（端云路由、任务编排）随 L7 2027 H2 演进，届时接口可能
  需要一次 minor 修订（proto 已留状态机空间）。
- ⚠️ Tripo V2 退役在即（2026-11-01）：adapter 的 fixture 是按当前公开文档
  形状手写的，接线时必须对照官方 changelog 校验，必要时升 `v2` → `v3`。
- ⚠️ 本 ADR 提前启动 ROADMAP 排期，属于范围变更——已按治理流程在 Ideas
  区开 RFC 征询评论；若社区否决，回退成本仅为删除一个 crate 与两份 manifest。

## 参考

- `crates/oriera-genai/`（骨架实现）
- `contracts/oriera/genai/v1/genai.proto`
- `manifests/L08-genai.yaml`
- `crates/oriera-infer/src/provider.rs`（被镜像的 L7 边界）
- `crates/oriera-asset/src/lib.rs`（`insert_named` 落地点）
- ADR 0001（能力位图）、ADR 0002（神经渲染运行时不变量）
- AI-NATIVE.md（"AI 是引擎系统，不是应用代码"）
