#!/usr/bin/env python3
"""Convert an OriEra embodied JSONL dataset into a native LeRobotDataset.

The engine writes rows as JSONL (numeric-only, zero-dependency — see
crates/oriera-agent/src/lerobot.rs). This script converts each file to
parquet 1:1 (column names and dtypes already follow the contract),
archives the JSONL sources under `raw_jsonl/`, and leaves the
directory directly readable by `LeRobotDataset(repo_id)` /
`lerobot-train`.

The layout is auto-detected from `meta/info.json`:

- v2.1: `data/chunk-000/episode_XXXXXX.jsonl` per episode,
  `videos/chunk-000/<cam>/episode_XXXXXX/` per-episode frame dirs.
- v3.0 (`--lerobot-v3` exports): consolidated `data/chunk-000/file-000.jsonl`,
  `meta/tasks.jsonl` → `meta/tasks.parquet`,
  `meta/episodes/chunk-000/file-000.jsonl` → parquet,
  shared `videos/<cam>/chunk-000/file-000/` frame dirs → one mp4 per file.

Re-entrant (WPC 实测修复): converted sources live in `raw_jsonl/`;
re-running a converted directory is a friendly no-op, not an error,
and re-archiving over an existing copy uses os.replace (Windows-safe).

MANIFEST 提交点语义 (DE-P1，PLAN-embodied-v3 §二 #14/#20)：引擎直出的
目录以 `MANIFEST.jsonl` 为原子提交点，清单里的行文件转换后已归档进
`raw_jsonl/`——原清单在新布局上失效。转换收尾时本脚本在 `raw_jsonl/`
同级落 `MANIFEST.converted.json`（原 MANIFEST 全部条目 +
`"converted": true, "converted_at": <utc>`）：**转换后原 MANIFEST 只
指向 raw_jsonl 存档，本文件才是转换后目录的提交点**；selfcheck
（`lerobot_v3_selfcheck.py --v21`）见该文件即按「已转换」口径对账
（原清单哈希逐条对 raw_jsonl/ 里的归档核验）。

Usage:
    python scripts/jsonl_to_lerobot_v21.py out/embodied-sim/lerobot-v21

Requires: python -m pip install pyarrow （相机轨另需 imageio imageio-ffmpeg）
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from datetime import datetime, timezone
from pathlib import Path

# Windows GBK console: 中文输出需显式 UTF-8。
for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

import pyarrow as pa
import pyarrow.parquet as pq

DTYPES = {"float32": pa.float32(), "float64": pa.float64(), "int64": pa.int64()}


def _utcnow() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_converted_manifest(root: Path) -> Path:
    """DE-P1：转换收尾落 `MANIFEST.converted.json`（raw_jsonl/ 同级）。

    内容 = 原 `MANIFEST.jsonl` 全部条目 + `converted`/`converted_at`
    标记。语义（README 同款说明）：转换后原 MANIFEST 指向的行文件已
    归档进 raw_jsonl/，其哈希仍可对归档逐条对账；`MANIFEST.converted.
    json` 是转换后目录的提交点。幂等：重跑只刷新 converted_at。
    """
    manifest_path = root / "MANIFEST.jsonl"
    entries: list[dict] = []
    note = None
    if manifest_path.exists():
        for line in manifest_path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                entries.append(json.loads(line))
    else:
        note = "原 MANIFEST.jsonl 缺失（非引擎直出目录）——条目为空，仅标记 converted"
    out = {
        "converted": True,
        "converted_at": _utcnow(),
        "source_manifest": "MANIFEST.jsonl",
        "note": (
            "转换后原 MANIFEST.jsonl 指向的行文件已归档至 raw_jsonl/；"
            "本文件为转换后目录的提交点，entries 为原清单逐条内容。"
        ),
    }
    if note:
        out["note"] = f"{out['note']}（{note}）"
    out["entries"] = entries
    dst = root / "MANIFEST.converted.json"
    dst.write_text(
        json.dumps(out, ensure_ascii=False, indent=1) + "\n", encoding="utf-8"
    )
    print(f"  {dst.name}  落盘（提交点：转换后目录，{len(entries)} 条原清单条目）")
    return dst


def _archive(src: Path, raw: Path) -> None:
    """Move a converted JSONL into raw_jsonl/ (overwrite-safe on Windows)."""
    raw.mkdir(exist_ok=True)
    dst = raw / src.name
    if dst.exists():
        os.replace(src, dst)  # atomic overwrite; rename() raises on Windows
    else:
        os.replace(src, dst)


def _infer_type(values: list) -> pa.DataType:
    """Infer an arrow type from a JSON column's values (v3 meta tables)."""
    for v in values:
        if v is None:
            continue
        if isinstance(v, bool):
            return pa.bool_()
        if isinstance(v, int):
            return pa.int64()
        if isinstance(v, float):
            return pa.float64()
        if isinstance(v, str):
            return pa.string()
        if isinstance(v, list):
            inner = _infer_type(v)
            return pa.list_(inner)
    return pa.string()


