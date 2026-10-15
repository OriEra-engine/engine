# 安全策略 / Security Policy

## 报告漏洞

**不要用公开 issue 报安全漏洞。** 请发邮件到 `security@oriera.dev`（占位，
正式域名启用前用 GitHub Security Advisories）：

1. 仓库页 → Security → Advisories → Report a new vulnerability
2. 48 小时内会有回应；修复期间请勿公开细节
3. 我们会在致谢名单里感谢报告者（除非你要求匿名）

## 支持版本

| 版本 | 状态 |
|---|---|
| 0.0.x (main) | ✅ 接受报告 |

## 安全边界（这个仓库特别在乎的）

OriEra 是 AI 原生引擎，除了常规软件安全，还有三类引擎特有的边界
（对应书 T15 横切安全子系统）：

1. **Agent 工具越权**：L9 Agent 只能通过 L7 提交 `Command` 白名单，
   任何「让 Agent 直接执行任意代码/任意文件读写」的 PR 会被拒绝。
   新增 Command 必须在 `crates/oriera-infer/src/lib.rs` 的枚举里显式声明。
2. **跨端数据**：空间锚点、眼动/语音数据属敏感数据，L1 PAL 的
   input/net 边界不得在日志里落盘原始流（`tracing` 里只允许元信息）。
3. **BCI 预留位**：bit 40+ 恒为 0，不接受任何真实脑机硬件驱动进 v1。

## 供应链

- 依赖只从 `crates.io` 官方源解析；新增依赖需要在 PR 里说明用途
- CI 跑 `cargo clippy -D warnings`；发布产物签名（SLSA 路线，见 ROADMAP）
- 检测到恶意依赖投毒时：立即 lock `Cargo.lock` → 发 Advisory → 回归测试
