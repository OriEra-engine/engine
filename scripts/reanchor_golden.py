#!/usr/bin/env python3
"""reanchor_golden.py — 固化「指纹金标重锚」流程（PLAN-embodied-v3 P2 #51，
本包 P1-C2 提前落地）。

金标锚（contracts/golden/embodied_fingerprints.v2.json）是「同平台逐位」
契约的锚点：任何物理行为变化（有意或无意）都会让 12 集逐集三口径漂移。
此前重锚靠人肉四步（跑 collect → 逐集 diff → 手改 JSON → 凭记忆写 PR），
本脚本把四步固化成一条命令：

  1. 跑默认桌面实验 12 集 collect（--fingerprints 写「新锚草稿」，
     与锚同 schema——草稿即未来锚，不二次手工编辑）；
  2. 读草稿三口径（纯状态 / v1 折叠 / v2 完整身份）+ pre-m0a 锚的
     v1 连续性轴，与现有金标逐集 diff；
  3. 生成 PR 描述模板（变更摘要 + 新锚 JSON 全文）打印并落盘
     out/reanchor/pr-description.md；
  4. `--write`：旧 v2 锚备份为 `.bak` → 写入新锚 → 打印
     `physics-behavior-change` 标签提醒（PR 必须挂该标签 + scheme 递增
     决策，见锚内 purpose 字段）。

用法：
    python scripts/reanchor_golden.py                # dry-run（默认；不碰 contracts/golden/，
                                                     #  产物只落 out/reanchor/）
    python scripts/reanchor_golden.py --write        # 确认重锚（备份 + 写入）
    python scripts/reanchor_golden.py --episodes 12 --seed0 1000

退出码：0 = no drift（锚未漂移，无需重锚）；1 = 有 drift 且未 --write
（先走 physics-behavior-change 流程确认变化是有意的，再带 --write 重跑）；
2 = 命令/环境错误。`--write` 在 no drift 时拒绝写入（无事可重锚）。

输出目录：out/reanchor/（collect 产物 + 草稿 + PR 模板；均为产物，不入库）。
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
ANCHOR_V2 = REPO / "contracts" / "golden" / "embodied_fingerprints.v2.json"
ANCHOR_PRE = REPO / "contracts" / "golden" / "embodied_fingerprints.pre-m0a.json"
OUT = REPO / "out" / "reanchor"

COLLECT_CMD = [
    "cargo", "run", "-q", "-p", "oriera-agent", "--example", "embodied_sim_collect",
    "--features", "rigid3d,cam,spec,urdf", "--",
    "--task", "pick_place",
]

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
sys.stderr.reconfigure(encoding="utf-8", errors="replace")


def log(msg: str) -> None:
    print(msg, flush=True)


def run_collect(episodes: int, seed0: int) -> Path:
    """跑 collect 12 集，--fingerprints 写草稿，返回草稿路径。"""
    OUT.mkdir(parents=True, exist_ok=True)
    draft = OUT / "anchor-draft.json"
    run_dir = OUT / "run"
    cmd = COLLECT_CMD + [
        "--seed", str(seed0),
        "--episodes", str(episodes),
        "--fingerprints", str(draft),
        "--out", str(run_dir),
    ]
    # 种子语义：--seed 显式给定时 episodes 从 seed0 起抽；锚的种子带是
    # seed0=1000 + 退化重抽 displaced band（+65536/重抽次）——收集器自洽。
    log(f"[collect] {' '.join(cmd)}")
    for attempt in range(4):
        p = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True,
                           encoding="utf-8", errors="replace", timeout=3600)
        if p.returncode == 0:
            break
        blob = (p.stderr or "") + (p.stdout or "")
        low = blob.lower()
        # 并行工作包占构建锁是常态（PLAN-embodied-v3 P1 报告纪律）：等 30s 重试
        if ("file lock" in low or "blocking waiting" in low
                or "another cargo" in low or "Compiling" in blob):
            log(f"[collect] build lock busy / retryable (attempt {attempt + 1}/4), wait 30s")
            time.sleep(30)
            continue
        sys.stderr.write(blob[-4000:])
        raise RuntimeError("embodied_sim_collect 失败（见上方 stderr）")
    else:
        raise RuntimeError("collect：构建锁重试耗尽（4 次）")
    tail = (p.stdout or "").strip().splitlines()[-6:]
    for line in tail:
        log(f"[collect] {line}")
    if not draft.exists():
        raise RuntimeError(f"collect 未写出草稿 {draft}")
    return draft


def load(path: Path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def view(golden: dict, key: str):
    fp = golden.get("fingerprints", {})
    return [str(x) for x in fp.get(key) or golden.get(key) or []]


def diff_axis(name: str, got: list[str], want: list[str],
              seeds: list[int], limit: int = 12) -> tuple[list[str], list[int]]:
    """逐集 diff 一个口径；返回（人话行，漂移集索引）。"""
    if len(got) != len(want):
        return ([f"  {name}: 集数不同 run={len(got)} anchor={len(want)}"],
                list(range(max(len(got), len(want)))))
    bad = [i for i, (a, b) in enumerate(zip(got, want)) if a != b]
    if not bad:
        return ([f"  {name}: 12/12 逐集相等（no drift）"], [])
    lines = [f"  {name}: {len(got) - len(bad)}/{len(got)} 相等，漂移 {len(bad)} 集"]
    for i in bad[:limit]:
        lines.append(f"    ep{i + 1} seed {seeds[i] if i < len(seeds) else '?'}: "
                     f"anchor {want[i]} → run {got[i]}")
    if len(bad) > limit:
        lines.append(f"    …另有 {len(bad) - limit} 集（见 {OUT / 'anchor-draft.json'}）")
    return (lines, bad)


def build_pr_template(anchor_new: dict, anchor_old: dict, pre: dict,
                      drift_lines: list[str], drifted: bool) -> str:
    """PR 描述模板：变更摘要 + 新锚 JSON 全文。"""
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    old_commit = anchor_old.get("engine_commit", "?")
    new_commit = anchor_new.get("engine_commit", "?")
    succ_old = anchor_old.get("successes")
    succ_new = anchor_new.get("successes")
    head = f"""## physics-behavior-change：指纹金标重锚（contracts/golden/embodied_fingerprints.v2.json）

