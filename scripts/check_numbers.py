#!/usr/bin/env python3
"""文档数字守卫（DX 评审 P1，W8）：README/GUIDE 里写死的数量声明与
仓库实物对拍——漂移数小时内复发过两次（11 个 demo 实为 14、测试数
53 实为 59），一次脚本根治这类事故。

机器可数项才进本脚本；测试总数以 CI 实测为准（不在静态对拍范围）。
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FAIL = []


def demo_dirs() -> list:
    return sorted(
        d.name
        for d in (ROOT / "examples").iterdir()
        if d.is_dir() and re.match(r"^\d\d-", d.name)
    )


def count_in(path: str, pattern: str) -> list:
    text = (ROOT / path).read_text(encoding="utf-8")
    return re.findall(pattern, text)


def main() -> int:
    demos = demo_dirs()
    n_demos = len(demos)

    # 1) README「可玩 demo」数声明与实物一致（中英任一处声明即查）
    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    for m in re.finditer(r"(\d+)\s*[个款]?\s*(?:可玩\s*)?demo", readme, re.I):
        claimed = int(m.group(1))
        if claimed != n_demos:
            FAIL.append(
                f"README 声明 {claimed} 个 demo，实物 {n_demos} 个"
                f"（{', '.join(demos)}）"
            )

    # 2) OPEN_SOURCE_GUIDE 同款声明
    guide = ROOT / "docs" / "OPEN_SOURCE_GUIDE.md"
    if guide.exists():
        gtext = guide.read_text(encoding="utf-8")
        for m in re.finditer(r"(\d+)\s*个[个款]?\s*(?:浏览器\s*)?(?:可玩\s*)?demo", gtext):
            claimed = int(m.group(1))
            if claimed != n_demos:
                FAIL.append(
                    f"OPEN_SOURCE_GUIDE.md 声明 {claimed} 个 demo，实物 {n_demos}"
                )

    # 3) 每个示例目录都有编号前缀（命名规范守卫）
    for d in demos:
        if not (ROOT / "examples" / d / "index.html").exists() and not (
            ROOT / "examples" / d / "engine"
        ).exists():
            FAIL.append(f"examples/{d} 缺 index.html 与 engine/（非完整 demo 目录）")

    # 4) ARCHITECTURE.md 层数声明：十一层清单行数
    arch = (ROOT / "docs" / "ARCHITECTURE.md").read_text(encoding="utf-8")
    layer_rows = re.findall(r"^\| L(\d+) \|", arch, re.M)
    n_layers = len(set(layer_rows))
    if n_layers < 11:
        FAIL.append(f"ARCHITECTURE.md 层行只覆盖 {n_layers}/11 层")

    if FAIL:
        print("数字守卫（W8）未通过：")
        for f in FAIL:
            print(f"  - {f}")
        return 1

    print(f"数字守卫（W8）通过：{n_demos} 个 demo 目录，11 层清单覆盖。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
