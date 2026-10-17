# 贡献指南 / Contributing to OriEra

欢迎！OriEra 是一个 AI 原生空间计算世界引擎，走开源共建路线。
这份文档告诉你怎么把代码、文档、想法变成对项目有用的 PR。

## 你可以怎么贡献

| 类型 | 入口 |
|---|---|
| 修 bug / 加小功能 | 直接提 PR，关联 issue |
| 改架构 / 加新层 / 动契约 | 先在 Discussions 开 RFC，7 天评论期后再动手 |
| 文档翻译 / 案例 / 教程 | 直接 PR，不卡 RFC |
| 新平台后端（如新增 Vulkan 后端） | 走 RFC，因为会动 L1 PAL 契约 |

## 开发环境

```bash
# Rust 侧（需要 Rust 1.80+）
rustup toolchain install stable
cargo test --workspace

# Web demo（无需 Rust，任选一个 demo 目录）
cd examples/01-hello-triangle && python3 -m http.server 8000
# 浏览器打开 http://localhost:8000，需要 Chrome/Edge 113+（WebGPU）
```

## 代码规范

- **Rust**：`cargo fmt` + `cargo clippy -- -D warnings` 零警告才让合。
- **跨层调用**：禁止上层 `use` 下层私有字段；跨层必须走 `contracts/` 里的 Protobuf/JSON Schema。
- **能力判断**：新代码里不许出现 `#[cfg(target_os = "...")]`；要查 `oriera_pal::Capabilities` 位图。
- **脑机接口**：v1 不许动 bit 40+，不许接真实硬件驱动。

## PR 流程

1. fork → 建分支 `feat/xxx` 或 `fix/xxx`
2. commit message 用 Conventional Commits：`feat(pal): add android spatial backend`
3. PR 模板会自动跑：单元测试 + `cargo clippy` + 契约兼容性检查
4. 至少 1 个 Core Team 成员 approve 才能合并
5. 合并前必须 **DCO 签名**：commit 里加一行 `Signed-off-by: Your Name <you@example.com>`
   （这等价于你声明这段代码你有权贡献，不需要签 CLA）
6. 如果 PR 引入**新机制**（见下节「新机制披露检查」），请在 PR 描述勾选对应复选框

## 不能做的事

- 不许在没 RFC 的情况下改 `contracts/` 里的已 stable 契约（破坏性变更要 bump major）
- 不许把 API key / 模型 token / 用户隐私数据提交进仓库
- 不许绕过安全护栏做 Agent 越权工具（参考 L9 安全约束）

## 新机制披露检查（Patent Gate / 专利先行门禁）

OriEra 的开源路线与专利管线并行：代码一旦合并进 main 即构成公开。为保证有专利价值的机制**先拿到申请日、再开源**，引入新机制的 PR 需过一个 30 秒检查：

- **触发条件**：PR 引入了新的算法、调度/仲裁/预算/校验/降级/路由逻辑、新的协议格式或新的数据结构设计。修 bug、补测试、改文档、等价重构不触发；拿不准就当触发。
- **流程**：在 PR 描述里勾选「本 PR 含新机制」；维护者会在 30 分钟内完成三选一分诊——**先申请后开源**（PR 挂起等申请号，通常几周）、**防御性公开**（直接合并并留档）、**保持私有**（PR 关闭或转私有分支）。
- **对贡献者的影响**：作者署名与 PR 记录不受任何影响；若机制进入专利申请，贡献者将作为共同发明人记录在案。

## 哪里找任务

- 标了 `good first issue` 的 issue
- `help wanted`
- `ROADMAP.md` 里未勾选的条目（如 L8/L10/L11 对应的 crate）
- 各 crate 里的 `TODO` 注释

## 沟通

- 中文：飞书群 / GitHub Discussions（中文区）
- 英文：GitHub Discussions（English）/ Matrix
- 月度例会：每月最后一个周六，公开录像，议题提前一周在 Discussions 征集
