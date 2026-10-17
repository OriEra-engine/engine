# 感谢贡献！请确认以下事项：

## 检查清单

- [ ] 我跑过 `cargo test --workspace`（或对应端的测试）
- [ ] 我跑过 `cargo clippy --workspace -- -D warnings`，零警告
- [ ] 跨层调用走的是 `contracts/` 里的契约，没有直接 use 下层私有字段
- [ ] 新代码没有 `#[cfg(target_os = "...")]`，而是查 `Capabilities` 位图
- [ ] 我没有动 bit 40+ 的 BCI 预留位
- [ ] 如果是 breaking change，我已经在 PR 标题加 `BREAKING:` 并说明了迁移路径
- [ ] 每个 commit 都有 `Signed-off-by:`（DCO）
- [ ] 本 PR 是否引入**新机制**（新算法/调度/校验/降级/路由/协议格式，见 CONTRIBUTING「新机制披露检查」）：否 / **是**（已通知维护者分诊）

## 这个 PR 做了什么？

<!-- 一两句话说明 -->

## 关联 issue

Fixes #

## 截图 / Trace（如适用）

<!-- 截图、性能数字、对比图 -->