> 由 `scripts/reanchor_golden.py` 生成（{now}）。**PR 必须挂 `physics-behavior-change`
> 标签**；若三口径 schema 语义变化还需 scheme 递增（锚内 `scheme` 字段）。

### 变更摘要

- engine_commit：`{old_commit}` → `{new_commit}`
- 成功率：{succ_old}/12 → {succ_new}/12
- spec_hash：`{anchor_old.get('spec_hash')}` → `{anchor_new.get('spec_hash')}`
- protocol：`{anchor_old.get('protocol_id')}` → `{anchor_new.get('protocol_id')}`
  （parameter `{anchor_old.get('protocol_parameter_hash')}` → `{anchor_new.get('protocol_parameter_hash')}`）
- identity_hash：`{anchor_old.get('identity_hash')}` → `{anchor_new.get('identity_hash')}`

### 逐集 diff（旧锚 vs 本次 collect）

```
{chr(10).join(drift_lines) if drift_lines else '  （空）'}
```
"""
    if drifted:
        head += """
### 重锚理由（提交前必填）

<!-- 有意的物理行为变化来源：PR #___ / 调参项 ___；统计口径影响：
     跨平台成功率差 ___ 集（CI 矩阵 banded report）；金标三口径全量漂移/
     部分漂移。若变化是无意的，停止重锚并回退。 -->

### 新锚 JSON（contracts/golden/embodied_fingerprints.v2.json 全文）

