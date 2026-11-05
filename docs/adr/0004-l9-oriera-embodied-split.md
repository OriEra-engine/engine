# ADR 0004: L9 拆分——具身仿真运行时独立成 `oriera-embodied`

- 状态：已接受
- 日期：2026-11-06
- 决策者：架构组（20 角色审查·技术架构集群·架构师节拍板，PLAN-embodied-v3 §三 P2）
- 关联：ADR 0003（L8 内生化，同款「一代世界一个 crate」分层逻辑）

## 背景

`oriera-agent`（L9）一个 crate 混居两代世界：

- **认知/故事层**：`cognition` / `memory` / `story` / `genai` 桥 / `tools` /
  `prefab` / NL `parse`——书 T12 的引擎服务面；
- **具身仿真运行时**（五期 PLAN-embodied-sim / -general / -v3 堆叠）：
  `embodied_env`（umbrella + P1-C1 四子模块 scene/tasks/env/cameras）、
  `spec`、`urdf`、`meshres`、`lerobot`、`eval`、`ik`、`manifest`。

混居的实际代价（架构师审查实锤）：

1. **wasm 壳被迫穿透**：`oriera-wasm` 只需要 `TableTopEnv`/`SceneSpec`/
   `eval::fingerprint_set`，却必须依赖 `oriera-agent`——浏览器 bundle 把
   认知层（cognition/memory/story/genai 桥）一并拖上；
2. **继续堆叠**：每一期具身功能都往 L9 里加文件，认知面与仿真面的
   演化节奏、依赖面、审计面完全不同；
3. **feature 代数失真**：`spec`/`urdf`/`cam` 本是仿真面 feature，却长在
   认知 crate 上，`oriera-agent/json`（memory 持久化）与仿真面的
   `serde_json` 需求互相牵制。

## 决策

### 1. 新建 `crates/oriera-embodied`，纯移动、零逻辑改动

八组模块整体搬迁：`embodied_env`（umbrella + scene/tasks/env/cameras）、
`spec`、`urdf`、`meshres`、`lerobot`、`eval`、`ik`、`manifest`。依赖方向：

```text
oriera-subsys(L6) ─▶ oriera-embodied(L9 仿真运行时) ─▶ oriera-wasm（浏览器壳）
                    oriera-agent(L9 认知层) ─▶ oriera-embodied（spec 桥/spec_hash）
```

`oriera-wasm` 改挂 `oriera-embodied`，**不再穿透 oriera-agent**——认知层
退出浏览器 bundle。`oriera-agent` 保留认知面 + 对 `oriera-embodied` 的
依赖（PyO3 桥的 cdylib `oriera_sim` 仍从 agent lib 构建，`python.rs` 经
兼容层引用 env/spec/manifest）。

### 2. 公开 API 路径兼容（硬验收）

`oriera-agent` lib.rs 保留一组 feature 门控的 `pub use oriera_embodied::…`
兼容 re-export：`oriera_agent::embodied_env::TableTopEnv`、
`oriera_agent::spec::SceneSpec` 等全部历史路径继续解析。外部 crate
（wasm 壳、平台壳）、示例、文档命令**零改动照常工作**。新增代码一律直接
依赖 `oriera-embodied`。

### 3. feature 代数保持逐位兼容

agent 的 `rigid3d`/`cam`/`spec`/`urdf` 变成纯转发 shim（同名转发到
`oriera-embodied`，`spec` 仍联动本 crate `rigid3d`+`json` 标志位）；
`oriera-embodied/python` 是无依赖的**收据保真标志位**——`manifest::
feature_list()` 报告的 feature 串与拆分前逐字节一致（python wheel 的
run_manifest 供应链不因拆分变字）。历史 CI 腿命令
（`-p oriera-agent --features rigid3d,cam,spec,urdf,json`）继续可用。

### 4. specs 预设库与测试随源搬迁

- `specs/**`（official + lab + INDEX.json）→ `crates/oriera-embodied/specs/`；
  `official_specs_dir()` 是编译期 `CARGO_MANIFEST_DIR` 路径，随 crate 自
  动重锚；`ORIERA_SPECS_OFFICIAL` 环境变量覆盖语义不变。全仓路径引用
  （baselines YAML / scripts / docs / wasm 测试 include_str / INDEX root /
  `gen_spec_index.py`）全链同步。
- 具身面集成测试（`urdf_robot_spec` / `slide_drawer` / `embodied_cam` /
  `gripper_down_smoke` + `tests/fixtures/**`）随源迁 `oriera-embodied/tests/`；
  认知面测试（lib 单元测试 79 项）留 `oriera-agent`。

### 5. examples 归属：保留在 `oriera-agent`

采集/基线/相机/URDF 探针示例（`embodied_sim_collect` /
`embodied_sim_baseline` / `embodied_cam_capture` / `urdf_chain_dump` /
`urdf_traj_dump` / 2D `embodied_collect`）**留在 oriera-agent/examples**，
理由：

1. LAB-GUIDE §1 交作业主命令、CONTRIBUTING、baselines YAML `entry:`、
   reanchor/fingerprint 工具链全部锚定 `-p oriera-agent --example …`——
   这是 P0 #1 刚修好的学生第一公里，迁移是纯文档翻动、零功能收益；
2. 示例是 dev-target 不是运行时：它们经兼容层编译，不污染 crate 依赖
   边界（wasm 壳已直接挂 `oriera-embodied`）；
3. 未来若要迁，是机械改动（路径改 `oriera_embodied::` + Cargo `[[example]]`
   搬家 + 文档命令同步），随时可做，不构成本 ADR 的阻塞面。

认知系示例（`cafe` / `studio_e2e` / `tool_loop`）天然留 agent。

## 后果

- **金标锚不动是移动零行为的硬证据**：默认 12 集 collect 的 receipt
  state hashes 与 pre-m0a 锚逐个相等；`reanchor_golden.py --dry-run`
  no drift。
- 体积：`oriera-agent` src 从 ~17.6k 行降到 ~4.5k 行（认知面）；具身面
  ~13.1k 行在 `oriera-embodied`。
- `ARCHITECTURE.md` 分层表补 `oriera-embodied` 行（L9 拆两 crate）。
- 后续具身功能（PLAN-embodied-v3 P2：真 fixture 预设、mesh 碰撞量化、
  Contact 判据）全部落在 `oriera-embodied`，不再进 agent。

## 验收清单（本 ADR 落地 PR）

1. `cargo test -p oriera-embodied --features rigid3d,cam,spec,urdf,json` 全绿；
   `cargo test -p oriera-agent` 认知面 79 项不退；
2. `cargo test -p oriera-wasm` 6/6；wasm32 check 过；
3. 金标 12 集 receipt 与 pre-m0a 锚逐集相等；reanchor dry-run exit 0；
4. clippy 三 crate 0 warning；fmt clean；`gen_spec_index.py --check` 过；
   `sync-resolved.mjs --check` 过；
5. Python 三冒烟：`import oriera_sim` / `from_spec` / legacy 构造器。
