# 10 · 言灵世界 Word Spirit — AI 原生小游戏

> 你不操作角色。你说的每一句话，都是这个世界的物理法则。

[▶ 在线玩](https://oriera-engine.github.io/engine/examples/10-word-spirit/index.html) ·
[▶ 自动演示模式](https://oriera-engine.github.io/engine/examples/10-word-spirit/index.html?demo=1)
（本地：`python -m http.server` 后打开 `examples/10-word-spirit/index.html`）

## 玩法

三座星座沉睡在虚空中（虚线圆圈）。每座需要 **60 只萤火虫聚集 0.6 秒**才能点亮，
180 秒内点亮全部三座即胜。你唯一的操作是——**说话**：

| 言灵 | 效果 |
|---|---|
| `spawn 200` / `生成 200` | 世界上多出 200 个光点 |
| `gravity 60` / `重力 60` | 改写重力：群体坠向底部（聚群神器） |
| `speed 0.3` / `速度 0.3 倍` | 时间流速 |
| `teleport 0,-620` / `传送到 0,-620` | 空间折跃：全体瞬移（支持负坐标） |
| `how many` / `多少个` | 询问世界现状 |
| `clear` / `清空` | 万物归零 |

通关思路（25 秒实录）：`spawn 600` → `重力 60`（底部两座点亮）→
`重力 0` `speed 0.3`（稳住群体）→ `传送到 0,-620`（折跃点亮悬穹座）✦

右侧 AI 导演会解说你的每句话；乱说话会被礼貌地拒绝（L7 `rejected` 计数）。

## 架构（为什么它是 OriEra 的 demo）

这个页面是 Rust 引擎的**同构投影**——每一块都对照一个 crate：

| 页面里的 | 引擎里的 | 层 |
|---|---|---|
| TypedArray SoA 列 + generation 句柄 | `oriera-ecs` | L3 |
| 固定步长积分 + Gravity/SpeedScale resource | `oriera-subsys` + `oriera-core` | L6/L2 |
| `parse()` 规则解析（中英六意图+负数） | `oriera-agent` | L9 |
| 命令队列逐帧执行 + processed/rejected | `oriera-infer` | L7 |
| AI 导演解说 | 书 T12 AI 导演（规则版） | L9 |
| 右侧层级仪表盘 | `docs/ARCHITECTURE.md` | 全部 |

## 过程存档

`docs/` 下是四个阶段的浏览器实测截图：
stage1 世界与物理 → stage2 言灵通道 → stage3 星座与胜负 → stage4 仪表盘。

## 视频

| 视频 | 内容 | 时长 |
|---|---|---|
| [docs/demo.mp4](docs/demo.mp4) | **最终演示片**：标题 → 四阶段流程 → 22 秒自动演奏实录（重力聚群 → 传送折跃 → 胜利结算）→ 结尾 | 42s |
| [docs/process.mp4](docs/process.mp4) | **制作流程片**：四个阶段的浏览器实测画面 | 16s |

实录来自浏览器标签页原生录像（24fps），自动演奏用 `?demo=1` 谱面模式。
