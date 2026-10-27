# OriEra 稳定性政策 / Stability Policy

> 本文件回答两个问题:**这门引擎在什么承诺下保持稳定**(契约/行为/数据),以及**它靠什么持续**(可维护性承诺)。面向三类读者:上层开发者(L9/L11)、合作伙伴(模型厂商/平台)、贡献者。
> 状态:生效(v0.5.1 起,2026-10-20)。修订走 RFC。

## 一、稳定性承诺(什么不会坏)

### 1. 契约版本化
- 跨语言契约(`contracts/` Protobuf、各层 trait 对应的语义版本)只做**加法式变更**;破坏性变更必须升 major 并与旧版并存至少一个 minor 周期(RFC 0001 先例:加枚举值不 bump major)。
- trait 默认方法是唯一的例外通道:新增带默认实现的方法不算破坏(`degradation_note()` 先例)。

### 2. 行为不变量(测试锁死,CI 门禁)
- **确定性回放**:同种子同输入同世界状态(LCG 回放、命令合并位级等价测试)。
- **降级不崩溃**:任一层缺失/断供/超限时落占位或规则后端,引擎不崩(ADR 0003 准则 3)。
- **降级必须可见**:顶替真实后端的规则/占位实现必须通过 `degradation_note()` 等接口暴露降级事实——**静默降级视为 bug**(测试 `degrade_is_visible_not_silent` 锁死)。
- **预算钳制**:用户配置的预算经单一 `effective()` 入口钳制,模型幻觉出的极值不会变成政策。

### 3. 错误处理政策
- 对外错误一律折叠为 `ErrorClass` 八桶公共分类;重试策略由 [`ErrorClass::retryable`] 决定:**传输瞬态**(rate_limited/server/network/timeout)可退避重试或走槽位降级链;**策略与配置类**(content_moderation/auth/quota)禁止同载荷重试。
- 上层永远不解析厂商错误码——厂商码在各 adapter 内映射,保证换厂商不改上层。

### 4. 数据格式
- Studio 项目数据(game/story/agents/provider JSON)变更必须向后兼容读取;写旧读新不做承诺。
- 存档/剧情历史(`.history/`)只增不删(自动保留最近 10 份的滚动策略不变)。

## 二、持续性承诺(靠什么活着)

### 1. 工程纪律
- 每层 crate 维持内联单测(当前 180+),`cargo fmt --check` + `cargo test` 是 CI 门禁,main 永远绿。
- 文档诚实义务:ARCHITECTURE.md 状态列(✅骨架/✅可用/📋规划)必须与代码事实一致;宣称未实现的能力视为文档 bug(先例:ar_glasses 目录未建已在文档标注)。

### 2. 发布与支持
- 发版节奏见 GOVERNANCE(里程碑驱动,休眠公告义务);bugfix 落在最新 minor,不做多分支长期支持。
- 安全漏洞走 SECURITY.md 私密披露通道;涉密钥/网络层的修复优先级最高。

### 3. 依赖与供应链
- 引擎核心零 vendor SDK(红线);第三方 crate 仅取社区主流且维护活跃者;升级走独立 PR + 全量测试。
- 演示资产仅用 CC0/许可证明确素材。

### 4. 知识传承(抗单点)
- 每层必须有 ARCHITECTURE.md 行级说明 + 内联测试作为"可执行规格";
- 关键决策全部留痕 ADR/RFC,新维护者可从文档重建上下文;
- BDFL 连续 6 周不可用时,Core Team 按 GOVERNANCE 接管发版(见休眠公告条款)。

### 5. B8 模型验收门禁(issue #99)

模型或预设行升「推荐位」前必须通过的行为套件,入口是**一个**显式命名的测试:

- **门禁入口**:`provider::b8_gate::b8_gate_smoke_reference_backend_clears_all_legs`
  (crates/oriera-infer/src/provider.rs,离线零网络,进 CI workspace 腿)——
  确定性底线 / usage 计量底线 / 流式拼接诚实 / 降级链韧性四腿聚合冒烟;
- **深度覆盖**(不在此重复,由以下常驻测试指认):结构化输出线格式与
  fail-fast(`invalid_schema_fails_before_any_network_or_key_spend`、
  `schema_reaches_the_wire_as_json_schema_and_supersedes_json_mode`)、
  工具往返(`openai_tool_calls_round_trip`、`anthropic_tool_use_round_trip`
  及多轮线上测试)、缓存方言(`deepseek_style_cache_hits…`、
  `anthropic_cache_read_parses_and_a_lying_vendor_is_clamped` 等三方言)、
  降级链政策(`health::tests` 八条,含凭据连击开闸);
- **预设表登记完整性**:预设单一来源 `contracts/golden/presets.json`(#98),
  Rust 与浏览器内核逐行对照;新增/改行不更新 fixture 即双端 CI 同红;
  能力与牌价登记语义由 `caps_and_prices_are_registered_where_claimed` 锁定。

裁定记录(2026-10-22):行为四腿在 #92–#95 已高度覆盖,B8 的实际增量是
「显式门禁入口 + 登记单一来源」,不再新开 CI 独立腿(native-http 腿已常驻
全部深度测试),避免 CI 时长膨胀。

## 三、已知不稳定边界(诚实清单)

以下能力**尚未达到上述承诺**,使用前须知:
- L5 默认构建仍为计数占位;真 GPU 光栅化已通过 `render-wgpu` feature 无头闭环
  (wgpu,本机 Intel GL 后端实测出图;windows-gnu 下 DX12/Vulkan 枚举崩溃故默认钳制 GL),
  但 Pass 图/材质/纹理/相机矩阵尚未落地,且 Rust 引擎与浏览器 demo 之间仍无编译关系;
- L7 同步 `complete()` 仍是默认路径(离线规则后端零成本);异步作业层 `InferJobs` +
  认知/导演 `with_async()` 已落地(提交-收割,帧不阻塞网络,真 LLM 接入的壳层应启用),
  工具调用(OpenAI/Anthropic 双方言 wire)与 SSE 真流式已随 native-http 落地
  (不支持的后端 `supports_tools()=false` 诚实上报、默认降级纯文本);
  L9 工具桥 EngineTools 需 json feature(浏览器内核在 JS 侧实现同一循环);
- `platforms/web` 为 9 行壳,无 wasm 产物;
- GLB 解码取 POSITION/NORMAL/三角索引 + 材质基础色 baseColorFactor(深校验;
  顶点兰伯特光照已闭环,像素级验证),纹理贴图/蒙皮/KHR 扩展未解码;
  生成→落地→解码→GPU 出图全链已闭环(示例 `glb_headless --from-l8`,3 项集成测试)。
