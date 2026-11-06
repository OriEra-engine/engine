#!/usr/bin/env python3
"""plot_results（M2，PLAN §9.1 P-6）——聚合表出图：每个
(spec_hash8, method) 组的**均值 + 分层 bootstrap 95% CI + IQM**。

分层 bootstrap（两级重抽）：组内先对 seed 有放回重抽，再对每个抽中的
seed 按其成功率重抽 episode 的 0/1 结果——seed 间与 episode 间两层方差
都进区间，比单层 bootstrap 诚实（12 集 Wilson 区间宽到没法 argue 的病
就是这么治的）。

用法：
    python scripts/plot_results.py out/results/results.parquet --out-dir out/results/plots
    python scripts/plot_results.py out/results/results.csv  --bootstrap 10000

无 matplotlib 时降级：只写 <out-dir>/summary.csv（均值/IQM/CI 列）并提示。

失败相位（P2，opt-in）：聚合表带 `fail_reach/fail_grasp/fail_lift/fail_place`
列时（`embodied_sim_baseline --failure-phases` → `oriera_results.py aggregate`），
另写 `<out-dir>/failure_phases.csv`（每组四相位失败合计）并出堆叠条形图
`failure_phases.png`；没有这几列则产物与旧版完全相同。
"""
from __future__ import annotations

import argparse
import csv
import json
import random
import sys
from pathlib import Path


def load_rows(path: Path) -> list[dict]:
    if path.suffix == ".parquet":
        try:
            import pandas as pd

            df = pd.read_parquet(path)
            return df.to_dict("records")
        except ImportError:
            alt = path.with_suffix(".csv")
            if alt.exists():
                print(f"[plot-results] pandas 不可用 → 读旁挂 CSV：{alt}", file=sys.stderr)
                return load_rows(alt)
            raise SystemExit("[plot-results] 读 parquet 需要 pandas+pyarrow（或旁挂同名 CSV）")
    with open(path, encoding="utf-8", newline="") as f:
        return list(csv.DictReader(f))


def iqm(values: list[float]) -> float:
    """Interquartile mean — the middle-50% trimmed mean (rliable 口径)."""
    v = sorted(values)
    n = len(v)
    lo = int(n * 0.25)
    hi = int(n * 0.75)
    # rliable 的 IQM：丢弃下 25% 与上 25%。截断数与 scipy.stats.
    # trim_mean(v, 0.25) 一致：两端各 int(n*0.25)（奇数 n=5 → 两端各 1，
    # 保 3 值——此前 int(n*0.75)-int(n*0.25)=2 值与 scipy 不一致）。
    keep = v[lo : n - lo] if n - lo > lo else v
    return sum(keep) / len(keep)


def hierarchical_bootstrap(
    groups: dict[str, list[dict]], keys: list[str], b: int, rng: random.Random
) -> dict[str, tuple[float, float]]:
    """组内两级重抽 → 每组 once-rate 均值的 95% 分位区间。

    groups[key] = 该组的行表（每行一个 seed，含 episodes + successes_once）。
    """
    out: dict[str, tuple[float, float]] = {}
    for key in keys:
        rows = groups[key]
        stats: list[float] = []
        for _ in range(b):
            acc = 0.0
            n = 0.0
            for row in rng.choices(rows, k=len(rows)):
                episodes = int(row.get("episodes") or 0)
                succ = int(row.get("successes_once") or 0)
                if episodes <= 0:
                    continue
                rate = succ / episodes
                # 第二层：按该 seed 的成功率重抽 episode 的 0/1
                hits = sum(1 for _ in range(episodes) if rng.random() < rate)
                acc += hits / episodes
                n += 1
            if n:
                stats.append(acc / n)
        if not stats:
            out[key] = (float("nan"), float("nan"))
            continue
        stats.sort()

        def q(p: float) -> float:
            i = min(len(stats) - 1, max(0, int(p * len(stats))))
            return stats[i]

        out[key] = (q(0.025), q(0.975))
    return out