def _scalar(v):
    return v[0] if isinstance(v, list) else v


def convert_rows(rows: list[dict], table_feats: dict) -> pa.Table:
    """JSONL rows → typed table for the declared non-video features."""
    columns = {}
    for key, feat in table_feats.items():
        dtype = DTYPES.get(feat["dtype"], pa.float32() if feat["dtype"] == "float32" else pa.float64())
        shape = feat.get("shape", [1])
        # P0 修复（数据工程师深检）：判据是 shape[0]（元素数）而非
        # len(shape)（维数）——[21] 一维矢量曾被误判为标量，每行
        # 只留第 0 维，20/21 维训练数据静默丢失。
        if shape[0] > 1:  # vector feature → fixed-size list
            values = [row[key] for row in rows]
            columns[key] = pa.array(values, pa.list_(dtype, shape[0]))
        else:
            values = [_scalar(row[key]) for row in rows]
            columns[key] = pa.array(values, dtype)
    table = pa.table(columns)
    # 写后断言：parquet 字段结构必须与 info.json 声明一致
    for key, feat in table_feats.items():
        f = table.schema.field(key)
        declared = feat.get("shape", [1])
        if declared[0] > 1:
            is_vec = pa.types.is_list(f.type) or pa.types.is_fixed_size_list(f.type)
            actual = f.type.list_size if is_vec else -1
            assert actual == declared[0], (
                f"字段 {key}: parquet list_size {actual} != 声明 {declared[0]}")
    return table


def convert_v21_data(root: Path, features: dict) -> int:
    """v2.1: per-episode JSONL → per-episode parquet."""
    eps = sorted((root / "data" / "chunk-000").glob("episode_*.jsonl"))
    if not eps:
        return 0
    video_feats = {k: v for k, v in features.items() if v.get("dtype") == "video"}
    # M3-A 修复：image（depth.*）特性是帧目录轨，从不进 JSONL 行——
    # 只排除会 KeyError（实测 convert 任何带深度导出的 M3-A 数据集即崩）。
    table_feats = {
        k: v for k, v in features.items()
        if v.get("dtype") not in ("video", "image")
    }
    raw = root / "raw_jsonl"
    for src in eps:
        rows = [json.loads(line) for line in src.read_text(encoding="utf-8").splitlines() if line.strip()]
        table = convert_rows(rows, table_feats)
        dst = src.with_suffix(".parquet")
        pq.write_table(table, dst)
        # 非破坏性：源 JSONL 归档到 raw_jsonl/（调试与审计留底），
        # 已有同名旧档用 os.replace 原子覆盖（Windows rename 会炸）。
        _archive(src, raw)
        print(f"  {src.name} → {dst.name}  ({table.num_rows} rows)")
    return len(eps)


def convert_v30(root: Path, features: dict) -> int:
    """v3.0: consolidated file-*.jsonl → parquet（data/episodes/tasks）."""
    converted = 0
    # 同 convert_v21_data：image（depth.*）是帧目录轨，不进行文件
    table_feats = {
        k: v for k, v in features.items()
        if v.get("dtype") not in ("video", "image")
    }
    raw = root / "raw_jsonl"
    for src in sorted(root.glob("data/chunk-*/file-*.jsonl")):
        rows = [json.loads(line) for line in src.read_text(encoding="utf-8").splitlines() if line.strip()]
        table = convert_rows(rows, table_feats)
        dst = src.with_suffix(".parquet")
        pq.write_table(table, dst)
        _archive(src, raw)
        print(f"  {src.relative_to(root)} → {dst.relative_to(root)}  ({table.num_rows} rows)")
        converted += 1
    # meta/tasks.jsonl → meta/tasks.parquet（index=task 字符串，
    # 列=task_index；无 pandas 时退化为带 task 列的 pyarrow 表）
    tasks_src = root / "meta" / "tasks.jsonl"
    if tasks_src.exists():
        tasks = [json.loads(line) for line in tasks_src.read_text(encoding="utf-8").splitlines() if line.strip()]
        try:
            import pandas as pd

            df = pd.DataFrame(
                {"task_index": [t["task_index"] for t in tasks]},
                index=pd.Index([t["task"] for t in tasks], name="task"),
            )
            df.to_parquet(root / "meta" / "tasks.parquet")
        except ImportError:
            tbl = pa.table({
                "task_index": pa.array([t["task_index"] for t in tasks], pa.int64()),
                "task": pa.array([t["task"] for t in tasks], pa.string()),
            })
            pq.write_table(tbl, root / "meta" / "tasks.parquet")
        _archive(tasks_src, raw)
        print("  meta/tasks.jsonl → meta/tasks.parquet")
    # meta/episodes/chunk-*/file-*.jsonl → parquet（列类型从值推断：
    # stats/* 为 list<double>（count 为 int64），tasks 为 list<string>）
    for src in sorted(root.glob("meta/episodes/chunk-*/file-*.jsonl")):
        rows = [json.loads(line) for line in src.read_text(encoding="utf-8").splitlines() if line.strip()]
        columns = {}
        for key in rows[0]:
            values = [row[key] for row in rows]
            t = _infer_type(values)
            if pa.types.is_list(t):
                columns[key] = pa.array(values, t)
            else:
                columns[key] = pa.array([_scalar(v) for v in values], t)
        dst = src.with_suffix(".parquet")
        pq.write_table(pa.table(columns), dst)
        _archive(src, raw)
        print(f"  {src.relative_to(root)} → {dst.relative_to(root)}  ({len(rows)} episodes)")
        converted += 1
    return converted


