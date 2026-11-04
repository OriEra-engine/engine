#!/usr/bin/env python3
"""OriEra release 版本一致性守卫（PLAN-embodied-v3 §三 P2「release 工程线」）。

背景（CI/CD 工程师审查）：engine 的版本号散布在多处，发版靠人肉同步——
`[workspace.package] version`、`[workspace.dependencies]` 里每个 oriera-* 的
自引用 version、`pyproject.toml [project] version`。漏改一处，`cargo publish`
或 maturin wheel 就会带错版本。本脚本把「发版前人眼对数」变成一条可进 CI 的
机械检查。

检查面：
  1. 根 Cargo.toml `[workspace.package] version`（基准值）
  2. 根 Cargo.toml `[workspace.dependencies]` 全部 oriera-* 自引用版本
     （动态发现，后续新增 oriera-wasm 等条目自动纳入，不用改脚本）
  3. 全部 workspace 成员 crate 的 `[package] version`——显式版本必须等于
     基准；`version.workspace = true` 视为继承（OK）。覆盖任务里点名的
     crates/oriera-agent（当前即继承，无独立版本）
  4. pyproject.toml `[project] version`（oriera-sim wheel，maturin）
  5. （可选）studio 仓 VERSION.json——传 `--studio-root <路径>` 才查；
     文件不存在只警告不拦（studio 版本锚点是 W5 发布线的交付，当前尚未建立）

用法：
  python scripts/check_release_versions.py                    # 只查 engine 侧
  python scripts/check_release_versions.py --studio-root ../studio
  python scripts/check_release_versions.py --expect-version 0.11.0
      # release.yml 的 tag 触发腿用：额外断言基准版本 == tag 去掉 v 前缀

不一致 exit 1 并列出每处路径与值；一致 exit 0。只读不改——改版本号是人的决定，
脚本只负责把不一致暴露出来。
"""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path

# Windows 控制台默认 cp936/gbk：打非 ASCII 直接 UnicodeEncodeError
# （P0-7 同款预防，gen_spec_index.py 先例）。
for _st in (sys.stdout, sys.stderr):
    if hasattr(_st, "reconfigure"):
        _st.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent

# 输出尽量 ASCII（OK / FAIL / 继承），中文正常——两边在 utf-8 reconfigure 后都安全。
MARK_OK = "OK"
MARK_BASE = "基准"
MARK_INHERIT = "OK(继承)"
MARK_FAIL = "FAIL"
MARK_WARN = "WARN(缺文件)"


def load_toml(path: Path) -> dict:
    with path.open("rb") as f:
        return tomllib.load(f)


def collect() -> tuple[str, list[dict], list[str]]:
    """返回 (基准版本, 检查行列表, 警告列表)。行 = {loc, value, status}。"""
    rows: list[dict] = []
    warns: list[str] = []

    ws = load_toml(ROOT / "Cargo.toml")
    base = ws.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(base, str) or not base:
        raise SystemExit(
            "[check_release_versions] 根 Cargo.toml 缺 [workspace.package] version——"
            "无法确定基准版本，先补上再跑"
        )
    rows.append({"loc": "Cargo.toml [workspace.package] version", "value": base, "status": MARK_BASE})

    deps = ws.get("workspace", {}).get("dependencies", {})
    oriera_deps = {
        k: v for k, v in deps.items() if k.startswith("oriera-") and isinstance(v, dict)
    }
    if not oriera_deps:
        raise SystemExit(
            "[check_release_versions] [workspace.dependencies] 里没找到任何 oriera-* "
            "自引用条目——解析逻辑失效或 Cargo.toml 结构变了，人工核对"
        )
    for name in sorted(oriera_deps):
        ver = oriera_deps[name].get("version")
        rows.append(
            {
                "loc": f"Cargo.toml [workspace.dependencies] {name}",
                "value": ver,
                "status": MARK_OK if ver == base else MARK_FAIL,
            }
        )

    # 成员 crate：显式 version 必须等于基准；version.workspace=true = 继承。
    members = ws.get("workspace", {}).get("members", [])
    for member in members:
        crate_toml = ROOT / member / "Cargo.toml"
        if not crate_toml.is_file():
            warns.append(f"成员 {member}: Cargo.toml 不存在（跳过）")
            continue
        pkg = load_toml(crate_toml).get("package", {})
        ver = pkg.get("version")
        if ver is True or isinstance(ver, dict):  # version.workspace = true
            rows.append({"loc": f"{member} version.workspace=true", "value": "(继承)", "status": MARK_INHERIT})
        elif isinstance(ver, str):
            rows.append({"loc": f"{member} [package] version（显式）", "value": ver, "status": MARK_OK if ver == base else MARK_FAIL})
        else:
            rows.append({"loc": f"{member} [package] version", "value": "(缺)", "status": MARK_FAIL})

    pyproject = load_toml(ROOT / "pyproject.toml")
    py_ver = pyproject.get("project", {}).get("version")
    rows.append(
        {
            "loc": "pyproject.toml [project] version (oriera-sim wheel)",
            "value": py_ver,
            "status": MARK_OK if py_ver == base else MARK_FAIL,
        }
    )

    return base, rows, warns