def wilson95(successes: int, n: int) -> tuple[float, float]:
    """Wilson 95% score interval（EV-P1：seeds<3 时分层 bootstrap 的
    seed 层无方差，区间系统性偏窄——退回单层二项口径并标注）。"""
    if n == 0:
        return (float("nan"), float("nan"))
    p = successes / n
    z = 1.959963984540054  # 双侧 95%
    denom = 1 + z * z / n
    centre = (p + z * z / (2 * n)) / denom
    half = z * ((p * (1 - p) + z * z / (4 * n)) / n) ** 0.5 / denom
    return (max(0.0, centre - half), min(1.0, centre + half))


FAILURE_PHASES = ("reach", "grasp", "lift", "place")


def _count(v) -> int | None:
    """聚合表单元格 → 计数；空串/None/NaN（缺失失败相位块的行）→ None。"""
    if v is None or v == "":
        return None
    try:
        f = float(v)
    except (TypeError, ValueError):
        return None
    return None if f != f else int(f)  # NaN != NaN


def failure_phase_rows(groups: dict[str, list[dict]], keys: list[str],
                       labels: dict[str, str]) -> list[dict]:
    """P2：每组四相位失败合计（只统计带失败相位列的行）；无数据 → []。"""
    out = []
    for key in keys:
        acc = [0, 0, 0, 0]
        seeds = 0
        for r in groups[key]:
            cells = [_count(r.get(f"fail_{p}")) for p in FAILURE_PHASES]
            if any(c is None for c in cells):
                continue
            seeds += 1
            for i, c in enumerate(cells):
                acc[i] += c
        if seeds:
            row = {"group": labels[key], "seeds": seeds, "failures": sum(acc)}
            row.update({f"fail_{p}": n for p, n in zip(FAILURE_PHASES, acc)})
            out.append(row)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("results", type=Path, help="oriera_results.py aggregate 的产物（parquet/csv）")
    ap.add_argument("--out-dir", type=Path, default=Path("out/results/plots"))
    ap.add_argument("--bootstrap", type=int, default=10000, help="重抽次数（默认 10000）")
    ap.add_argument("--seed", type=int, default=2026, help="bootstrap RNG 种子（默认 2026，可复现）")
    args = ap.parse_args()

    rows = load_rows(args.results)
    if not rows:
        raise SystemExit("[plot-results] 聚合表为空")

    # 分组：(spec_hash8, method) —— 行 = seed
    groups: dict[str, list[dict]] = {}
    labels: dict[str, str] = {}
    for r in rows:
        h8 = str(r.get("spec_hash8", r.get("spec_hash", "")))[:8]
        method = str(r.get("method", "unknown"))
        key = f"{h8}|{method}"
        rate = r.get("success_once_rate")
        if rate is None:
            continue
        groups.setdefault(key, []).append(r)
        labels[key] = f"{h8}/{method}"
    keys = sorted(groups)
    if not keys:
        raise SystemExit("[plot-results] 表里没有 success_once_rate 列——先用 oriera_results.py aggregate")

    rng = random.Random(args.seed)
    ci = hierarchical_bootstrap(groups, keys, max(200, args.bootstrap), rng)

    summary_rows = []
    for key in keys:
        rates = [float(r["success_once_rate"]) for r in groups[key]]
        lo, hi = ci[key]
        ci_kind = "bootstrap"
        # EV-P1：seeds<3 时 seed 层重抽无方差——分层区间系统性偏窄
        #（11/12 组实测 [75,100] vs Wilson [64.6,98.5]；100% 组零宽）。
        # 自动退回 Wilson 并在列名/输出标注。
        if len(rates) < 3:
            ep = sum(int(r.get("episodes") or 0) for r in groups[key])
            ok = sum(int(r.get("successes_once") or 0) for r in groups[key])
            lo, hi = wilson95(ok, ep)
            ci_kind = "wilson95(fallback: seeds<3)"
        summary_rows.append(
            {
                "group": labels[key],
                "seeds": len(rates),
                "mean_once_rate": sum(rates) / len(rates),
                "iqm_once_rate": iqm(rates),
                "ci_kind": ci_kind,
                "boot_ci95_lo": lo,
                "boot_ci95_hi": hi,
            }
        )

    args.out_dir.mkdir(parents=True, exist_ok=True)
    csv_path = args.out_dir / "summary.csv"
    with open(csv_path, "w", newline="", encoding="utf-8") as f:
        writer = csv.DictWriter(f, fieldnames=list(summary_rows[0].keys()))
        writer.writeheader()
        writer.writerows(summary_rows)
    print(f"[plot-results] 摘要 → {csv_path}")
    for s in summary_rows:
        print(
            f"  {s['group']:34s} seeds={s['seeds']:2d}  mean {s['mean_once_rate']:.1%}  "
            f"IQM {s['iqm_once_rate']:.1%}  CI95 [{s['boot_ci95_lo']:.1%}, {s['boot_ci95_hi']:.1%}]"
        )

    # P2 失败相位（opt-in：聚合表带 fail_* 列才出）——先落 CSV，绘图在下方
    phase_rows = failure_phase_rows(groups, keys, labels)
    if phase_rows:
        phase_csv = args.out_dir / "failure_phases.csv"
        with open(phase_csv, "w", newline="", encoding="utf-8") as f:
            writer = csv.DictWriter(f, fieldnames=list(phase_rows[0].keys()))
            writer.writeheader()
            writer.writerows(phase_rows)
        print(f"[plot-results] 失败相位 → {phase_csv}（规则 docs/LAB-GUIDE.md §2.2）")
        for p in phase_rows:
            cells = " · ".join(f"{k} {p[f'fail_{k}']}" for k in FAILURE_PHASES)
            print(f"  {p['group']:34s} seeds={p['seeds']:2d}  失败 {p['failures']:3d} → {cells}")

    # 图：装了 matplotlib 才画（均值 ± CI 误差棒 + IQM 点）
    try:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        print(
            "[plot-results] 未装 matplotlib → 只输出 CSV 摘要（pip install matplotlib 出图）",
            file=sys.stderr,
        )
        return 0

    ys = list(range(len(summary_rows)))
    means = [s["mean_once_rate"] * 100 for s in summary_rows]
    errs_lo = [(s["mean_once_rate"] - s["boot_ci95_lo"]) * 100 for s in summary_rows]
    errs_hi = [(s["boot_ci95_hi"] - s["mean_once_rate"]) * 100 for s in summary_rows]
    iqms = [s["iqm_once_rate"] * 100 for s in summary_rows]

    fig, ax = plt.subplots(figsize=(8, 0.6 * len(summary_rows) + 2))
    ax.errorbar(
        means,
        ys,
        xerr=[errs_lo, errs_hi],
        fmt="o",
        capsize=4,
        label="mean once-rate (hierarchical bootstrap 95% CI)",
    )
    ax.scatter(iqms, ys, marker="D", color="tab:orange", zorder=3, label="IQM")
    ax.set_yticks(ys)
    ax.set_yticklabels([s["group"] for s in summary_rows])
    ax.invert_yaxis()
    ax.set_xlabel("success_once_rate (%)")
    ax.set_title("OriEra embodied baselines — mean ± CI95, IQM per (spec × method)")
    ax.grid(True, axis="x", alpha=0.3)
    ax.legend(loc="lower right")
    fig.tight_layout()
    png = args.out_dir / "success_rates.png"
    fig.savefig(png, dpi=150)
    print(f"[plot-results] 图 → {png}")
    if phase_rows:
        # 堆叠条形：每组失败集按 reach → grasp → lift → place 分段
        fig2, ax2 = plt.subplots(figsize=(8, 0.6 * len(phase_rows) + 2))
        ys2 = list(range(len(phase_rows)))
        left = [0] * len(phase_rows)
        for k in FAILURE_PHASES:
            vals = [p[f"fail_{k}"] for p in phase_rows]
            ax2.barh(ys2, vals, left=left, label=k)
            left = [a + b for a, b in zip(left, vals)]
        ax2.set_yticks(ys2)
        ax2.set_yticklabels([p["group"] for p in phase_rows])
        ax2.invert_yaxis()
        ax2.set_xlabel("failed episodes (failure_phase@v1)")
        ax2.set_title("OriEra embodied baselines — failures by phase")
        ax2.legend(loc="lower right")
        fig2.tight_layout()
        png2 = args.out_dir / "failure_phases.png"
        fig2.savefig(png2, dpi=150)
        print(f"[plot-results] 失败相位图 → {png2}")
    # 原始摘要也落一份 JSON（对账用）
    (args.out_dir / "summary.json").write_text(
        json.dumps(summary_rows, indent=1, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
