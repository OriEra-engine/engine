#!/usr/bin/env python3
"""Structural self-check for an OriEra LeRobot v3.0 export (P-7 tail).

Checks the on-disk v3.0 contract the lerobot reader will rely on:
required meta files, the info.json schema keys, the data/video path
templates and chunk/file naming, the episodes metadata columns and
contiguity, and (when pyarrow is installed) the parquet tables'
columns/row counts.

HONESTY NOTE: this is a *structure* audit. 本机没有 lerobot，它不能（也
不声称）证明 `LeRobotDataset(repo_id)` 能加载这个目录——那一步要等 CI
钉住 lerobot 版本后跑 `lerobot-train` 1-step 冒烟（M2 验收）。

Usage:
    python scripts/lerobot_v3_selfcheck.py out/embodied-sim/lerobot-v30
    （转换成 parquet 之前后都可跑：JSONL 代理文件按同等语义检查）

Exit code 0 = all checks passed; 1 = at least one failed.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

# Windows GBK console: ✓/✗/中文 need an explicit UTF-8 stdout.
for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

CHUNK_FILE_RE = re.compile(r"^chunk-\d{3}/file-\d{3}$")

REQUIRED_INFO = [
    "codebase_version",
    "robot_type",
    "total_episodes",
    "total_frames",
    "total_tasks",
    "chunks_size",
    "data_files_size_in_mb",
    "video_files_size_in_mb",
    "fps",
    "splits",
    "data_path",
    "features",
]

DATA_TEMPLATE = "data/chunk-{chunk_index:03d}/file-{file_index:03d}.parquet"
VIDEO_TEMPLATE = "videos/{video_key}/chunk-{chunk_index:03d}/file-{file_index:03d}.mp4"

REQUIRED_EPISODE_COLS = [
    "episode_index",
    "length",
    "dataset_from_index",
    "dataset_to_index",
    "data/chunk_index",
    "data/file_index",
    "meta/episodes/chunk_index",
    "meta/episodes/file_index",
]

DATA_SCALAR_COLS = ["index", "episode_index", "frame_index", "task_index", "timestamp"]


class Check:
    def __init__(self) -> None:
        self.ok: list[str] = []
        self.bad: list[str] = []

    def expect(self, cond: bool, msg: str) -> None:
        (self.ok if cond else self.bad).append(msg)

    def report(self) -> int:
        for line in self.ok:
            print(f"  ✓ {line}")
        for line in self.bad:
            print(f"  ✗ {line}")
        print(
            f"\n{len(self.ok)} 项通过，{len(self.bad)} 项失败。"
            "注意：这是结构自检，未经 lerobot 实机加载验证（本机无 lerobot）。"
        )
        return 1 if self.bad else 0


def _read_rows(path: Path) -> tuple[list[dict], str] | None:
    """Rows of an episodes/data file, parquet or JSONL stand-in."""
    if path.suffix == ".parquet":
        try:
            import pyarrow.parquet as pq
        except ImportError:
            return None, "pyarrow 未安装，跳过 parquet 内容检查"
        tbl = pq.read_table(path)
        cols = {name: tbl.column(name).to_pylist() for name in tbl.column_names}
        n = tbl.num_rows
        return [{k: cols[k][i] for k in cols} for i in range(n)], "parquet"
    rows = [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    return rows, "jsonl"


def check(root: Path) -> int:
    c = Check()
    info_path = root / "meta" / "info.json"
    if not info_path.exists():
        c.expect(False, f"{info_path} 存在")
        c.report()
        return 1
    info = json.loads(info_path.read_text(encoding="utf-8"))

    c.expect(info.get("codebase_version") == "v3.0",
             f"codebase_version == v3.0（当前 {info.get('codebase_version')!r}）")
    for key in REQUIRED_INFO:
        c.expect(key in info, f"info.json 有 {key}")
    c.expect(info.get("data_path") == DATA_TEMPLATE,
             f"data_path 模板 == {DATA_TEMPLATE}（当前 {info.get('data_path')!r}）")
    video_path = info.get("video_path")
    c.expect(video_path == VIDEO_TEMPLATE or video_path is None,
             f"video_path 模板合法（当前 {video_path!r}）")
    fps = info.get("fps")
    c.expect(isinstance(fps, (int, float)) and fps > 0, f"fps 为正数（当前 {fps!r}）")
    feats = info.get("features", {})
    for name, feat in feats.items():
        if name in ("timestamp", "frame_index", "episode_index", "index", "task_index"):
            c.expect(feat.get("dtype") == ("float32" if name == "timestamp" else "int64"),
                     f"features[{name}] dtype 合法（{feat.get('dtype')}）")
            continue
        c.expect("dtype" in feat and "shape" in feat,
                 f"features[{name}] 有 dtype+shape")
        shape = feat.get("shape", [])
        c.expect(isinstance(shape, list) and len(shape) >= 1 and shape[0] >= 1,
                 f"features[{name}].shape[0] ≥ 1（{shape}）")

    total_eps = info.get("total_episodes", -1)
    total_frames = info.get("total_frames", -1)

    # meta tasks + episodes metadata（parquet 或 jsonl 代理）
    tasks_p, tasks_f = root / "meta" / "tasks.parquet", root / "meta" / "tasks.jsonl"
    c.expect(tasks_p.exists() or tasks_f.exists(),
             "meta/tasks.parquet 存在（或 jsonl 代理）")
    ep_files = sorted((root / "meta" / "episodes").glob("chunk-*/file-*"))
    c.expect(bool(ep_files), "meta/episodes/chunk-XXX/file-XXX 存在")
    rows: list[dict] = []
    for f in ep_files:
        rel = f.relative_to(root).as_posix()
        c.expect(bool(CHUNK_FILE_RE.match(f.parent.name + "/" + f.stem)),
                 f"{rel} 命名符合 chunk-XXX/file-XXX")
        got, fmt = _read_rows(f)
        if got is None:
            c.expect(False, f"{rel}: {fmt}")
            continue
        rows.extend(got)
        c.expect(True, f"{rel} 可读（{fmt}，{len(got)} 行）")
    if rows:
        cols = rows[0].keys()
        for col in REQUIRED_EPISODE_COLS:
            c.expect(col in cols, f"episodes 元数据有列 {col}")
        for col in list(cols):
            if col.startswith("videos/") and col.endswith(
                ("chunk_index", "file_index", "from_timestamp", "to_timestamp")
            ):
                c.expect(True, f"episodes 元数据有视频指针列 {col}")
        n_tasks = info.get("total_tasks")
        for i, row in enumerate(rows):
            if row.get("episode_index") != i:
                c.expect(False, f"episode_index 连续（第 {i} 行是 {row.get('episode_index')}）")
                break
        else:
            c.expect(True, "episode_index 从 0 连续")
        # dataset_from/to_index 覆盖 0..total_frames 无缝无叠
        cursor = 0
        contiguous = True
        for row in rows:
            if row.get("dataset_from_index") != cursor or row.get(
                "dataset_to_index"
            ) != cursor + row.get("length"):
                contiguous = False
                break
            cursor = row["dataset_to_index"]
        c.expect(contiguous and cursor == total_frames,
                 f"dataset_from/to_index 无缝覆盖 0..{total_frames}（实际到 {cursor}）")
        c.expect(len(rows) == total_eps, f"episodes 行数 == total_episodes（{len(rows)}/{total_eps}）")
        c.expect(sum(r.get("length", 0) for r in rows) == total_frames,
                 "Σ length == total_frames")
        if isinstance(n_tasks, int) and n_tasks > 0:
            first_tasks = rows[0].get("tasks")
            c.expect(isinstance(first_tasks, list) and bool(first_tasks),
                     "episodes 行带 tasks 字符串列表")

    # data files
    data_files = sorted(root.glob("data/chunk-*/file-*"))
    c.expect(bool(data_files), "data/chunk-XXX/file-XXX 存在")
    total_rows = 0
    for f in data_files:
        rel = f.relative_to(root).as_posix()
        c.expect(bool(CHUNK_FILE_RE.match(f.parent.name + "/" + f.stem)),
                 f"{rel} 命名符合 chunk-XXX/file-XXX")
        got, fmt = _read_rows(f)
        if got is None:
            c.expect(False, f"{rel}: {fmt}")
            continue
        total_rows += len(got)
        need = set(DATA_SCALAR_COLS) | set(
            k for k in feats if k not in ("timestamp", "frame_index", "episode_index", "index", "task_index")
            and feats[k].get("dtype") != "video"
        )
        missing = need - set(got[0].keys()) if got else need
        c.expect(not missing, f"{rel} 数据列齐全（{fmt}，缺 {sorted(missing) if missing else '无'}）")
        if got:
            c.expect(got[0].get("timestamp") == 0.0,
                     f"{rel} 第一行 timestamp == 0（episode 内秒数）")
    c.expect(total_rows == total_frames, f"数据行数 == total_frames（{total_rows}/{total_frames}）")

    # video files / frame dirs
    if video_path is not None:
        frame_dirs = sorted(root.glob("videos/*/chunk-*/file-*"))
        mp4s = sorted(root.glob("videos/*/chunk-*/file-*.mp4"))
        c.expect(bool(frame_dirs or mp4s), "videos/<key>/chunk-XXX/file-XXX 存在（帧目录或 mp4）")

    # stats.json（真实数据集惯例：只含观测/动作/时间戳，不含簿记整型列）
    stats = root / "meta" / "stats.json"
    if stats.exists():
        st = json.loads(stats.read_text(encoding="utf-8"))
        for name, feat in feats.items():
            if feat.get("dtype") in ("video", "int64"):
                continue
            c.expect(name in st, f"stats.json 有 {name}")
    else:
        c.expect(False, "meta/stats.json 存在")

    return c.report()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    sys.exit(check(Path(sys.argv[1])))
