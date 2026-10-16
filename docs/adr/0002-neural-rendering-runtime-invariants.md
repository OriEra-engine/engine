# ADR 0002: 神经渲染运行时不变量（吸收自 Matrix-Game 4.0 的公开工程契约）

- 状态：已接受（契约先行，代码缓行）
- 日期：2026-10-15
- 决策者：架构组
- 关联 RFC：Ideas 区《L7/L5 神经渲染运行时契约》（7 天评论期）

## 背景

昆仑万维 Skywork AI 的 [Matrix-Game 4.0](https://github.com/SkyworkAI/Matrix-Game)
（Apache-2.0，2026-10-15 开源）验证了一条与 OriEra L5 规划（网格 + 3DGS/NeRF 混合渲染）
互补的路线：**原生引擎持有玩法与物理，因果视频模型负责画面**。其
`docs/architecture.md` 公开的三条运行时契约在工程上被证明是"引擎确定性"与
"神经生成概率性"共存的关键。

OriEra 当前的现实：L5 的真实 Pass 图在 v0.2 路线图上，L7 的端云路由排在
2027 H2。**此刻把这三条契约写成代码模块，是为还不存在的实现提前造抽象**
（脚手架上的脚手架）。但把它们写成规范条款，今天就是有效的：它们约束
未来一切神经渲染/流式推理实现的行为边界，且可以被 LayerManifest 的
budgets/degrade 字段机器读出、被 CI 门禁。

## 决策

以**规范性条款**（normative clauses）吸收以下三条运行时不变量，落位于
`manifests/L5-render.yaml` 与 `manifests/L7-infer.yaml`；本 ADR 不引入任何
业务代码。

### 不变量 1：整数步帧同步（integer-step frame pacing）

物理模拟以固定频率 `physics_hz` 步进；在线采样/生成每帧必须推进**整数个**
物理步，因此目标帧率必须整除物理频率（如 120Hz 物理支持 20/24/30/40/60 FPS）。

- 约束对象：任何向 L5/L7 供给帧的神经渲染或流式推理会话。
- 理由：非整数步会破坏 L2/L3 的确定性回放（book T5）——这是引擎的根基。

### 不变量 2：有界显示队列——只丢已解码帧，减速而非追帧

显示帧队列必须有界（Matrix-Game 取 4 槽）。生产受阻时，**游戏放慢于墙钟时间，
而不是追帧爆发**；且**只有已解码的显示帧可以被丢弃**——模拟帧、模型条件帧
永远不丢。

- 约束对象：L5 的呈现路径与 L7 的输出队列。
- 理由：追帧爆发会把毛刺放大成雪崩；丢条件帧会污染神经模型的时序状态
  （KV/RoPE 语义要求连续帧索引）。

### 不变量 3：进程/会话隔离

推理运行时不得 import 游戏逻辑或呈现传输；游戏（引擎态）不得 import 推理
运行时；由一个会话所有者把两者连起来。对应 OriEra 既有铁律"只准向下依赖"
在**进程维度**的投影。

- 约束对象：未来神经渲染 sidecar / 云渲染执行位的接线方式（L1
  `RouteRemoteTask` 已预留该通道）。

### 降级语义

无 `NEURAL_RENDER_3DGS` 能力位、或神经会话不可用时：**透传传统光栅化**，
神经精修永远是可以完全关闭的增量，不是必需路径。

## 落代码的触发条件（重要）

当出现**第二个消费者**（例如 L5 Pass 图 v0.2 落地后的第一个真实 Neural
Refine Pass，或第二个流式推理接入方）时，才把上述不变量实现为
`oriera-infer` / `oriera-render` 内的类型与校验代码。在此之前，任何 PR
不得以"预实现"为由提交契约代码骨架。

## 许可与来源说明

- Matrix-Game 4.0 代码为 Apache-2.0。本 ADR 吸收的是**公开文档描述的工程
  模式与数值契约**，不拷贝其任何源代码；数值（120Hz、4 槽）在条款中作为
  参考基线，OriEra 可按端型自定（AR 眼镜的物理频率不同）。
- 其模型权重携带独立许可，与 OriEra 无关，本 ADR 不涉及。

## 后果

- ✅ 三条不变量从第一天起约束未来实现，避免"先写代码再补规范"的倒挂。
- ✅ LayerManifest 的 degrade 条目让降级路径可被 CI 与上层预知。
- ⚠️ 在触发条件满足前，本 ADR 只是纸面约束——评审神经渲染相关 PR 时必须
  对照本文件逐条核对。
- ⚠️ 若 Matrix-Game 后续版本修订契约数值，本 ADR 需同步评审（以 OriEra
  自身端型为准，不盲从）。

## 参考

- `manifests/L5-render.yaml`（degrade: neural_render_cap_missing）
- `manifests/L7-infer.yaml`（degrade: neural_session_overrun / display_queue_full）
- Matrix-Game 4.0 `Matrix-Game-4/docs/architecture.md`（Runtime Contract /
  Synchronization and Backpressure / Ownership 三节）
- ADR 0001（能力位图与降级哲学）
