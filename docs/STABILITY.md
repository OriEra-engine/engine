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
  (wgpu,本机 Intel GL 后端实测出图;windows-gnu 下 DX12/Vulkan 枚举崩溃故默认钳制 GL;
  本机**链接** wgpu 测试二进制须 msvc-local 工具链——`windows` crate 的 raw-dylib
  导入库 GNU ld 链不了),Pass 图/材质系统/纹理/蒙皮尚未落地,且 Rust 引擎与浏览器
  demo 之间仍无编译关系;
  **相机矩阵与深度缓冲已随 #152 首刀落地**(2026-11-10):`oriera_render::camera::Camera`
  (GL 式 y-up/-z-forward,WebGPU NDC z∈[0,1],10 项闭式单测)+
  `HeadlessRenderer::render_mesh_view`(MVP uniform + Depth32Float 深度测试 Less+write;
  本机 GPU 实测:投影落点逐像素吻合 + 近面先画/远面后画仍被近面遮蔽)。
  旧 `render_mesh`/`render_mesh_shaded` 的 CPU 归一化路径行为冻结不变;
- L7 同步 `complete()` 仍是默认路径(离线规则后端零成本);异步作业层 `InferJobs` +
  认知/导演 `with_async()` 已落地(提交-收割,帧不阻塞网络,真 LLM 接入的壳层应启用),
  工具调用(OpenAI/Anthropic 双方言 wire)与 SSE 真流式已随 native-http 落地
  (不支持的后端 `supports_tools()=false` 诚实上报、默认降级纯文本);
  L9 工具桥 EngineTools 需 json feature(浏览器内核在 JS 侧实现同一循环);
- `platforms/web` 为 9 行壳,无 wasm 产物;
- GLB:`decode_glb_mesh`(冻结:首 mesh 首 primitive)之外,#152 第二刀落地
  `decode_glb_scene`(2026-11-10)——全 mesh×primitive、节点图 TRS/matrix 世界变换、
  材质摊位(baseColorFactor/metallic/roughness + baseColorTexture 原始字节;
  PNG 解码走 oriera-asset feature `texture`,png crate 门控默认关)、accessor
  byteOffset 与交错布局支持(真实文件常态;冻结路径维持拒绝)。显式拒绝:skins、
  Draco、specularGlossiness(替换 baseColor 模型)、JPEG/外部图片 URI、节点环。
  L5 `render_scene_views` 逐 primitive 纹理×兰伯特渲染(fp32 不适用——本路径 unorm8
  目标,单层材质无叠加,不涉泼溅的累积混色),CC0 WaterBottle 实测出图
  (docs/media/gltf-scene-waterbottle.png,4.7ms 解码/40ms 渲染)。
  #152 第四刀落地 **PBR 最小着色**(2026-11-10):Cook-Torrance(GGX+Smith-Schlick+
  Schlick 菲涅尔)+ 单方向光 + 半球环境替身;MR 打包纹理(G=粗糙 B=金属)消费,
  无 MR 纹理时 metallic 视作 0(无 IBL 金属黑兜底);无 IBL/预积分环境/聚光/点光。
  仍未解码:蒙皮动画、IBL、mipmap;Duck 官方样例为 SCEA 共享源许可,
  按「许可先行」红线不入库(参考 KhronosGroup/glTF-Sample-Models 2.0/Duck);
  生成→落地→解码→GPU 出图全链已闭环(示例 `glb_headless --from-l8`,3 项集成测试)。
- demo 13 已**原生渲染**(2026-11-10,#152 第五刀):generate gallery 摘除 three.js
  (vendor 1.5MB+utils 移除),裸 WebGL2 重写(穹顶/地面/三墙/挂画/展台/TorusKnot 道具 +
  双方向光兰伯特×纹理 + 指数雾 + 背面剔除);L8 genai.js 镜像零改动,槽位契约
  (sky.dome/ground.albedo/poster.1-3/hero.prop)不变,纹理热替换=GL 纹理直写,
  GLB 道具经 engine/gltf.js(与 oriera-asset::glb 金样对拍)解码逐 primitive 绘制;
  诚实边界:Tripo GLB 仅消费 baseColor 因子纹理,骨架动画道具不支持(等 #153)。
- demo 05 已**原生渲染**(2026-11-10,#152 第三刀):JS 镜像 `engine/gltf.js` 与
  decode_glb_scene 公式同源、双端金样 `contracts/golden/gltf_golden.json` 对拍
  (变换顶点/法线容差 1e-4,索引/材质摊位精确),裸 WebGL2(sRGB 纹理×兰伯特+
  栅格线)替代 three.js——本 demo 零外部依赖、完全离线;Duck 仍为运行时拉取
  (SCEA 许可不入库)。demo 11/13 的 three.js 为场景渲染用途,摘除属后续刀次。
- `oriera-splat`(L5,PLAN-splat v2 M0,契约 `oriera.render.splat.v1` 与 L4 `oriera.asset.splat_ply.v1`
  均标 unstable)是**实验性 CPU 参考内核**:只有 EWA 投影(相机进入 splat 内部会失真,射线类方法未实现),
  颜色只用 SH DC 项(无视角相关光照);PLY 只认 binary_little_endian 且 vertex 为首个 element,
  压缩 PLY/SPZ/RAD 未解码(一律显式报错);CPU 光栅受 `RasterBudget` 约束(默认 5 千 splat、
  1280×720,超限返回 `OverBudget`,绝不静默降级)。**非实时**——本机 release 单线程实测(criterion):
  排序 1 万/10 万/100 万 splat = 0.19/3.2/39 ms,投影 10 万 = 7.1 ms,解码 10 万 = 5.4 ms,
  5 千细长 splat 光栅 640×360 = 91 ms、1280×720 = 343 ms;CC0 真扫描样本(10,330 splat,SH 2 阶)
  960×540 出图 92 ms。观测触发:排序超过 2 ms/帧(约 6 万 splat)即提级 GPU radix(PLAN-splat M3)。
  GPU 路径已落地(PLAN-splat M1,feature `splat-wgpu` 默认关,消费 #152 底座
  `oriera_render::camera::Camera` 经 `SplatCamera::from_view` 换算):确定性数学
  (投影/排序)仍在 CPU,GPU 只做实例化四边形展开 + 后到前 over 混合(fp32 目标——
  unorm8/fp16 的每层混合舍入会在几十层叠加的热点像素累积超差,实测踩过);
  与 CPU 参考按评审门槛互证(本机 GPU 实测:petal_ring 600 满足 99.9% 像素每通道
  ≤2/255 且 RMS ≤1/255;倾斜 splat 交叉项 ≤2;单 splat 逐位一致)。CI 不编译任何
  splat GPU 代码到默认腿(windows-check 腿 check+clippy);
  不读也不置 `NEURAL_RENDER_3DGS`(经典泼溅属 ADR 0002 的 passthrough 侧)。
