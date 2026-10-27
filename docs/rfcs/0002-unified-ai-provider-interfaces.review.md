# RFC 0002 附件 — 统一 AI 接口：LLM 生态合作方视角评审（DeepSeek / Kimi / GLM / 字节）

- 状态 / Status: **评审记录 / Review**（2026-10-20，厂商事实当日核实）
- 评审对象 / Reviews: [RFC 0002](0002-unified-ai-provider-interfaces.md)
- 结论 / Verdict: 可落地，附四条阻塞条件（计量一致性/错误可观测/清单时效/合规通道）——P0① `ErrorClass` 已随本次提交落入 `oriera-infer`

---

# RFC-3 合作方视角评审:LLM 生态(DeepSeek / Kimi / GLM)可落地性与优化方案

> 评审视角声明:以模型厂商解决方案/开发者关系团队的标准评审——"我们是否愿意成为该引擎的一等供应商?接入成本多高?长期运维负担?商务模式是否成立?"(2026-10-20,厂商事实已核实,来源见文末)

## 一、总体判定:可落地,附四条阻塞条件

架构对厂商是"低门槛接入、难误用"的:

- **OpenAI 兼容优先**(`OpenAiCompat` 一类)→ DeepSeek/Kimi 零成本接入,无需为引擎写 SDK;
- **零 vendor SDK + 传输注入** → 厂商贡献的是一份 JSON 预设,不是代码,PR 门槛极低;
- **槽位路由** → 厂商按强项占槽(免费档抢 npc_dialog、长上下文抢 director),这是厂商愿意 co-marketing 的流量结构;
- **BYOK + 密钥只在平台壳** → 密钥责任清晰,厂商无代收与盗用风险;
- **RuleProvider 离线兜底** → 厂商故障时引擎自动降级,不会把用户卡死在报错页(降低厂商舆情压力)。

四条阻塞条件(不解决,合作走不完合规/技术评审):

1. **计量一致性**:流式调用必须启用 `stream_options.include_usage`(OpenAI 兼容系),否则引擎无法计量流式消耗 → 用户账单与厂商账单对不上 → 工单打到厂商。有厂商 usage 块时**绝不本地估算**(现状 chars/4 仅限 RuleProvider,保持住)。
2. **错误可观测**:厂商错误码归一为公共分类(rate_limited / content_moderation / auth / quota / server / timeout,如智谱 1301 类内容违规)+ **request_id 透传**到宿主日志。用户投诉先打到厂商客服,没有 request_id 厂商无法定位。
3. **manifest 时效**:硬编码预设必然腐化——**实证**:Moonshot 开放平台域名已从 platform.moonshot.cn 迁至 platform.kimi.com;智谱旗舰已从 glm-4.6 演进到 GLM-5.3(引擎注释里的 glm-4.6 已过时);DeepSeek 已到 V4 系。需要:`GET /v1/models` 探活 + **厂商可 PR 的 preset 注册表** + UI 价格列标注"以厂商定价页为准"(引擎不背书价格,价格错了用户骂的是厂商)。
4. **合规通道**:游戏内 NPC 对话/生成剧情是典型的生成式 AI 监管场景。引擎侧需要:AI 生成内容标识通道(《人工智能生成合成内容标识办法》已施行)、内容安全错误 → 友好降级(接 RuleProvider,已有)、未成年保护位。厂商侧合规评审会问这三项。

## 二、逐家画像(按槽位对号)

| 厂商 | 2026-10 现状(核实) | 最适配槽位 | 合作要点 |
|---|---|---|---|
| DeepSeek | V4 系:V4-Flash ~$0.14/M;V4.1-Flash 2026-09 GA(缓存命中更低价);错峰折扣 2026-08 起扩到周末;V4-Pro 促销 75 折转永久 | codegen(代码强)+ 预生成批处理通道 | 错峰/周末调度对引擎的夜间预生成管线是天然折扣;开源权重 → Ollama 本地轨 |
| Kimi (Moonshot) | 平台迁 platform.kimi.com;K2.6 视觉+文本、256K 上下文、¥6.5/M 入 ¥27/M 出、缓存命中折扣;K3 传闻 1M 上下文 | director(世界书/剧情一致性靠长上下文) | **预设域名必须更新**(manifest 时效的实证案例);K 系列开源权重本地轨 |
| GLM (智谱) | Flash 档免费(GLM-4-Flash / 4.5-Flash / 4.7-Flash;**GLM-4.6V-Flash 免费多模态 128K**);旗舰 GLM-5.3 ~$1.40/$4.40;另有 Coding Plan 订阅 | npc_dialog(零成本)+ **reviewer 视觉审校(4.6V-Flash 免费截图评审)** | 引擎专设 Zhipu kind 正确(国内通道 + V4 细节);免费视觉档让"生成-评审对抗回路"零成本搭起 |

