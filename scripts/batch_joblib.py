#!/usr/bin/env python3
"""Batch sweep with joblib (M0b, PLAN §9.1 P-5 — "slurm array / joblib
示例而非自建调度器"): N specs × M seeds → one `embodied_sim_collect`
subprocess each, written into the `runs/<hash8>/<method>/<seed>/` layout
with skip-if-done resume, `--verify-determinism=off` (the seed IS the
receipt; full double-runs are for the classroom contract) and
`--gate=report` (a sweep records, never panics).

    pip install joblib
    python scripts/batch_joblib.py --specs crates/oriera-embodied/specs/official/tabletop-4dof-pick.json \
        crates/oriera-embodied/specs/lab/example-cylinder.json --seeds 1000 2000 3000 --jobs 4
    # re-run the same line → every finished (spec, seed) is skipped (断点续跑)

Each job's stdout/stderr lands in `<runs-root>/_logs/<hash8>-<seed>.log`;
the summary table at the end reads every `eval_report.json` back
(success_rate + Wilson 95%, protocol id + hash) so a 300-config sweep
ends in one table, not 300 terminals.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]


def find_exe(explicit: str | None) -> list[str]:
    if explicit:
        return [explicit]
    for cand in (
        REPO / "target" / "release" / "examples" / "embodied_sim_collect.exe",
        REPO / "target" / "release" / "examples" / "embodied_sim_collect",
        REPO / "target" / "debug" / "examples" / "embodied_sim_collect.exe",
        REPO / "target" / "debug" / "examples" / "embodied_sim_collect",
    ):
        if cand.exists():
            return [str(cand)]
    return [
        "cargo", "run", "-q", "--release", "-p", "oriera-agent", "--example", "embodied_sim_collect",
        "--features", "rigid3d,cam,spec,urdf", "--",
    ]


def run_one(exe: list[str], spec: str, seed: int, runs_root: Path, protocol: str, episodes: int | None,
            extra: list[str], no_cam: bool) -> dict:
    """One (spec, seed) job. The engine itself does the skip-if-done check
    (`eval_report.json` present ⇒ `↷ 断点续跑` and exit 0)."""
    cmd = exe + [
        "--spec", spec, "--seed", str(seed), "--runs-root", str(runs_root),
        "--protocol", protocol, "--verify-determinism", "off", "--gate", "report",
    ]
    if episodes is not None:
        cmd += ["--episodes", str(episodes)]
    if no_cam:
        cmd += ["--no-cam"]
    cmd += extra
    t0 = time.time()
    out = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", cwd=REPO)
    logs = runs_root / "_logs"
    logs.mkdir(parents=True, exist_ok=True)
    tag = f"{Path(spec).stem}-{seed}"
    (logs / f"{tag}.log").write_text(out.stdout + "\n--- stderr ---\n" + out.stderr, encoding="utf-8")
    skipped = "断点续跑" in out.stdout
    return {
        "spec": spec, "seed": seed, "rc": out.returncode, "skipped": skipped,
        "secs": round(time.time() - t0, 1), "log": str(logs / f"{tag}.log"),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--specs", nargs="+", required=True, help="spec 文件列表（official/lab 都行，extends 会解析）")
    ap.add_argument("--seeds", nargs="+", type=int, default=[1000], help="seed0 列表（每个 = 一个 run 目录）")
    ap.add_argument("--runs-root", default="runs", help="runs/<hash8>/<method>/<seed>/ 的根（默认 runs/）")
    ap.add_argument("--protocol", default="paper@v1", help="course@v1 | paper@v1（默认 paper@v1 = 只记录）")
    ap.add_argument("--episodes", type=int, default=None, help="每个 run 的集数（默认取协议默认：course 12 / paper 100）")
    ap.add_argument("--jobs", type=int, default=2, help="并行进程数（joblib n_jobs）")
    ap.add_argument("--exe", default=None, help="embodied_sim_collect 可执行文件（默认找 target/，否则 cargo run --release）")
    ap.add_argument("--no-cam", action="store_true", help="不录相机帧（扫参常用，快 3–5×）")
    ap.add_argument("--dry-run", action="store_true", help="只打印作业列表")
    ap.add_argument("extra", nargs="*", help="透传给 CLI 的额外参数（放在 -- 之后，如 -- --set max_steps=500）")
    args = ap.parse_args()

    jobs = [(s, seed) for s in args.specs for seed in args.seeds]
    print(f"[batch_joblib] {len(args.specs)} spec × {len(args.seeds)} seed = {len(jobs)} 个作业，并行 {args.jobs}，协议 {args.protocol}，runs-root {args.runs_root}")
    if args.dry_run:
        for s, seed in jobs:
            print(f"  {s}  seed {seed}")
        return 0
    try:
        from joblib import Parallel, delayed
    except ImportError:
        print("[batch_joblib] 需要 joblib：pip install joblib", file=sys.stderr)
        return 2
    exe = find_exe(args.exe)
    runs_root = Path(args.runs_root)
    results = Parallel(n_jobs=args.jobs)(
        delayed(run_one)(exe, s, seed, runs_root, args.protocol, args.episodes, args.extra, args.no_cam)
        for s, seed in jobs
    )

    # Summary: read every eval_report.json under runs_root written by this sweep.
    failed = [r for r in results if r["rc"] != 0]
    skipped = [r for r in results if r["skipped"]]
    print(f"\n[batch_joblib] 完成 {len(results)}：跳过(已完成) {len(skipped)}，失败 {len(failed)}")
    for r in failed:
        print(f"  ✗ {r['spec']} seed {r['seed']} rc={r['rc']} → {r['log']}")
    rows = []
    for rep in sorted(runs_root.rglob("eval_report.json")):
        try:
            d = json.loads(rep.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        rows.append((
            rep.parent.relative_to(runs_root).as_posix(), d.get("protocol", {}).get("id", "?"),
            d.get("spec_hash", "?")[:8], d.get("episodes", "?"), f"{d.get('success_rate', 0):.3f}",
            "[{:.3f},{:.3f}]".format(*d.get("wilson95", [0, 0])), d.get("gate", "?"),
        ))
    if rows:
        print("\n  run                                   protocol   spec8     n    rate   wilson95        gate")
        for row in rows:
            print("  {:38s}{:11s}{:10s}{:5}{:>7}  {:16s}{}".format(*row))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
