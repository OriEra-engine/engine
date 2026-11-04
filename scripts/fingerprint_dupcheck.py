#!/usr/bin/env python3
"""助教防抄袭：两个 run 的逐集指纹对表（PLAN-embodied-v3 §三 P2「防抄袭
fingerprint_dupcheck.py」）。

引擎给每一集采集数据写了三口径指纹（纯状态 / v1 回执口径 / v2 完整身份，
见 docs/EMBODIED-PIPELINES.md §6.1 与 LAB-GUIDE §2）。两份交上来的数据集
如果**逐集指纹全部相同**，只可能是同一份采集冒充两次独立实验——比任何
文本查重都硬的实锤。本脚本把「人肉对哈希」变成一条命令。

读什么（按优先级，找到一个就用）：
  1. <run>/lerobot-v21/meta/oriera_receipt.json —— 转换过/引擎直出的数据集
     自带 receipt（episode_fingerprints_v1 规范名；旧 receipt 只有
     episode_state_hashes 键，值相同，读兼容）
  2. <run>/eval_report.json 的 fingerprints 块 —— 未转换的采集产物
     （注意：这里的 episode_state_hashes 是**纯状态**口径，与 receipt 同名
     不同义——P0-4 一名两义修复的背景，见 docs/LAB-GUIDE.md §2 表）

输入两种形态（每个参数各自独立解析）：
  - 一个 run 目录（目录里有 eval_report.json 或 lerobot-v21/）
  - 一个 glob，如 runs/<hash8>/<method>/<seed>/ 或批量：
    ``python -I scripts/fingerprint_dupcheck.py "out/m2b/runs/*/*/*"``

用法：
  python -I scripts/fingerprint_dupcheck.py <runA> <runB> [--csv OUT.csv]
  python -I scripts/fingerprint_dupcheck.py <runA> <runA>     # 自指演示：同一目录跑两次 → 全同 = 重复检出
  python -I scripts/fingerprint_dupcheck.py --view v2 <runA> <runB>   # 换 v2 完整身份口径对表

退出码：0 = 未检出指纹级重复（含一方无可比指纹——如实打印原因）；
        1 = 检出指纹级重复（双方逐集全部相同 = 抄袭实锤）；
        2 = 输入/读取错误（路径不存在、双方都无可比指纹）。

判定口径（诚实边界）：「同 spec 同种子逐位一致」本身就是引擎的确定性契约
（LAB-GUIDE §2）——所以全同不一定=抄袭，也可能是同一实验交了两遍或
助教复跑。脚本把 spec_hash / seed / method 一起打出来给你判：spec_hash
不同而指纹全同 = 最强实锤；spec_hash 相同 = 重复提交/复跑，同样需要在
报告里标注，不能当两次独立采集计分。本脚本只出事实表，不代替人下结论。
"""
from __future__ import annotations

import argparse
import csv
import glob
import json
import sys
from pathlib import Path

# Windows 控制台默认 cp936/gbk：打非 ASCII 直接 UnicodeEncodeError
# （P0-7 同款预防，gen_spec_index.py / check_release_versions.py 先例）。
for _st in (sys.stdout, sys.stderr):
    if hasattr(_st, "reconfigure"):
        _st.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent


