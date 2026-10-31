# 路线图 / Roadmap

对齐系统论三阶段演化，按年滚动更新。最近 12 个月细颗粒度，更远只列方向。

## 阶段一：AR 适配完备期（2026 Q4 – 2028）

**目标版本：0.x → 1.0**
**核心目标**：五端跑通、闭环成立、能在 AR 眼镜上 90fps 跑 demo。

### 2026 Q4（当前，v0.1）
- [x] Monorepo 骨架（Rust + pnpm workspace）
- [x] L1 PAL 能力位图 + 五端入口
- [x] 开源治理文件（CONTRIBUTING / GOVERNANCE / Code of Conduct）
- [x] 02-ecs-spawn：ECS demo 已上线（浏览器 20 万粒子 SoA）
- [x] 架构整理：层↔crate 对照、LayerManifest、依赖规则（见 docs/ARCHITECTURE.md）
- [x] L8 骨架提前落地：GenerationProvider 边界 + Mock + 校验 + L4 热替换（ADR 0003，2028 管线做深）
- [ ] 01-hello-triangle 升级为 wgpu 原生窗口（不只是网页版）

### 2027 H1（v0.2 – v0.4）
- [ ] L2 调度器：帧预算切片 + 无锁队列
- [ ] L3 完整 ECS：WorldObject、Scene、Signal 总线
- [ ] L5 Render Graph：清屏 → 渲染网格 → 加载 glTF
- [x] L6 rapier3d 具身后端：multibody 关节臂 + 确定性采集（PLAN-embodied-sim，2026-10-29落地；Jolt 评估结论：无 reduced-coordinate articulation，不做，见 growth/PLAN-embodied-sim.md）
- [ ] WebGPU Baseline 全绿（Chrome/Firefox/Safari/visionOS）

### 2027 H2（v0.5 – v0.7）
- [ ] L7 推理运行时：端侧 ONNX Runtime + 云端 vLLM 路由
- [ ] L9 单 Agent 闭环：感知-记忆-规划-行动，MCP 工具注册表
- [ ] L9 生成溯源一等化：画布/影游的生成档案（prompt→素材→采纳链）进引擎一等类型（PLAN-canvas M3 占位，2026-10-28）
- [ ] 04-agent-chat：对话 → 调一个工具 → 在场景里放物体
- [ ] L10 WebXR 手势/眼动接入

### 2028（v0.8 – v1.0）
- [ ] L8 文生3D 管线：Tripo/Meshy 适配器 + 几何修复
- [ ] AR 眼镜端到端：visionOS 原生包 + 90fps 实测
- [ ] 1.0 发布：契约全部 stable，提供迁移指南

## 阶段二：自组织生态期（2028 – 2031）

**目标版本：1.x → 2.0**
**核心目标**：UGC 编辑器上线，社区能自己造世界，"千人千叙事" A/B 跑出留存提升。

- L11 同源编辑器：自然语言 / 半可视化 / 专业三层创作
- AI 导演四状态机 + TrueSkill + PID 难度控制上线
- A2A 多 Agent 协议落地，跨组织 Agent 互通
- UGC 内容审核 + C2PA 水印合规
- 2.0 发布：生态 SLO 稳定，社区贡献占比 ≥50%

## 阶段三：物理-数字统一世界模型期（2031 – 2035）

**目标版本：2.x → 3.0**
**核心目标**：脑机接入、世界模型统一物理与数字。

- 脑机接口非侵入消费级接入（bit 40+ 从预留转正）
- 3DGS 动态化 + 可重光照达到电影级
- 物理引擎与神经渲染统一（可微物理）
- 3.0 发布

## 不在路线图上的事（明确不做）

- ❌ 自研游戏工作室 / 自研头部 IP
- ❌ 绑定单一云厂商的私有 API
- ❌ v1 阶段接侵入式 BCI（三类医疗器械，合规不允许）
- ❌ 做微信小程序 / 快应用等闭源分发渠道
