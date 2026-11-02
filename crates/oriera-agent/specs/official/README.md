# specs/official — 受治理的官方 spec 库（M0b，PLAN-embodied-general §9.1 P-9）

**规则一句话：这里的每个文件都是一份实验身份（`spec_hash`），改动任何非 `meta` 字段 = 新实验 ⇒ 必须另起新文件名（或新版本后缀），并在 PR 打 `physics-behavior-change` 标签、重跑 `contracts/golden/embodied_fingerprints.v2.json`；禁止原地改值。**

- 文件名 = `extends` 里引用的名字（`"extends": "tabletop-4dof-pick"` → `official/tabletop-4dof-pick.json`）。
- 实验室/个人实验放 `specs/lab/`（建议 `lab/<user>/`），用 `extends` 继承官方 spec、只写差异（见 `lab/example-cylinder.json`）；lab 文件不受治理，但同名异 hash 会被 `scripts/gen_spec_index.py` 拦下。
- `INDEX.json`（`python scripts/gen_spec_index.py` 生成）：`meta.name → spec_hash` 的登记表；同名异 hash 报错、同 hash 异名放行（同一实验的别名）。
- `tabletop-4dof-pick.json` 是 M0a 的 v2 金标，与 `specs/tabletop-4dof-pick.json`（WP-B 契约测试钉住的旧路径）逐字节一致——两份都不许动；spec_hash 锚 `c90869dcf6a04875`。
- 建议配合 `CODEOWNERS`：`crates/oriera-agent/specs/official/ @oriera-embodied-maintainers`（仓库级文件不在 M0b 授权面内，此处只留建议）。
