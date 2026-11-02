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
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]


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
        rows.append(row)
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
    for r in rows:
        key = (r["spec_hash8"], r["method"])
        v = r["success_once_rate"]
        if v is not None:
            groups.setdefault(key, []).append(float(v))
    print(f"[oriera-results] 聚合 {len(rows)} 行 → {args.out}（{fmt}）")
    for (h8, method), rates in sorted(groups.items()):
        mean = sum(rates) / len(rates)
        print(f"  {h8} × {method:20s} seeds={len(rates):2d}  mean once-rate {mean:.1%}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