def check_studio(studio_root: Path, base: str, rows: list[dict], warns: list[str]) -> None:
    vjson = studio_root / "VERSION.json"
    if not vjson.is_file():
        pkg_json = studio_root / "package.json"
        ctx = ""
        if pkg_json.is_file():
            try:
                pv = json.loads(pkg_json.read_text(encoding="utf-8")).get("version")
                if pv:
                    ctx = f"（package.json 是 {pv}，未对账）"
            except (OSError, ValueError):
                pass
        warns.append(
            f"studio {vjson} 不存在，跳过对账{ctx}——studio 版本锚点是 W5 发布线交付，"
            "建立后此警告自动消失"
        )
        rows.append({"loc": f"studio VERSION.json ({studio_root})", "value": "(缺)", "status": MARK_WARN})
        return
    try:
        data = json.loads(vjson.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        rows.append({"loc": str(vjson), "value": f"(解析失败: {e})", "status": MARK_FAIL})
        return
    ver = data.get("version") if isinstance(data, dict) else None
    rows.append({"loc": f"studio VERSION.json ({studio_root})", "value": ver, "status": MARK_OK if ver == base else MARK_FAIL})


def main() -> int:
    ap = argparse.ArgumentParser(description="OriEra release 版本一致性守卫（只读）")
    ap.add_argument("--studio-root", type=Path, default=None, help="studio 仓根目录（查 VERSION.json）；不给则只查 engine 侧")
    ap.add_argument(
        "--expect-version",
        default=None,
        help="额外断言基准版本等于该值（release.yml tag 触发腿传 tag 去掉 v 前缀）",
    )
    args = ap.parse_args()

    base, rows, warns = collect()

    if args.studio_root is not None:
        check_studio(args.studio_root, base, rows, warns)
    else:
        print("（未传 --studio-root：只查 engine 侧）")

    if args.expect_version is not None and base != args.expect_version:
        rows.append({"loc": "--expect-version（如 tag v0.11.0 → 0.11.0）", "value": args.expect_version, "status": MARK_FAIL})

    width = max(len(r["loc"]) for r in rows) + 2
    print(f"== OriEra release 版本一致性 ==（基准 {base}，共 {len(rows)} 处）")
    print()
    for r in rows:
        val = r["value"] if r["value"] is not None else "(缺)"
        print(f"  {r['loc']:<{width}} {str(val):<10} {r['status']}")

    if warns:
        print()
        for w in warns:
            print(f"  WARN: {w}")

    failed = [r for r in rows if r["status"] == MARK_FAIL]
    if failed:
        print()
        print(f"不一致 {len(failed)} 处——发版前逐处改齐（或改基准），exit 1：")
        for r in failed:
            print(f"  FAIL {r['loc']} = {r['value']}（基准 {base}）")
        return 1

    print()
    print(f"全部一致（{len(rows)} 处，基准 {base}）——release 就绪。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
