# 开源参与指南 / Open Source Guide

> 写给第一次来 OriEra 社区的人——不管你会不会写代码。
> 15 分钟能提交第一个 PR 的速通版在 [FIRST_PR.md](FIRST_PR.md)；本篇是全景地图。

## 1. OriEra 开源社区全景

| 入口 | 地址 | 用途 |
|---|---|---|
| 代码仓库 | https://github.com/OriEra-engine/engine | star / fork / 提 PR |
| 在线 Demo | https://oriera-engine.github.io/engine/ | 11 个浏览器可玩 demo，零安装 |
| Discussions | https://github.com/OriEra-engine/engine/discussions | 提问、聊天、开 RFC（中英文都行） |
| 官方演示视频 | `docs/media/oriera-official-demo.mp4`（仓库内，54s） | 一分钟看懂两个 AI 原生范式 |
| 架构文档 | [ARCHITECTURE.md](ARCHITECTURE.md) | 十一层架构、依赖规则、一帧数据流 |
| 中文即时交流 | 微信群 / 飞书群（入群联系维护者） | 中文社区日常交流 |

**License：Apache 2.0**（含专利授权，商用友好）。

## 2. 五分钟上手

1. **Star** 仓库——这是对项目最便宜的支援，也是别人发现它的方式。
2. 打开 [在线 Demo](https://oriera-engine.github.io/engine/)，玩一遍
   [10 言灵世界](https://oriera-engine.github.io/engine/examples/10-word-spirit/index.html)
   （用自然语言改写物理法则）和
   [11 隅角咖啡馆](https://oriera-engine.github.io/engine/examples/11-ai-cafe/index.html)
   （3D AI NPC + 长期记忆 + AI 导演）。
3. 想跑本地：`git clone` 后 `npx serve .`（Web demo，无需 Rust），
   或 `cargo run -p oriera-pc`（Rust 引擎，需 1.80+）。

## 3. 参与阶梯：从围观者到 Core Team

开源社区的参与是一个阶梯，每一级都是自愿的，停在任意一级都欢迎：

```
围观者（star / 玩 demo）
   └─ 用户（提 issue 报 bug、给体验反馈）
        └─ 贡献者（合并第 1 个 PR，自动成为 Contributor）
             └─ 活跃贡献者（持续评审、参与 RFC）
                  └─ Core Team（合并权限，按模块分工）
```

角色与决策流程的正式定义见 [GOVERNANCE.md](../GOVERNANCE.md)：
早期 BDFL + Core Team 模式，小改动 1 人 approve 即合，跨层/动契约走 7 天评论期的 RFC。

## 4. 不写代码也能贡献

| 类型 | 怎么做 |
|---|---|
| 玩 demo 提体验反馈 | 开 issue 写「哪个 demo + 什么操作 + 期望 vs 实际」 |
| 文档翻译 | 中英 README / 架构文档互译，直接 PR |
| 写教程 / 案例 | 「我用 OriEra 做了 XX」发 Discussions Show and tell 区 |
| 玩法创意 | 言灵世界式的「自然语言改法则」新玩法，开 issue 讨论 |
| 找 bug | 挑一个 demo 极限操作（连点 / 超大数值 / 断网），能复现就开 issue |

## 5. 写代码：从 good first issue 开始

1. 打开 [good first issue 列表](https://github.com/OriEra-engine/engine/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)，
   每个 issue 都写了**涉及文件和验收标准**。
2. 回帖认领（"I'll take this" / 「我认领」），避免撞车。
3. 按 [FIRST_PR.md](FIRST_PR.md) 的 15 分钟流程走完：fork → 分支 → 改 →
   `cargo fmt` + `clippy` + `test` → 带 DCO 签名提交 → PR。

提交规范两条硬要求（详见 [CONTRIBUTING.md](../CONTRIBUTING.md)）：

- **Conventional Commits**：`feat(pal): add android spatial backend`
- **DCO 签名**：`git commit -s` 自动加 `Signed-off-by:`（声明你有权贡献，不需要签 CLA）

## 6. 社区规范

- **行为准则**：[CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md)（Contributor Covenant 2.1）。
  一句话版：对事不对人，「新手问题」在这个社区是欢迎帖。
- **提问的姿势**：先搜 Discussions 存量帖；提问贴上环境（浏览器/Rust 版本）+ 完整报错 + 复现步骤。
- **中文可以吗**：可以。issue / PR / Discussions 中英文都行，PR 标题建议英文（方便检索），正文中文 OK。
- **安全漏洞**：不要开公开 issue，走 [SECURITY.md](../SECURITY.md)。
- **绝对红线**：不许把 API key / 模型 token / 用户隐私数据提交进仓库。

## 7. Hacktoberfest（每年 10 月）

OriEra 连续参与 Hacktoberfest：10 月内合并 **4 个** 合格 PR 可获得 Hacktoberfest 数字奖励。
仓库已备好一批挂着 `hacktoberfest` 标签的 good first issue，从文档 typo 到 demo 功能都有。
注意纯改错别字的 PR 可能不被官方计入——挑带 `hacktoberfest` 标签的 issue 做最稳。

## 8. 常见问题

**我不会 Rust，能贡献吗？**
能。examples/ 下的 demo 是纯 HTML/JS，文档、翻译、测试反馈都不需要 Rust。

**我是学生，可以参与吗？**
可以，开源不看文凭看 PR。good first issue 的验收标准写得足够细，照着做就行。

**公司能用吗？**
Apache 2.0，含专利授权，商用友好。唯一约束：改了要遵循协议声明，别去掉版权头。

**怎么联系维护者？**
Discussions 任意板块发帖，或中文微信群/飞书群（入群方式问群里任何一位管理员）。

**我想改架构 / 加新层，行吗？**
行，但先在 [Discussions · Ideas](https://github.com/OriEra-engine/engine/discussions/categories/ideas)
开 RFC，7 天评论期后动手——这是为了保护十一层边界不被无意破坏。