def convert_videos(root: Path, features: dict, version: str) -> int:
    """Frame PNG dirs → mp4 (v2.1 per-episode; v3.0 one mp4 per file)."""
    mp4_made = 0
    pending = []
    if version == "v3.0":
        # videos/<cam>/chunk-000/file-000/{frame_*.png} → ../file-000.mp4
        for frame_dir in sorted(root.glob("videos/*/chunk-*/file-*")):
            if frame_dir.is_dir() and any(frame_dir.glob("frame_*.png")):
                pending.append(frame_dir)
    else:
        # videos/chunk-000/<cam>/episode_XXXXXX/{frame_*.png} → ../episode_XXXXXX.mp4
        for cam_dir in sorted((root / "videos" / "chunk-000").glob("observation.images.*")):
            for ep_dir in sorted(cam_dir.glob("episode_*")):
                if any(ep_dir.glob("frame_*.png")):
                    pending.append(ep_dir)
    if not pending:
        return 0
    try:
        import imageio.v2 as iio  # noqa: SLF001 — optional extra
    except ImportError:
        # 静默保留 PNG 目录会让 lerobot reader 挂掉——这里显式报错退出，
        # 由用户装好 imageio 后重跑（非破坏，可重入）。
        sys.exit("错误：检测到相机帧目录但缺 imageio[ffmpeg]——pip install imageio imageio-ffmpeg 后重跑")
    for frame_dir in pending:
        frames = sorted(frame_dir.glob("frame_*.png"))
        mp4 = frame_dir.parent / f"{frame_dir.name}.mp4"
        fps = 60.0
        for key, feat in features.items():
            if feat.get("dtype") == "video" and key in str(frame_dir):
                fps = feat.get("info", {}).get("video.fps", 60.0)
        with iio.get_writer(mp4, fps=fps, codec="libx264", pixelformat="yuv420p", macro_block_size=1) as w:
            for f in frames:
                w.append_data(iio.imread(f))
        # 写后即验：mp4 帧数必须等于 PNG 帧数（off-by-one 实录）
        check = iio.get_reader(mp4)
        n = sum(1 for _ in check)
        if n != len(frames):
            sys.exit(f"错误：{mp4} 帧数 {n} != PNG {len(frames)}——保留 PNG 目录供排查")
        shutil.rmtree(frame_dir)
        mp4_made += 1
        print(f"  {mp4.relative_to(root)}  ({n} 帧)")
    return mp4_made


def convert(root: Path) -> None:
    info_path = root / "meta" / "info.json"
    if not info_path.exists():
        sys.exit(f"错误：{info_path} 不存在——这不是引擎导出的数据集目录")
    info = json.loads(info_path.read_text(encoding="utf-8"))
    version = info.get("codebase_version", "v2.1")
    features = info["features"]

    if version == "v3.0":
        n_eps = convert_v30(root, features)
    else:
        n_eps = convert_v21_data(root, features)

    if n_eps == 0:
        # 可重入（WPC 实测）：转换后的目录里 JSONL 已进 raw_jsonl/，
        # parquet 已在——友好退出，不是报错。
        has_parquet = any(root.glob("data/chunk-*/*.parquet"))
        if has_parquet:
            print(f"已转换过了：{root} 下没有待转换的 JSONL（原始行在 raw_jsonl\\ 留底），无需重复转换。")
            # 幂等补漏：老版本转换的目录可能没有 converted 标记——补上
            # （DE-P1：转换后目录必须能被 selfcheck 识别为已转换）。
            if not (root / "MANIFEST.converted.json").exists():
                write_converted_manifest(root)
        else:
            sys.exit(
                f"错误：{root} 下既没有待转换的 JSONL 也没有 parquet——"
                "先跑采集（LAB-GUIDE §1 第 4 条）再转换。"
            )
        return

    mp4_made = convert_videos(root, features, version)
    write_converted_manifest(root)
    print(
        f"done: {n_eps} 个数据文件已转换 under {root}"
        + (f"，{mp4_made} 段相机视频" if mp4_made else "")
    )


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    convert(Path(sys.argv[1]))
