#!/usr/bin/env python3
"""Generate ``specs/INDEX.json`` — the spec library registry (M0b, PLAN
§9.1 P-9): ``meta.name → spec_hash`` over every spec under
``crates/oriera-embodied/specs/`` (official + lab + legacy root files).

Hashes come from the ENGINE (``embodied_sim_collect --validate FILE``
prints ``spec_hash <hex16>``) — never re-implemented here, so ``extends``
resolution, typed round-trip defaults and axis canonicalization are the
one and only definition.

Rule: the same name with two different hashes is an error (exit 1) —
two labs cannot both call their variant ``tabletop-4dof-pick``. The same
hash under two names is fine (an alias of one experiment) and is listed.

Usage:
    python scripts/gen_spec_index.py                  # writes specs/INDEX.json
    python scripts/gen_spec_index.py --check          # no write; exit 1 if INDEX.json is stale
    python scripts/gen_spec_index.py --exe target/debug/examples/embodied_sim_collect.exe
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys

# Windows 控制台默认 cp1252/gbk：打 ✓/✗ 直接 UnicodeEncodeError（20 角色审查 P0-7）
for _st in (sys.stdout, sys.stderr):
    if hasattr(_st, "reconfigure"):
        _st.reconfigure(encoding="utf-8", errors="replace")
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
SPECS = REPO / "crates" / "oriera-embodied" / "specs"
INDEX = SPECS / "INDEX.json"
HASH_RE = re.compile(r"spec_hash ([0-9a-f]{16})")


def find_exe(explicit: str | None) -> list[str]:
    """The validator command: a prebuilt example binary, or `cargo run`."""
    if explicit:
        return [explicit]
    for cand in (
        REPO / "target" / "debug" / "examples" / "embodied_sim_collect.exe",
        REPO / "target" / "debug" / "examples" / "embodied_sim_collect",
        REPO / "target" / "release" / "examples" / "embodied_sim_collect.exe",
        REPO / "target" / "release" / "examples" / "embodied_sim_collect",
    ):
        if cand.exists():
            return [str(cand)]
    return [
        "cargo", "run", "-q", "-p", "oriera-agent", "--example", "embodied_sim_collect",
        "--features", "rigid3d,cam,spec,urdf", "--",
    ]


def spec_hash(exe: list[str], path: Path) -> str:
    out = subprocess.run(
        exe + ["--validate", str(path)],
        capture_output=True, text=True, encoding="utf-8", errors="replace", cwd=REPO,
    )
    if out.returncode != 0:
        raise SystemExit(f"[gen_spec_index] {path}: 校验失败\n{out.stdout}{out.stderr}")
    m = HASH_RE.search(out.stdout)
    if not m:
        raise SystemExit(f"[gen_spec_index] {path}: 没在输出里找到 spec_hash\n{out.stdout}")
    return m.group(1)


def spec_name(path: Path) -> str:
    """`meta.name` when present (overlays may set it), else the file stem."""
    try:
        with open(path, encoding="utf-8") as f:
            v = json.load(f)
        name = (v.get("meta") or {}).get("name")
        if isinstance(name, str) and name.strip():
            return name.strip()
    except (OSError, json.JSONDecodeError):
        pass
    return path.stem


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--specs", default=str(SPECS), help="spec 库根目录（默认 crates/oriera-embodied/specs）")
    ap.add_argument("--exe", default=None, help="embodied_sim_collect 可执行文件路径（默认自动找 target/，找不到用 cargo run）")
    ap.add_argument("--check", action="store_true", help="只校验现有 INDEX.json 是否最新，不写")
    args = ap.parse_args()

    root = Path(args.specs)
    exe = find_exe(args.exe)
    files = sorted(
        p for p in root.rglob("*.json")
        if p.name != "INDEX.json"
    )
    if not files:
        print(f"[gen_spec_index] {root} 下没有 spec", file=sys.stderr)
        return 1

    by_name: dict[str, dict] = {}
    conflicts: list[str] = []
    for p in files:
        rel = p.relative_to(root).as_posix()
        h = spec_hash(exe, p)
        name = spec_name(p)
        tier = "official" if rel.startswith("official/") else ("lab" if rel.startswith("lab/") else "legacy")
        entry = by_name.setdefault(name, {"hash": h, "files": [], "tier": tier})
        if entry["hash"] != h:
            conflicts.append(
                f"  同名异 hash：{name!r}\n    {entry['files'][0]}  spec_hash {entry['hash']}\n    {rel}  spec_hash {h}"
            )
        entry["files"].append(rel)
        if tier == "official":
            entry["tier"] = "official"
        print(f"  {name:32s} {h}  {rel}")

    if conflicts:
        print("\n[gen_spec_index] 错误：spec 库里同名异 hash（同一个名字只能是一份实验身份；改了字段就另起名字）：", file=sys.stderr)
        for c in conflicts:
            print(c, file=sys.stderr)
        return 1

    # aliases: same hash under several names — informational
    by_hash: dict[str, list[str]] = {}
    for name, e in by_name.items():
        by_hash.setdefault(e["hash"], []).append(name)
    aliases = {h: sorted(ns) for h, ns in by_hash.items() if len(ns) > 1}

    index = {
        "schema": "oriera.spec-index/1",
        "root": root.relative_to(REPO).as_posix() if root.is_relative_to(REPO) else str(root),
        "rule": "name → spec_hash；同名异 hash 报错；同 hash 异名 = 别名（见 aliases）",
        "specs": {name: by_name[name] for name in sorted(by_name)},
        "aliases": aliases,
    }
    text = json.dumps(index, indent=1, ensure_ascii=False) + "\n"
    out_path = root / "INDEX.json"
    if args.check:
        current = out_path.read_text(encoding="utf-8") if out_path.exists() else ""
        if current != text:
            print(f"[gen_spec_index] {out_path} 已过期，重新生成：python scripts/gen_spec_index.py", file=sys.stderr)
            return 1
        print(f"[gen_spec_index] {out_path} 最新 ✓")
        return 0
    out_path.write_text(text, encoding="utf-8")
    print(f"\n[gen_spec_index] 写出 {out_path}（{len(by_name)} 个名字，{len(files)} 个文件，{len(aliases)} 组别名）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
