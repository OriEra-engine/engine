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

    for src in eps:
        rows = [json.loads(line) for line in src.read_text(encoding="utf-8").splitlines() if line.strip()]
        columns = {}
        for key, feat in features.items():
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
        src.unlink()
        print(f"  {src.name} → {dst.name}  ({table.num_rows} rows)")

    print(f"done: {len(eps)} episode(s) under {root}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    convert(Path(sys.argv[1]))
