# 15 分钟提交你的第一个 PR

从零到合入 main 的完整路径。不需要先读完架构文档——做完这个 PR 你自然就懂了。

## 第 0 分钟：选任务

打开 [good first issue 列表](https://github.com/OriEra-engine/engine/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)，
挑一个没有其他人认领的，回帖认领（"I'll take this" / 「我认领」）。
每个 issue 都写了涉及文件和验收标准。

## 第 2 分钟：搭环境

```bash
git clone https://github.com/<你的用户名>/engine.git
cd engine
rustup toolchain install stable   # 需要 1.80+
cargo test --workspace            # 全绿再动手
```

没装 Rust？只改文档/网页 demo 的话跳过这步也行。

## 第 5 分钟：改代码

```bash
git checkout -b fix/my-first-pr
# ... 改动 ...
cargo fmt                         # 必跑
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 第 10 分钟：提交（带 DCO 签名）

```bash
git add -A
git commit -s -m "fix(core): 描述你做了什么"
# -s 会自动加 Signed-off-by，这是我们的 DCO 要求
git push -u origin fix/my-first-pr
```

commit message 用 Conventional Commits：`类型(范围): 描述`。
类型：feat / fix / docs / refactor / test / chore。

## 第 12 分钟：开 PR

GitHub 页面上点 "Compare & pull request"，PR 模板的检查清单照着勾。
关联 issue 写 `Fixes #11`（换成你认领的编号）。

## 第 14 分钟：等评审

CI 会自动跑 fmt / build / test / clippy。红了就按日志修；
绿了就等 Core Team 评审（通常 48 小时内）。改完被要求调整很正常，
直接 push 新 commit 即可，不要 force push。

## 常见卡点

| 症状 | 解法 |
|---|---|
| clippy 报 `field is never read` | 加测试用到它，或删掉字段（不许滥用 `#[allow]`） |
| 想写 `#[cfg(target_os)]` | 停。查 `oriera_pal::Capabilities` 位图（ADR 0001） |
| 想让两层直接互相 use | 停。看 [ARCHITECTURE.md](ARCHITECTURE.md) 依赖规则，走契约或 resource |
| 测试在 Windows 本机跑不了 bench | 正常，CI(Linux) 会跑；本地 `cargo test -p <crate> --lib` 即可 |

有任何问题，直接去 [Discussions · Q&A](https://github.com/OriEra-engine/engine/discussions/categories/q-a) 发帖，
贴上你的报错——「新手问题」在这个社区是欢迎帖。