def load_json(path: Path):
    try:
        with path.open("r", encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def _pick_v1(obj: dict, source: str) -> list | None:
    """v1（回执折叠口径）的取法按来源分家——receipt 与 eval_report 的
    episode_state_hashes 同名不同义（P0-4），不能统一取。"""
    fp = obj.get("episode_fingerprints_v1")
    if isinstance(fp, list) and fp:
        return [int(x) for x in fp]
    legacy = obj.get("episode_state_hashes")
    if isinstance(legacy, list) and legacy:
        # receipt 里的 episode_state_hashes 是 v1 的旧键名（legacy alias）；
        # eval_report 里的同名键是纯状态口径——只在 receipt 来源当 v1 用。
        if source == "receipt":
            return [int(x) for x in legacy]
        return None
    return None


def _pick_v2(obj: dict) -> list | None:
    fp = obj.get("episode_fingerprints_v2")
    return [int(x) for x in fp] if isinstance(fp, list) and fp else None


def extract_from(obj: dict, source: str) -> dict | None:
    """从 receipt 或 eval_report 的 fingerprints 块提三口径 + 身份元数据。"""
    container = obj
    if source == "eval_report":
        block = obj.get("fingerprints")
        if not isinstance(block, dict):
            return None
        container = block
    v1 = _pick_v1(container, source)
    v2 = _pick_v2(container)
    if v1 is None and v2 is None:
        return None
    state = container.get("episode_state_hashes")
    state = [int(x) for x in state] if isinstance(state, list) and state else None
    scheme = container.get("fingerprint_scheme")
    spec_hash = obj.get("spec_hash")
    meta = {
        "v1": v1,
        "v2": v2,
        "state": state,
        "scheme": scheme,
        "spec_hash": spec_hash,
        "source": source,
    }
    for key in ("method", "seed"):
        meta[key] = obj.get(key)
    runs = obj.get("runs")
    meta["seeds"] = (
        [r.get("seed") for r in runs if isinstance(r, dict) and "seed" in r]
        if isinstance(runs, list)
        else []
    )
    return meta


def load_run(run_dir: Path) -> dict | None:
    """读一个 run 目录的指纹（receipt 优先，eval_report 兜底）；
    身份元数据（spec_hash/method/seed）与逐集种子两处都收，取并集。"""
    receipt = load_json(run_dir / "lerobot-v21" / "meta" / "oriera_receipt.json")
    report = load_json(run_dir / "eval_report.json")
    meta = None
    if receipt is not None:
        meta = extract_from(receipt, "receipt")
    if meta is None and report is not None:
        meta = extract_from(report, "eval_report")
    if meta is None:
        return None
    # 元数据补全：receipt 里没有的（method/seed/runs[].seed）从 eval_report 拿
    if report is not None:
        for key in ("method", "seed", "spec_hash"):
            if meta.get(key) is None:
                meta[key] = report.get(key)
        runs = report.get("runs")
        if isinstance(runs, list) and not meta["seeds"]:
            meta["seeds"] = [
                r.get("seed") for r in runs if isinstance(r, dict) and "seed" in r
            ]
    return meta


def is_run_dir(p: Path) -> bool:
    return (p / "eval_report.json").is_file() or (p / "lerobot-v21" / "meta" / "oriera_receipt.json").is_file()


def collect_run_dirs(arg: str) -> tuple[list[Path], list[str]]:
    """把一个参数（目录或 glob）解析成 run 目录列表；找不到 run 的路径进 warns。"""
    warns: list[str] = []
    paths = [Path(x) for x in sorted(glob.glob(arg))]
    if not paths:
        p = Path(arg)
        paths = [p] if p.exists() else []
        if not paths:
            warns.append(f"路径不存在：{arg}")
            return [], warns
    runs: list[Path] = []
    for p in paths:
        if is_run_dir(p):
            runs.append(p)
            continue
        if p.is_dir():
            # runs-root 形态（runs/<hash>/<method>/<seed>/）：向下找 run 目录
            found = [
                q
                for q in sorted(p.rglob("eval_report.json"))
                if ".git" not in q.parts and "lerobot-v21" not in q.parts
            ]
            for q in found:
                runs.append(q.parent)
            if not found:
                receipts = sorted(p.rglob("oriera_receipt.json"))
                for q in receipts:
                    runs.append(q.parent.parent.parent)
                if not receipts:
                    warns.append(f"{arg} 下的 {p.name}：没找到 eval_report.json / receipt")
        else:
            warns.append(f"{arg} 下的 {p.name}：不是目录")
    runs = sorted(dict.fromkeys(runs))
    return runs, warns


def view_of(meta: dict, view: str) -> list | None:
    if view == "auto":
        return meta["v1"] if meta["v1"] is not None else meta["v2"]
    return meta.get(view)


def label(p: Path) -> str:
    try:
        return str(p.relative_to(ROOT))
    except ValueError:
        return str(p)


def compare_pair(a_path: Path, b_path: Path, a: dict, b: dict, view: str) -> dict:
    fa, fb = view_of(a, view), view_of(b, view)
    rows: list[dict] = []
    if fa is None or fb is None:
        return {
            "verdict": "incomparable",
            "why": (
                f"{label(a_path)}（{a['source']}）无可比指纹"
                if fa is None
                else f"{label(b_path)}（{b['source']}）无可比指纹"
            ),
            "rows": rows,
        }
    n = min(len(fa), len(fb))
    len_note = "" if len(fa) == len(fb) else f"（集数不同：{len(fa)} vs {len(fb)}，只对前 {n} 集）"
    matched = 0
    for i in range(n):
        same = fa[i] == fb[i]
        matched += same
        rows.append(
            {
                "episode": i,
                "seed_a": (a["seeds"][i] if i < len(a["seeds"]) else ""),
                "seed_b": (b["seeds"][i] if i < len(b["seeds"]) else ""),
                "fp_a": fa[i],
                "fp_b": fb[i],
                "match": int(same),
            }
        )
    full = n > 0 and matched == n and len(fa) == len(fb)
    spec_note = ""
    if full:
        if a.get("spec_hash") and b.get("spec_hash") and a["spec_hash"] != b["spec_hash"]:
            spec_note = "spec_hash 不同而指纹全同——不同实验身份装同一份数据，最强实锤"
        else:
            spec_note = "spec_hash 相同：同 spec 同种子逐位一致本就是引擎确定性契约（LAB-GUIDE §2）——先排除同一实验交两遍/助教复跑，同样不能当两次独立采集计分"
    return {
        "verdict": "duplicate" if full else "distinct",
        "why": f"逐集相同 {matched}/{n} {len_note}",
        "rows": rows,
        "spec_note": spec_note,
    }


def main() -> int:
    ap = argparse.ArgumentParser(
        description="助教防抄袭：两个 run 的逐集指纹对表（全部相同 = 指纹级重复）",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split("用法：")[1] if "用法：" in __doc__ else None,
    )
    ap.add_argument("run_a", help="run 目录或 glob（如 runs/<hash8>/<method>/<seed>/）")
    ap.add_argument("run_b", help="另一个 run 目录或 glob")
    ap.add_argument(
        "--view",
        choices=["auto", "v1", "v2", "state"],
        default="auto",
        help="对表口径：auto=v1 回执口径优先（默认）/ v2=完整身份 / state=纯状态（eval_report 侧才有）",
    )
    ap.add_argument("--csv", default=None, help="把逐集对照表写到这个 CSV（每对 run 一个文件段头）")
    args = ap.parse_args()

    sides_a, warns_a = collect_run_dirs(args.run_a)
    sides_b, warns_b = collect_run_dirs(args.run_b)
    for w in warns_a + warns_b:
        print(f"  ! {w}")
    if not sides_a or not sides_b:
        print("输入解析失败：至少一侧没解析出任何 run 目录（run 目录 = 含 eval_report.json 或 lerobot-v21/meta/oriera_receipt.json）。exit 2")
        return 2

    pairs = [(a, b) for a in sides_a for b in sides_b]
    print(
        f"== 指纹级查重（口径 {args.view}）==  A={len(sides_a)} 个 run × B={len(sides_b)} 个 run = {len(pairs)} 对"
    )
    duplicates = 0
    comparable_pairs = 0
    incomparable_pairs = 0
    csv_rows: list[dict] = []
    for a_path in sides_a:
        a = load_run(a_path)
        for b_path in sides_b:
            b = load_run(b_path)
            print(f"\n--- {label(a_path)}  ×  {label(b_path)}")
            if a is None or b is None:
                miss = label(a_path) if a is None else label(b_path)
                other = b if a is None else a
                hint = "（基线入口 embodied_sim_baseline 的产物不带逐集指纹，属正常）" if other is not None else ""
                print(f"    无法对表：{miss} 没有可读指纹 {hint}")
                incomparable_pairs += 1
                continue
            res = compare_pair(a_path, b_path, a, b, args.view)
            print(
                f"    身份：spec_hash {a.get('spec_hash') or '?'}/{b.get('spec_hash') or '?'}"
                f" · method {a.get('method')}/{b.get('method')} · 指纹来源 {a['source']}/{b['source']}"
            )
            if res["verdict"] == "incomparable":
                print(f"    无法对表：{res['why']}")
                incomparable_pairs += 1
                continue
            comparable_pairs += 1
            for r in res["rows"]:
                r["pair"] = f"{label(a_path)} × {label(b_path)}"
                csv_rows.append(r)
                mark = "同" if r["match"] else "异"
                print(
                    f"    ep{r['episode']:>2}  seed {r['seed_a']} / {r['seed_b']}  "
                    f"{r['fp_a']:>20} / {r['fp_b']:>20}  [{mark}]"
                )
            if res["verdict"] == "duplicate":
                duplicates += 1
                print(f"    >>> 指纹级重复（抄袭实锤）：{res['why']}")
                if res.get("spec_note"):
                    print(f"    >>> {res['spec_note']}")
            else:
                print(f"    未检出指纹级重复：{res['why']}")

    if args.csv:
        with Path(args.csv).open("w", newline="", encoding="utf-8") as f:
            w = csv.DictWriter(f, fieldnames=["pair", "episode", "seed_a", "seed_b", "fp_a", "fp_b", "match"])
            w.writeheader()
            w.writerows(csv_rows)
        print(f"\n对照表已写 {args.csv}（{len(csv_rows)} 行）")

    print()
    if duplicates:
        print(f"结论：检出 {duplicates} 对指纹级重复（逐集全同）——exit 1。判定口径见文件头注释（先核对是否同一实验交两遍）。")
        return 1
    if comparable_pairs:
        print(f"结论：{comparable_pairs} 对可比 run 均未检出指纹级重复——exit 0。")
        return 0
    if incomparable_pairs:
        print(
            f"结论：{incomparable_pairs} 对均无法逐集对表（至少一方不带逐集指纹）——未检出指纹级重复，exit 0；"
            "要逐集对表请用带 receipt/eval_report 指纹的采集产物（collect 入口）。"
        )
        return 0
    print("结论：没有任何可读数据——exit 2。")
    return 2


if __name__ == "__main__":
    sys.exit(main())
