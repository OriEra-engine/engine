#!/usr/bin/env python3
"""Convert an OriEra embodied JSONL dataset into a native LeRobotDataset v2.1.

The engine writes rows as JSONL (numeric-only, zero-dependency — see
crates/oriera-agent/src/lerobot.rs). This script converts each episode
to parquet 1:1 (column names and dtypes already follow the v2.1
contract), removes the JSONL sources, and leaves the directory directly
readable by `LeRobotDataset(repo_id)` / `lerobot-train`.

Usage:
    python scripts/jsonl_to_lerobot_v21.py out/embodied-sim/lerobot-v21

Requires: python -m pip install pyarrow
"""

from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

DTYPES = {"float32": pa.float32(), "int64": pa.int64()}


def convert(root: Path) -> None:
    info = json.loads((root / "meta" / "info.json").read_text(encoding="utf-8"))
    features = info["features"]
    eps = sorted((root / "data" / "chunk-000").glob("episode_*.jsonl"))
    if not eps:
        sys.exit(f"no JSONL episodes under {root / 'data' / 'chunk-000'}")

    video_feats = {k: v for k, v in features.items() if v.get("dtype") == "video"}
    table_feats = {k: v for k, v in features.items() if k not in video_feats}

    for src in eps:
        rows = [json.loads(line) for line in src.read_text(encoding="utf-8").splitlines() if line.strip()]
        columns = {}
        # v2.1: video features live OUTSIDE the parquet tables (mp4 files
        # aligned by frame_index) — only table features become columns.
        for key, feat in table_feats.items():
            dtype = DTYPES[feat["dtype"]]
            shape = feat.get("shape", [1])
            if len(shape) > 1:  # vector feature → fixed-size list
                values = [row[key] for row in rows]
                columns[key] = pa.array(values, pa.list_(dtype, shape[0]))
            else:
                values = [row[key][0] if isinstance(row[key], list) else row[key] for row in rows]
                columns[key] = pa.array(values, dtype)
        table = pa.table(columns)
        dst = src.with_suffix(".parquet")
        pq.write_table(table, dst)
        # 非破坏性：源 JSONL 归档到 raw/（调试与审计留底），不再删除
        raw = root / "raw_jsonl"
        raw.mkdir(exist_ok=True)
        src.rename(raw / src.name)
        print(f"  {src.name} → {dst.name}  ({table.num_rows} rows)")

    # Camera tracks: assemble frame_*.png dirs into mp4 (LeRobot v2.1
    # video layout), or keep the PNG dirs when imageio is unavailable.
    mp4_made = 0
    try:
        import imageio.v2 as iio  # noqa: SLF001 — optional extra

        for cam_dir in sorted((root / "videos" / "chunk-000").glob("observation.images.*")):
            for ep_dir in sorted(cam_dir.glob("episode_*")):
                frames = sorted(ep_dir.glob("frame_*.png"))
                if not frames:
                    continue
                mp4 = ep_dir.parent / f"{ep_dir.name}.mp4"
                fps = next(
                    (v.get("info", {}).get("video.fps", 60.0) for k, v in video_feats.items() if k.endswith("." + cam_dir.name)),
                    60.0,
                )
                with iio.get_writer(mp4, fps=fps, codec="libx264", pixelformat="yuv420p", macro_block_size=1) as w:
                    for f in frames:
                        w.append_data(iio.imread(f))
                shutil.rmtree(ep_dir)
                mp4_made += 1
    except ImportError:
        # 静默保留 PNG 目录会让 lerobot reader 挂掉——这里显式报错退出，
        # 由用户装好 imageio 后重跑（非破坏，可重入）。
        sys.exit("错误：检测到相机帧目录但缺 imageio[ffmpeg]——pip install imageio imageio-ffmpeg 后重跑")

    print(f"done: {len(eps)} episode(s) under {root}" + (f"，{mp4_made} 段相机视频" if mp4_made else ""))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    convert(Path(sys.argv[1]))
