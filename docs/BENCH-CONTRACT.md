# OriEra Bench 契约 v0（BENCH-CONTRACT）

> 版本：v0（2026-11-13）｜ 战略出处：2026-11-12 拍板路线图 A2（`out/roadmap-expert-panel/04-算力注入规划v2.1.md` §3）
> 来源：Triton 昇腾算子大赛官方评测器源码级移植（npu-kernelbench：profiler Computing 计时 / avg_speedup 聚合 / 反作弊五查）+ 本仓四腿金样实践（splat / skin / physics / goal-behavior）
> 地位：一切 OriEra 自建 benchmark（TTFP-Bench / loadgen / headless 回路 / 金样回归 / 算子对拍）的**共同评测纪律**。
> 变更规则：v0 冻结至 W7 首轮实测；修订须升版本号并保留旧版数据（防 Goodhart）。

## 八条评测纪律

### 1. 计时只认设备/进程侧
profiler Computing 口径优先（剔除 host launch 开销）；fallback（墙钟/Event）必须在报告里标注"含 host 开销、数字虚高"。TTFP 三段分账（LLM 时延 / 引擎构建 / 渲染截图）各记各账——禁止把 LLM 等待算进引擎账上。

### 2. 参考与被测同计时同容差
golden 与被测走同一个 profile 通道、同 warmup/iterations、同容差（对应引擎惯例：CPU 参考 / WebGL2 端 / headless 端同批对拍，如 skin_gpu 分级门禁）。

### 3. PASSED-only 算术平均 + 全过优先
得分 = 仅 PASSED 且 speedup>0 的算术平均——失败退出均值（不拖低也不掩盖），失败项单独列失败清单。**先保证全部判据通过，再优化均值。**

### 4. 固定种子 + 确定性检查
默认 seed=200；TTFP 用 OE-27 固定种子集（27 条 brief）。agent 生成的世界做确定性检查：同 seed 重跑，世界 diff 为空。物理/AI 金样加双跑断言（两次独立运行指纹/日志逐位相等，见 `crates/oriera-subsys/tests/physics_golden.rs` 与 `crates/oriera-agent/tests/goal_behavior_golden.rs`）。

### 5. 输出缓存检测（反"假执行"）
5 个 seed（200..204）的输出 flatten 后 allclose(atol=1e-8) 两两比对，unique_count < 5 判"假执行"——防 agent 用缓存糊弄验证回路。

### 6. 容差双轨
结构/渲染类：逐位零误差或 1e-5（金样互证惯例：splat ≤1/255，skin/物理 1e-5）；数值类：atol/rtol 双轨 + matched_ratio ≥ 0.99。**误差预算证明先行**（大赛做法：尾块注入 ≤1.7e-5 ≪ atol 2e-3，250× 裕度）。

### 7. 验收门成组上报
硬门模式：TTFP < 30 分钟（主判据）+ headless 回路 ≤ 30 秒（三段分账：LLM/构建/渲染各记各账）+ 三件套成组上报（时延分布 / token 成本 / 修改轮数缺一不可）。对应大赛 min speedup > 1.2x 验收门。

### 8. 无重试，失败即清单
评测一次机会，失败 = 失败清单条目，不静默重试、不挑种子（与复盘班"无证据记未达成"纪律同构）。

## 附 A：TTFP 三段分账表

| 段 | 起点 | 终点 | 归属 |
|---|---|---|---|
| LLM 段 | brief 发出 | 最后一个 token/工具调用返回 | 模型侧（API 计费口径） |
| 引擎构建段 | 构建命令发出 | 构建完成事件 | 引擎侧 |
| 渲染验证段 | headless 渲染启动 | 截图+断言判定返回 | 引擎侧（≤30s 门禁对象） |

单种子 TTFP = 三段之和；多环境并行只缩短墙钟，不改单种子口径。

## 附 B：金样重锚惯例

| 金样 | BLESS 环境变量 | 容差 |
|---|---|---|
| splat 渲染 | `ORIERA_SPLAT_BLESS=1` | ≤1/255 |
| 蒙皮双端 | （仓内惯例） | palette 1e-5 |
| 物理轨迹 | `ORIERA_SUBSYS_BLESS=1` | 指纹逐位 + 姿态 1e-5 |
| AI 行为 | `ORIERA_AGENT_BLESS=1` | 逐行零容差 |

重锚仅在有意改变语义时执行；重锚提交必须在 commit message 说明语义变化（PR #201 先例）。

## 附 C：容量参考与平台边界

NPU 水位（0.31-0.40 TB/s · 并行度≈512 · UB 256KB 表）与算子候选清单见 [PERF.md](PERF.md)「算子层容量参考」节。HiDevLab 平台边界实测（2026-11-13）：PyPI triton-ascend ≤3.2.0 与 CANN 9.1 二进制不兼容；官方镜像栈对多迭代循环/直排大 UB 内核分别死于 ConvertLinalgRToBinary / hivm-plan-memory——大赛级 A/B 需官方 3.6 环境（triton 3.5.0 + triton-ascend 3.2.2 组合在官方镜像栈曾验证可用）。