三家共同点:全部 OpenAI 兼容 → `OpenAiCompat` 覆盖判断成立;智谱专 kind 保留。

## 三、优化方案

### P0(阻塞项,v0.5 必做,全部在 adapter 层)
1. 错误分类学 + request_id 透传(公共 enum,厂商码在各自 adapter 内映射);
2. 流式 usage 铁律(`include_usage`;有 usage 块不估算);
3. 引擎 UA 标识:`oriera-engine/0.x; slot=director`——厂商识别流量、提供支持的依据,也是其评估"该生态值不值得投 devrel"的数据;
4. 合规标识通道(对话/资产打 AI 生成标)+ 内容安全错误的友好降级文案。

### P1(v0.6–v0.7,提升合作质量)
5. **降级遥测**:降级原因分类(我方 429 / 我方 5xx / 内容 / 超时)进宿主日志——厂商复盘流量流失需要,也是商务谈判素材;
6. **错峰/批量通道**:预生成管线(剧情批量、NPC 语料、资产任务)调度到错峰/周末窗口(DeepSeek 已明示周末错峰),成本约省一半;挂 InferSystem 容量调度旁,不新造调度器;
7. **缓存友好 prompt 组装规范**(进 L9 指南):稳定系统前缀、动态内容后置、禁止在头部插时间戳/随机 id → 命中 DeepSeek 自动缓存 / Kimi 缓存命中折扣 / GLM context cache——不改协议,纯组装纪律,直接降单价;
8. **模型生命周期位**(preview / stable / deprecated / sunset)进 manifest 与 UI 矩阵:Tripo V2 式退役(Tripo 官方 2026-11-01 退役 V2)让引擎被动,也让厂商用户关系受损;厂商发布退役时间线,引擎提前黄条预警;
9. preset 注册表开放厂商 PR + `/v1/models` 探活双通道,价格外链厂商页。

### P2(商务层,不动引擎代码)
10. **双接入模式**:BYOK(现状)+ 平台代理键(引擎代收计费、与厂商返佣)——厂商关心结算粒度与流量预测,不关心引擎内部;
11. **默认槽位治理**:默认推荐位是商业地产;建议公开基准(TTFT/成本/质量)轮换 + 结果公示,**拒绝付费占位**——中立性是引擎对多家厂商的最大吸引力,一旦破例即贬值;
12. 本地开源权重轨:pin 官方量化版本 + 许可证标注 + 与云端版能力差异明示(避免用户拿量化版效果骂云端 API)。

## 四、落地排期(对齐 RFC-3 分期)

| 期 | 内容 | 层 |
|---|---|---|
| v0.5 | P0 四项(错误分类学/request_id、流式 usage、UA、合规标识) | L7 adapter |
| v0.6 | preset 注册表 + /models 探活 + 生命周期位;Studio 价格列外链化 | L7/Studio |
| v0.7 | 错峰/批量调度 + 缓存友好组装规范;降级遥测仪表 | L7/L9 |
| 并行(商务) | 默认槽位治理章程;平台键模式试点 1–2 家 | 不动代码 |

## 五、反向清单(厂商会问引擎的问题,需准备答案)

- 流量画像:QPS / TTFT 分布 / 平均上下文长度?(决定厂商是否给专用端点)
- 是否共享匿名化性能基准?(厂商愿意据此优化游戏负载)
- 平台键模式的结算粒度、退款责任边界?
- AI 生成内容标识的实现责任在引擎还是厂商?
- 槽位绑 provider id 还是能力档?(RFC-3 待决问题,厂商希望可预测——绑能力档对厂商是"凭实力占槽",绑 id 是"签了就被锁")

## 厂商事实来源(2026-10-20 核实)

- DeepSeek 定价/错峰:tldl.io/resources/deepseek-api-pricing;briefs.co(2026-08-23 周末错峰);apidog.com(V4.1-Flash 峰谷价);platform.deepseek.com
- 智谱:bigmodel.cn/pricing;z.ai/subscribe;openrouter.ai/z-ai/glm-4.6;felloai.com/glm-pricing
- Moonshot:platform.kimi.com(docs/pricing、docs/models)