```json
"""
    else:
        head += """
### 新锚 JSON（与现锚一致，no drift 复核记录）

```json
"""
    head += json.dumps(anchor_new, ensure_ascii=False, indent=2) + "\n```\n"
    return head


def main() -> int:
    ap = argparse.ArgumentParser(description="指纹金标重锚（dry-run 默认）")
    ap.add_argument("--write", action="store_true",
                    help="旧锚备份 .bak 后写入新锚（默认 dry-run 零写入）")
    ap.add_argument("--episodes", type=int, default=12)
    ap.add_argument("--seed0", type=int, default=1000)
    args = ap.parse_args()

    if not ANCHOR_V2.exists() or not ANCHOR_PRE.exists():
        log(f"错误：金标锚缺失（{ANCHOR_V2} / {ANCHOR_PRE}）")
        return 2

    log("== 指纹金标重锚 ==")
    log(f"模式：{'WRITE（备份 + 写入）' if args.write else 'DRY-RUN（零写入）'}")

    draft_path = run_collect(args.episodes, args.seed0)
    draft = load(draft_path)
    anchor = load(ANCHOR_V2)
    pre = load(ANCHOR_PRE)

    seeds = draft.get("seeds", [])
    lines: list[str] = []
    drifted = False

    log("")
    log("## 逐集 diff（现有金标 vs 本次 collect 三口径 + pre-m0a 连续性）")
    for name, key_new, key_old in [
        ("纯状态 state", "episode_state_hashes", "episode_state_hashes"),
        ("v1 折叠", "episode_fingerprints_v1", "episode_fingerprints_v1"),
        ("v2 完整身份", "episode_fingerprints_v2", "episode_fingerprints_v2"),
    ]:
        d_lines, bad = diff_axis(name, view(draft, key_new),
                                 view(anchor, key_old), seeds)
        lines += d_lines
        for l in d_lines:
            log(l)
        if bad:
            drifted = True
    # pre-m0a 连续性：v1 折叠轴必须与 pre-m0a 锚逐位一致（除非这次变化
    # 连 v1 折叠口径都动了——那要连 pre-m0a 一起处置，见 PR 模板提醒）。
    pre_lines, pre_bad = diff_axis("v1 ↔ pre-m0a 锚", view(draft, "episode_fingerprints_v1"),
                                   [str(x) for x in pre.get("episode_state_hashes", [])],
                                   seeds)
    for l in pre_lines:
        log(l)
    lines += pre_lines
    pre_drifted = bool(pre_bad)
    succ_run = draft.get("successes")
    succ_anchor = anchor.get("successes")
    log(f"  成功率: anchor {succ_anchor}/12 → run {succ_run}/12"
        f"（pre-m0a v1 连续性: {'漂移' if pre_drifted else '一致'}）")
    if succ_run != succ_anchor:
        drifted = True

    pr = build_pr_template(draft, anchor, pre, lines, drifted)
    pr_path = OUT / "pr-description.md"
    pr_path.write_text(pr, encoding="utf-8")
    log("")
    log(f"[pr] 描述模板已写出 {pr_path}")

    if not drifted:
        log("")
        log("no drift —— 三口径 + pre-m0a 连续性 + 成功率全部逐集相等，锚未漂移。")
        if args.write:
            log("--write 拒绝执行：没有可重锚的漂移（无事可写）。")
        return 0

    log("")
    log("!! 指纹漂移 —— 这是物理行为变化（有意或无意）。")
    log("!! 走 physics-behavior-change 流程：确认变化有意 → 本 PR 挂")
    log("!! `physics-behavior-change` 标签 → schema 语义变化时 scheme 递增 →")
    log("!! 重跑本脚本带 --write 落锚。")

    if not args.write:
        return 1

    backup = ANCHOR_V2.with_suffix(ANCHOR_V2.suffix + ".bak")
    shutil.copy2(ANCHOR_V2, backup)
    log("")
    log(f"[write] 旧锚已备份 → {backup}")
    ANCHOR_V2.write_text(
        json.dumps(draft, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    log(f"[write] 新锚已写入 → {ANCHOR_V2}")
    if pre_drifted:
        log("[note] v1 折叠轴相对 pre-m0a 锚也漂移——pre-m0a 锚是否随动重锚，")
        log("       需在 PR 里单独说明（本脚本按任务定义只写 v2 锚）。")
    log("")
    log("提醒：本 PR 必须挂 `physics-behavior-change` 标签（重锚不是静默操作），")
    log("并在 PR 描述里写明变化来源与统计口径影响（模板见 out/reanchor/pr-description.md）。")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as e:
        log(f"错误：{e}")
        sys.exit(2)
