# contracts/ — 跨层接口契约与逐位金标

一句话：**这里是「层与层之间怎么说话」的唯一真源**。实现可以换、可以被重写，契约不行——改契约的门槛高于改代码，这是 `docs/ARCHITECTURE.md` 原则 4（接口稳定优于实现统一）落地的地方。

目录里同时住着两类东西，性质完全不同，别混着读：

| 目录 | 是什么 | 能改吗 |
|---|---|---|
| `oriera/<layer>/v1/*.proto` | **契约**：跨层/跨端的接口形状（Protobuf） | 只能加法；破坏性变更开新版本目录 |
| `golden/*.json` | **金标**：某个提交上逐位算出来的期望值（指纹、预设行、意图表） | 禁止手改，只能由脚本重锚 |

---

## 1. 现有清单

### 契约（Protobuf）

| 文件 | 层 | 镜像的 Rust 面 | 消费方 |
|---|---|---|---|
| `oriera/pal/v1/capability.proto` | L1 PAL | `crates/oriera-pal/src/capability.rs` | 各端启动时读一次能力位图，上层只读 `Capabilities`、不写 `#[cfg(target_os)]` |
| `oriera/pal/v1/spatial_input.proto` | L1 PAL | `crates/oriera-pal/src/input.rs` | 统一 `SpatialInputEvent` 流（手/眼/语音/体感） |
| `oriera/genai/v1/genai.proto` | L8 生成式 AI | `crates/oriera-genai`（ADR 0003） | 提交-轮询-落地三段式生成；供应商是启动时选的数据，不硬编码 |

约定：`package oriera.<layer>.v1`，目录与 package 同构；**每个文件的头注释都写明它镜像哪个 Rust 模块**，实现改了却不改注释，等于契约失联。

### 金标（golden）

| 文件 | 钉住什么 | 重锚方式 |
|---|---|---|
| `golden/embodied_fingerprints.v2.json` | 具身仿真台 12 集、三口径的逐集状态哈希（当前锚） | `python scripts/reanchor_golden.py`（默认 dry-run），并在 PR 里解释物理行为为何变 |
| `golden/embodied_fingerprints.pre-m0a.json` | M0a 改动前的历史锚，用于证明「默认路径逐位不变」 | 只读，不再重锚 |
| `golden/presets.json` | L7 模型供应商预设行的单一来源（Rust `PROVIDER_PRESETS` 与浏览器 kernel 双侧对拍） | 与 `provider.rs` 的 `presets_match_golden_fixture` 测试对拍；改预设 = 改这份 JSON + 改测试 |
| `golden/intents.json` | L9 规则式意图解析的期望输出 | 与 `crates/oriera-agent` 的金标测试对拍 |

金标不是契约：它是**某个提交上的实测结果**。所以「金标变了」永远要回答一个问题——是物理/语义真变了（重锚 + 说明），还是代码跑偏了（修代码）。

---

## 2. 新增/修改契约的流程

1. **先 RFC，后动手。** 涉及 `contracts/` 里已 stable 的契约，先在 Discussions 的 Ideas 区开 RFC，7 天评论期（见 `CONTRIBUTING.md`）。
2. 在 `contracts/oriera/<layer>/v1/` 下新增或扩写 `.proto`，`syntax = "proto3"`，头注释写明镜像的 Rust 模块路径。
3. 在对应层的 `manifests/Lxx-*.yaml` 里把契约登记为 `proto/<path>`（L1 的 `capability.proto` / `spatial_input.proto` 就是这么登记的）。
4. 本地跑 `python scripts/check_manifests.py`——它会校验每个 `proto/<path>` 引用**真实存在**，以及 `requires` 能否被某个层的 `provides` 解析。
5. 实现侧跟上（当前是手写 mirror，见下一节），并补一个形状测试。
6. PR 描述里写清：加了什么字段、为什么是加法、对现有实现是否有行为影响。

### 兼容规则（写在契约里就是承诺）

- **只做加法**：加 message、加字段、加枚举值都对老读者无害；改字段语义、复用字段号、改类型都是破坏性变更。
- 删字段先 `reserved <n>; reserved "<name>";`，字段号永不复用。
- `proto3` 的枚举第一个值必须是 `0` 且命名 `*_UNSPECIFIED`（现有三个文件都遵守，例如 `GENERATION_KIND_UNSPECIFIED`）。
- 破坏性变更 = 新开 `vN` 目录 + 迁移说明，不在 `v1` 上直接改。`v1` 目录一旦有外部消费方就按冻结处理。

---

## 3. 生成流程：现状与缺口（如实记录）

目前仓库里**没有把生成代码入库**：`Cargo.toml` 里没有 `prost`/`tonic`，也没有 `build.rs` 跑 `protoc`。`.proto` 的事实角色是「人读契约 + manifest 引用目标」，Rust 侧是手写 mirror（`capability.rs`、`input.rs`、`oriera-genai`）。

这是**已知缺口**，不是设计意图。机器门禁现在只有一半：

```bash
# 已有：契约引用存在性 + 层依赖可解析 + 文档数字守卫
python scripts/check_manifests.py
python scripts/check_numbers.py

# 手动校验 proto 本身能不能编译（需要 protoc，CI 暂未接）
protoc --proto_path=contracts \
       --descriptor_set_out=/tmp/oriera-contracts.pb \
       contracts/oriera/*/v1/*.proto
```

接入生成时建议两条腿一起上，缺一不可：

1. **形状腿**：`protoc --descriptor_set_out` 产出 descriptor 并与提交的副本对比——纯文本 diff 看不出字段号被复用，descriptor 能。
2. **兼容腿**：`buf breaking`（或等价工具）对上一个 tag 做破坏性变更检测，把「只许加法」从文档承诺变成 CI 红灯。

在此之前，改契约的 PR 请人工确认上面两条并在描述里说明。

---

## 4. 不放进这里的东西

- **不是契约的实现细节**：具体算法、阈值、参数放各层 crate 与 `docs/`。
- **不是生成产物**：将来接入 `prost` 后，生成代码进 `OUT_DIR` 或独立 crate，不入库。
- **不是金标的替代品**：契约说「字段叫什么」，金标说「这个提交上算出来是多少」，两者不能互相背书。

---

相关：`docs/ARCHITECTURE.md`（层↔crate 对照、契约原则）、`CONTRIBUTING.md`（RFC 流程）、`scripts/reanchor_golden.py`（金标重锚）、`scripts/check_manifests.py`（契约引用门禁）。
