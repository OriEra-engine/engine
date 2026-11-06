#!/usr/bin/env python3
"""oriera-results aggregate（M2，PLAN-embodied-general §9.1 P-6）——把
`runs/` 树聚合成分析友好的表：行 = spec_hash × method × seed，列 =
成功率（once/at_end）、集数、Wilson 区间、吞吐、协议/闸/判据、manifest
出处（git SHA、dirty、features、rapier）。

读的是 M0b 约定的两个文件（embodied_sim_collect / embodied_sim_baseline
都写）：
    runs/<spec_hash8>/<method>-p<proto8>/<seed>/eval_report.json
    runs/<spec_hash8>/<method>-p<proto8>/<seed>/run_manifest.json

用法：
    python scripts/oriera_results.py aggregate out/m0b/runs --out out/results/results.parquet
    python scripts/oriera_results.py aggregate runs/ --out results.parquet --format csv

输出格式自动降级：装了 pandas+pyarrow 写 parquet；否则写 CSV 并提示
（列名一致，plot_results.py 两种都吃）。

失败相位列（P2，opt-in）：只要有一份 eval_report.json 带 `failure_phases` 块
（`embodied_sim_baseline --failure-phases` 产出，规则 docs/LAB-GUIDE.md §2.2），
全表追加 `failure_phase_scheme` + `fail_reach/fail_grasp/fail_lift/fail_place`
（无该块的行留空）并按组打印相位合计；一份都没有则输出逐字节同旧版。
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]

# P2 失败相位遥测（opt-in）：eval_report.json 的 `failure_phases.by_phase` 四键
# → 聚合表的四列（管线顺序）。
FAILURE_PHASES = ("reach", "grasp", "lift", "place")
FAILURE_COLUMNS = ["failure_phase_scheme"] + [f"fail_{p}" for p in FAILURE_PHASES]


def failure_phase_cells(report: dict) -> dict | None:
    """`failure_phases` 块 → 五列；报告没有该块（未开 --failure-phases）→ None。"""
    block = report.get("failure_phases")
    if not isinstance(block, dict):
        return None
    by_phase = block.get("by_phase") or {}
    cells = {"failure_phase_scheme": block.get("scheme", "")}
    for p in FAILURE_PHASES:
        cells[f"fail_{p}"] = int(by_phase.get(p, 0) or 0)
    return cells



def seed0_divergence_hint(rows):
    """EV-P1：同 (spec_hash, method) 出现多个 seed0 = 可能有重跑选优——
    打印提示（不阻止），供论文口径自查。"""
    from collections import defaultdict
    seen = defaultdict(set)
    for r in rows:
        seen[(r.get("spec_hash"), r.get("method"))].add(str(r.get("seed0")))
    multi = {k: v for k, v in seen.items() if len(v) > 1}
    if multi:
        print(f"[hint] {len(multi)} 组 (spec,method) 存在多个 seed0 并存——"
              f"重跑选优会让注册表数字偏高；论文口径请声明全部 attempts：")
        for k, v in sorted(multi.items()):
            print(f"    {k[0][:8]} / {k[1]}: seeds0={sorted(v)}")

def load_json(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def aggregate(runs_root: Path) -> list[dict]:
    rows: list[dict] = []
    for report_path in sorted(runs_root.rglob("eval_report.json")):
        run_dir = report_path.parent
        report = load_json(report_path)
        if not isinstance(report, dict):
            print(f"  · 跳过（eval_report 非法）：{report_path}", file=sys.stderr)
            continue
        manifest = load_json(run_dir / "run_manifest.json") or {}
        engine = manifest.get("engine") or {}
        build = manifest.get("build") or {}
        protocol = report.get("protocol") or {}
        gate = report.get("baseline_gate") or {}
        runs = report.get("runs") or []
        # 双报优先（M2 baseline 报告），退回单一 success（collect 报告）
        once = [bool(r.get("success_once", r.get("success", False))) for r in runs]
        at_end = [bool(r.get("success_at_end", r.get("success", False))) for r in runs]
        row = {
            "spec_hash": report.get("spec_hash", ""),
            "spec_hash8": report.get("spec_hash", "")[:8],
            "method": report.get("method", manifest.get("method", "unknown")),
            "seed": manifest.get("seed", protocol.get("seed0")),
            "protocol_id": protocol.get("id", ""),
            "protocol_hash": protocol.get("parameter_hash", ""),
            "success_criteria": protocol.get("success_criteria", ""),
            "task_id": report.get("task_id", ""),
            "episodes": report.get("episodes", len(runs)),
            "successes_once": sum(once),
            "successes_at_end": sum(at_end),
            "success_once_rate": report.get("success_once_rate", report.get("success_rate")),
            "spec_name": (
                lambda sp: (sp.get("meta", {}) or {}).get("name")
                or sp.get("robot", {}).get("name", "")
            )(manifest.get("spec", {}) or {}),
            "success_at_end_rate": report.get(
                "success_at_end_rate", report.get("success_rate")
            ),
            "wilson_once_lo": (report.get("wilson95_once") or report.get("wilson95") or [None])[0],
            "wilson_once_hi": (report.get("wilson95_once") or report.get("wilson95") or [None, None])[1],
            "gate_threshold": gate.get("threshold"),
            "gate_verdict": gate.get("verdict", report.get("gate", "")),
            "throughput_steps_per_s": report.get("throughput_steps_per_s"),
            "verify_determinism": report.get("verify_determinism", ""),
            "robot": ((manifest.get("spec") or {}).get("robot") or {}).get("name", ""),
            "engine_git_sha": engine.get("git_sha", ""),
            "engine_git_dirty": engine.get("git_dirty"),
            "rapier_version": engine.get("rapier_version", ""),
            "features": ";".join(build.get("features") or []),
            "profile": build.get("profile", ""),
            "os": build.get("os", ""),
            "arch": build.get("arch", ""),
            "run_dir": str(run_dir),
        }
        phases = failure_phase_cells(report)
        if phases is not None:
            row["_failure_phases"] = phases
        rows.append(row)
    # 只增不改：任一报告带失败相位块 → 全表追加五列（缺块行留空——None：
    # CSV 写空串、parquet 写 null，不混 int/str）；一份都没有 → 列集与旧版
    # 逐字节相同。
    if any("_failure_phases" in r for r in rows):
        for r in rows:
            cells = r.pop("_failure_phases", None) or {c: None for c in FAILURE_COLUMNS}
            r.update(cells)
    return rows


def write_out(rows: list[dict], out: Path, fmt: str | None) -> str:
    if not rows:
        raise SystemExit("[oriera-results] 没有找到任何 eval_report.json —— 先跑采集/基线")
    out.parent.mkdir(parents=True, exist_ok=True)
    columns = list(rows[0].keys())
    chosen = fmt
    if chosen is None:
        try:
            import pandas  # noqa: F401
            import pyarrow  # noqa: F401

            chosen = "parquet"
        except ImportError:
            chosen = "csv"
    if chosen == "parquet":
        try:
            import pandas as pd

            pd.DataFrame(rows, columns=columns).to_parquet(out, index=False)
            return "parquet"
        except ImportError:
            print(
                "[oriera-results] pandas/pyarrow 不可用 → 降级 CSV（pip install pandas pyarrow 可得 parquet）",
                file=sys.stderr,
            )
            chosen = "csv"
    csv_path = out.with_suffix(".csv") if out.suffix == ".parquet" else out
    import csv

    with open(csv_path, "w", newline="", encoding="utf-8") as f:
        writer = csv.DictWriter(f, fieldnames=columns)
        writer.writeheader()
        writer.writerows(rows)
    return f"csv ({csv_path.name})"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    agg = sub.add_parser("aggregate", help="runs/ → results.parquet（或 CSV）")
    agg.add_argument("runs_root", type=Path, help="runs/ 根目录（runs/<hash8>/<method>/<seed>/）")
    agg.add_argument("--out", type=Path, default=Path("out/results/results.parquet"))
    agg.add_argument("--format", choices=["parquet", "csv"], default=None)
    args = ap.parse_args()

    if not args.runs_root.exists():
        raise SystemExit(f"[oriera-results] {args.runs_root} 不存在")
    rows = aggregate(args.runs_root)
    fmt = write_out(rows, args.out, args.format)
    # 摘要：按 (spec_hash8, method) 分组报 once 率均值
    groups: dict[tuple[str, str], list[float]] = {}
    groups_at_end: dict[tuple[str, str], list[float]] = {}
    for r in rows:
        key = (r["spec_hash8"], r["method"])
        v = r["success_once_rate"]
        if v is not None:
            groups.setdefault(key, []).append(float(v))
            ae = r.get("success_at_end_rate")
            if ae is not None:
                groups_at_end.setdefault(key, []).append(float(ae))
    print(f"[oriera-results] 聚合 {len(rows)} 行 → {args.out}（{fmt}）")
    seed0_divergence_hint(rows)  # EV-P1：并存 seed0 提示（重跑选优自查）
    for (h8, method), rates in sorted(groups.items()):
        mean = sum(rates) / len(rates)
        ae = groups_at_end.get((h8, method))
        ae_txt = f"  at_end {sum(ae) / len(ae):.1%}" if ae else ""
        # EV-P1：once ≥ at_end 恒成立——展示面只报 once 会天然偏高
        print(f"  {h8} × {method:20s} seeds={len(rates):2d}  "
              f"once {mean:.1%}{ae_txt}")
    print_failure_phases(rows)
    return 0


def print_failure_phases(rows: list[dict]) -> None:
    """P2：按 (spec_hash8, method) 汇总失败相位（只在表里有这几列时打印）。"""
    if not rows or "fail_reach" not in rows[0]:
        return
    sums: dict[tuple[str, str], list[int]] = {}
    for r in rows:
        if r.get("fail_reach") in ("", None):
            continue
        key = (r["spec_hash8"], r["method"])
        acc = sums.setdefault(key, [0, 0, 0, 0])
        for i, p in enumerate(FAILURE_PHASES):
            acc[i] += int(r.get(f"fail_{p}") or 0)
    if not sums:
        return
    print("[oriera-results] 失败相位（failure_phase 遥测，规则见 docs/LAB-GUIDE.md §2.2）：")
    for (h8, method), acc in sorted(sums.items()):
        cells = " · ".join(f"{p} {n}" for p, n in zip(FAILURE_PHASES, acc))
        print(f"  {h8} × {method:20s} 失败 {sum(acc):3d} → {cells}")


if __name__ == "__main__":
    sys.exit(main())
