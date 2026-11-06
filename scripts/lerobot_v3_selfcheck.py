#!/usr/bin/env python3
"""Structural self-check for an OriEra LeRobot export (P-7 tail + v2.1).

Two layouts, one entrypoint:

    python scripts/lerobot_v3_selfcheck.py <dir>          # v3.0（默认）
    python scripts/lerobot_v3_selfcheck.py --v21 <dir>    # v2.1

v3.0 检查 on-disk v3.0 contract：required meta files, the info.json
schema keys, the data/video path templates and chunk/file naming, the
episodes metadata columns and contiguity, and (when pyarrow is
installed) the parquet tables' columns/row counts.

v3.0 相机/深度对账（P2-W1-D）：image/video 特性须满足 lerobot 相机契约
（三维 shape + `names == [height, width, channels]`——lerobot
`dataset_to_policy_features` 读 names[2]）；深度图（image, c=1）须带
`info.is_depth_map=true` + `depth_unit`；`video_path` 与 video 特性一致；
video 轨与 raw 树 image 替身轨每集四列 `videos/<key>/…` 指针、帧数 ==
length、同文件首尾相接、帧目录帧数 == Σ 指针帧数、PNG IHDR 与 shape
一致（深度 = 16-bit 灰度）；转换后 image 特性须嵌在 data parquet（Image
struct 全非空 PNG）、无失效指针、stats 为 lerobot 图像形状 (1,1,1)；
videos/ 下无幻影轨目录 / 空帧目录（深度-only 相机与 RGB-only 对称）。

v2.1（`--v21`，PLAN-embodied-v3 §二 #14 QA-5）检查引擎直出/转换后的
v2.1 目录：info.json 键集与模板、meta/tasks|episodes|stats|modality、
per-episode 数据文件列集与行数、videos 布局（帧目录 ↔ mp4 ↔ depth
PNG 序列）、以及 **MANIFEST 哈希对账**（splitmix64 byte-fold 的 Python
对拍实现；转换后目录按 `MANIFEST.converted.json` 标记改对 raw_jsonl/
归档与在位文件核验——DE-P1 提交点语义）。

HONESTY NOTE: this is a *structure* audit. 它不（也不声称）证明
`LeRobotDataset(repo_id)` 能加载这个目录——加载那一步是
`scripts/lerobot_load_smoke.py`（P2-W1-D：默认 emulate 后端零依赖复刻
lerobot v3 读路径，PR 门禁 a 层；`--backend lerobot` 真加载，CI 夜间/手动
b 层，钉版 lerobot）。

Usage:
    python scripts/lerobot_v3_selfcheck.py out/embodied-sim/lerobot-v30
    python scripts/lerobot_v3_selfcheck.py --v21 out/embodied-sim/lerobot-v21
    （转换成 parquet 之前后都可跑：JSONL 代理文件按同等语义检查）

Exit code 0 = all checks passed; 1 = at least one failed.
"""

from __future__ import annotations

import argparse
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

# lerobot 相机特性契约（P2-W1-D）：`dataset_to_policy_features` 对 image/
# video 特性要求三维 shape 并读 `names[2]`（null 直接 TypeError）；深度图
# 以 `info.is_depth_map` 标记、`info.depth_unit` 记单位（lerobot ≥ 0.6）。
CAMERA_NAMES = ["height", "width", "channels"]
POINTER_SUFFIXES = ("chunk_index", "file_index", "from_timestamp", "to_timestamp")
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def _png_ihdr(data: bytes) -> tuple[int, int, int, int] | None:
    """(width, height, bit_depth, colour_type) of a PNG, None if not one."""
    if len(data) < 26 or data[:8] != PNG_MAGIC or data[12:16] != b"IHDR":
        return None
    return (int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big"),
            data[24], data[25])


def _png_matches(data: bytes, feat: dict) -> bool:
    """PNG frame agrees with an image feature's declared shape [h, w, c]:
    c == 1 ⇒ 16-bit grayscale (the depth.{name} contract)."""
    ihdr = _png_ihdr(data)
    shape = feat.get("shape") or []
    if ihdr is None or len(shape) != 3:
        return False
    w, h, bit_depth, colour = ihdr
    if (h, w) != (shape[0], shape[1]):
        return False
    return (bit_depth, colour) == (16, 0) if shape[2] == 1 else colour in (2, 6)


def _pointer_frames(rows: list[dict], key: str, fps: float) -> list[tuple[int, int, int, int]] | None:
    """Per-episode `(chunk, file, k0, k1)` frame ranges of one track's
    `videos/<key>/…` pointer columns; None when any row lacks them."""
    out = []
    for r in rows:
        if any(f"videos/{key}/{s}" not in r for s in POINTER_SUFFIXES):
            return None
        out.append((r[f"videos/{key}/chunk_index"], r[f"videos/{key}/file_index"],
                    round(r[f"videos/{key}/from_timestamp"] * fps),
                    round(r[f"videos/{key}/to_timestamp"] * fps)))
    return out


def _pointers_contiguous(ranges: list[tuple[int, int, int, int]]) -> bool:
    """Within one chunk/file the episodes' frame ranges tile from 0 with no gap/overlap."""
    cursor: dict[tuple[int, int], int] = {}
    for ci, fi, k0, k1 in ranges:
        if k0 != cursor.get((ci, fi), 0) or k1 < k0:
            return False
        cursor[(ci, fi)] = k1
    return True


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
            "注意：这是结构自检；能否被 lerobot 加载见 scripts/lerobot_load_smoke.py。"
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
    video_feats = [k for k, v in feats.items() if v.get("dtype") == "video"]
    image_feats = [k for k, v in feats.items() if v.get("dtype") == "image"]
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
        if feat.get("dtype") in ("video", "image"):
            # P2-W1-D：lerobot dataset_to_policy_features 契约（names null 即崩）
            c.expect(len(shape) == 3 and shape[2] in (1, 3) and feat.get("names") == CAMERA_NAMES,
                     f"features[{name}] 为 lerobot 相机特性：shape [h,w,c∈{{1,3}}] + "
                     f"names == {CAMERA_NAMES}（当前 {shape} / {feat.get('names')!r}）")
        if feat.get("dtype") == "image" and len(shape) == 3 and shape[2] == 1:
            fi = feat.get("info") or {}
            c.expect(fi.get("is_depth_map") is True and fi.get("depth_unit") in ("mm", "m"),
                     f"features[{name}] 深度标记 info.is_depth_map=true + depth_unit∈{{mm,m}}"
                     f"（当前 {fi.get('is_depth_map')!r} / {fi.get('depth_unit')!r}）")
    c.expect((video_path is not None) == bool(video_feats),
             f"video_path 与 video 特性一致（有 video 特性 ⇔ 模板非空；video 特性 {len(video_feats)} 个）")

    total_eps = info.get("total_episodes", -1)
    total_frames = info.get("total_frames", -1)

    # data files 先读（image 特性的口径——raw 替身帧目录 or 已嵌入 parquet——
    # 由 data 文件里有没有该列决定，episodes/videos/stats 各项都按它分家）
    data_files = sorted(root.glob("data/chunk-*/file-*"))
    data_read: list[tuple[Path, list[dict] | None, str]] = [(f, *_read_rows(f)) for f in data_files]
    embedded: set[str] = set()
    for _f, got, _fmt in data_read:
        if got:
            embedded |= {k for k in image_feats if k in got[0]}
    raw_images = [k for k in image_feats if k not in embedded]

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
        # P2-W1-D 指针对账：video 轨与 raw 树的 image（深度）替身轨同一形状——
        # 每集四列 videos/<key>/{chunk,file,from_ts,to_ts}，帧数 == length，
        # 同一 chunk/file 内首尾相接；已嵌入 parquet 的 image 轨不得残留指针。
        fps_f = float(fps) if isinstance(fps, (int, float)) and fps > 0 else 0.0
        for key in video_feats + raw_images:
            kind = "video" if key in video_feats else "image 替身帧目录"
            ranges = _pointer_frames(rows, key, fps_f) if fps_f else None
            c.expect(ranges is not None,
                     f"episodes 每行带 videos/{key}/{{chunk_index,file_index,from_timestamp,to_timestamp}}（{kind}）")
            if ranges is not None:
                spans = [k1 - k0 for (_ci, _fi, k0, k1) in ranges]
                c.expect(spans == [r.get("length") for r in rows] and _pointers_contiguous(ranges),
                         f"videos/{key} 指针帧数 == 每集 length 且同文件首尾相接")
        for key in sorted(embedded):
            stale = sorted({col for r in rows for col in r if col.startswith(f"videos/{key}/")})
            c.expect(not stale, f"{key} 已嵌入 data parquet：无失效替身指针列（残留 {stale or '无'}）")

    # data files
    c.expect(bool(data_files), "data/chunk-XXX/file-XXX 存在")
    total_rows = 0
    numeric = {k for k in feats if k not in ("timestamp", "frame_index", "episode_index", "index", "task_index")
               and feats[k].get("dtype") not in ("video", "image")}
    for f, got, fmt in data_read:
        rel = f.relative_to(root).as_posix()
        c.expect(bool(CHUNK_FILE_RE.match(f.parent.name + "/" + f.stem)),
                 f"{rel} 命名符合 chunk-XXX/file-XXX")
        if got is None:
            c.expect(False, f"{rel}: {fmt}")
            continue
        total_rows += len(got)
        # image 特性：parquet（lerobot 直读的口径）里必须是列；raw JSONL
        # 行只装数值，image 由替身帧目录承载（下方 videos 段对账）
        need = set(DATA_SCALAR_COLS) | numeric | (set(image_feats) if fmt == "parquet" else set())
        missing = need - set(got[0].keys()) if got else need
        c.expect(not missing, f"{rel} 数据列齐全（{fmt}，缺 {sorted(missing) if missing else '无'}）")
        if got:
            c.expect(got[0].get("timestamp") == 0.0,
                     f"{rel} 第一行 timestamp == 0（episode 内秒数）")
        for key in sorted(k for k in image_feats if got and k in got[0]):
            vals = [r.get(key) for r in got]
            blobs = [v.get("bytes") if isinstance(v, dict) else None for v in vals]
            c.expect(all(isinstance(b, (bytes, bytearray)) and b[:8] == PNG_MAGIC for b in blobs),
                     f"{rel} 的 {key} 列为 Image struct：{len(vals)} 行全为内嵌 PNG")
            c.expect(bool(blobs) and all(isinstance(b, (bytes, bytearray)) and _png_matches(b, feats[key])
                                         for b in (blobs[0], blobs[-1])),
                     f"{rel} 的 {key} 首末帧 PNG 与 shape {feats[key].get('shape')} 一致"
                     + ("（16-bit 灰度）" if (feats[key].get("shape") or [0, 0, 0])[-1] == 1 else ""))
    c.expect(total_rows == total_frames, f"数据行数 == total_frames（{total_rows}/{total_frames}）")

    # video files / frame dirs
    if video_path is not None:
        frame_dirs = sorted(root.glob("videos/*/chunk-*/file-*"))
        mp4s = sorted(root.glob("videos/*/chunk-*/file-*.mp4"))
        c.expect(bool(frame_dirs or mp4s), "videos/<key>/chunk-XXX/file-XXX 存在（帧目录或 mp4）")
        # lerobot `_check_cached_episodes_sufficient`：每集每个 video 键的
        # 指针文件都要在（转换后 = mp4；raw = 帧目录）
        for key in video_feats:
            ranges = _pointer_frames(rows, key, float(fps)) if rows and isinstance(fps, (int, float)) and fps > 0 else None
            refs = sorted({(ci, fi) for ci, fi, _k0, _k1 in ranges}) if ranges else []
            absent = [f"chunk-{ci:03d}/file-{fi:03d}" for ci, fi in refs
                      if not (root / "videos" / key / f"chunk-{ci:03d}" / f"file-{fi:03d}.mp4").is_file()
                      and not any((root / "videos" / key / f"chunk-{ci:03d}" / f"file-{fi:03d}").glob("frame_*.png"))]
            c.expect(bool(refs) and not absent,
                     f"videos/{key} 指针引用的 chunk/file 都有 mp4 或帧目录（"
                     + (f"缺 {absent or '无'}" if refs else "无指针可核") + "）")
    # raw image 替身帧目录：帧数 == Σ 指针帧数，首末帧 PNG 与 shape 一致
    for key in raw_images:
        ranges = _pointer_frames(rows, key, float(fps)) if rows and isinstance(fps, (int, float)) and fps > 0 else None
        files = sorted({(ci, fi) for ci, fi, _k0, _k1 in ranges}) if ranges else [(0, 0)]
        n_frames = 0
        pngs: list[Path] = []
        for ci, fi in files:
            d = root / "videos" / key / f"chunk-{ci:03d}" / f"file-{fi:03d}"
            got_pngs = sorted(d.glob("frame_*.png")) if d.is_dir() else []
            n_frames += len(got_pngs)
            pngs += got_pngs
        want = sum(k1 - k0 for _ci, _fi, k0, k1 in ranges) if ranges else total_frames
        c.expect(n_frames == want and n_frames > 0,
                 f"videos/{key} 替身帧目录帧数 == Σ 指针帧数（{n_frames}/{want}）")
        if pngs:
            c.expect(all(_png_matches(p.read_bytes(), feats[key]) for p in (pngs[0], pngs[-1])),
                     f"videos/{key} 首末帧 PNG 与 shape {feats[key].get('shape')} 一致（16-bit 灰度）")
    # 无幻影轨（件2 深度-only 对称）：videos/ 下每个轨目录都对应已声明的
    # video 或 raw image 特性，且没有空帧目录
    vroot = root / "videos"
    if vroot.is_dir():
        declared = set(video_feats) | set(raw_images)
        undeclared = sorted(d.name for d in vroot.iterdir() if d.is_dir() and d.name not in declared)
        empty = sorted(d.relative_to(root).as_posix() for d in vroot.glob("*/chunk-*/file-*")
                       if d.is_dir() and not any(d.glob("frame_*.png")))
        c.expect(not undeclared and not empty,
                 f"videos/ 无幻影轨：轨目录都对应已声明特性、无空帧目录（未声明 {undeclared or '无'}，空 {empty or '无'}）")

    # stats.json（真实数据集惯例：只含观测/动作/时间戳，不含簿记整型列）
    # image 特性：已嵌入 parquet 时必须有 lerobot 形状 (C,1,1) 的 stats（转换器
    # 补算）；raw 树引擎不解码 PNG，不要求。
    stats = root / "meta" / "stats.json"
    if stats.exists():
        st = json.loads(stats.read_text(encoding="utf-8"))
        for name, feat in feats.items():
            if feat.get("dtype") in ("video", "int64") or name in raw_images:
                continue
            c.expect(name in st, f"stats.json 有 {name}")
            if feat.get("dtype") == "image" and name in st:
                s = st[name]
                c.expect(all(isinstance(s.get(k), list) and len(s[k]) == 1
                             and isinstance(s[k][0], list) and len(s[k][0]) == 1
                             and isinstance(s[k][0][0], list) and len(s[k][0][0]) == 1
                             for k in ("min", "max", "mean", "std"))
                         and s.get("count") == [s.get("count", [0])[0]],
                         f"stats.json 的 {name} 为 lerobot 图像 stats 形状 (1,1,1) + count")
    else:
        c.expect(False, "meta/stats.json 存在")

    return c.report()


# ----- v2.1 mode（`--v21`，PLAN-embodied-v3 §二 #14 / QA-5 + DE-P1）-----

V21_REQUIRED_INFO = [
    "codebase_version",
    "robot_type",
    "total_episodes",
    "total_frames",
    "total_tasks",
    "total_videos",
    "total_chunks",
    "chunks_size",
    "fps",
    "splits",
    "data_path",
    "video_path",
    "features",
]

V21_DATA_TEMPLATE = "data/chunk-{episode_chunk:03d}/episode_{episode_index:06d}.parquet"
V21_VIDEO_TEMPLATE = "videos/chunk-{episode_chunk:03d}/{video_key}/episode_{episode_index:06d}.mp4"

# 五个簿记列 + 特性列（v2.1 行契约，见 lerobot.rs row_jsonl）
V21_ROW_BOOKKEEPING = ["index", "episode_index", "frame_index", "task_index", "timestamp"]

FRAME_PNG_RE = re.compile(r"^videos/.+/frame_\d{6}\.png$")

_MASK64 = (1 << 64) - 1


def _mix64(h: int, x: int) -> int:
    """lerobot.rs `mix64` 的 Python 对拍实现。"""
    h ^= x
    h = (h * 0xFF51_AFD7_ED55_8CCD) & _MASK64
    h ^= h >> 29
    h = (h * 0xC4CE_B9FE_1A85_EC53) & _MASK64
    h ^= h >> 32
    return h


def content_hash(data: bytes) -> int:
    """lerobot.rs `content_hash`（splitmix64 byte-fold）——MANIFEST 哈希
    用它在本端重算，不信任清单自述。"""
    h = 0x0000_6F72_6965_7261
    for i in range(0, len(data), 8):
        chunk = data[i:i + 8]
        buf = int.from_bytes(chunk.ljust(8, b"\x00"), "little")
        h = _mix64(h, buf)
    return _mix64(h, len(data))


def _read_json(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def _read_jsonl(path: Path) -> list[dict] | None:
    try:
        return [
            json.loads(line)
            for line in path.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
    except (OSError, json.JSONDecodeError):
        return None


def check_v21(root: Path) -> int:
    """v2.1 目录契约自检（引擎直出 raw 树与转换后树两种口径）。"""
    c = Check()
    info_path = root / "meta" / "info.json"
    if not info_path.exists():
        c.expect(False, f"{info_path} 存在")
        c.report()
        return 1
    info = json.loads(info_path.read_text(encoding="utf-8"))

    c.expect(info.get("codebase_version") == "v2.1",
             f"codebase_version == v2.1（当前 {info.get('codebase_version')!r}）")
    for key in V21_REQUIRED_INFO:
        c.expect(key in info, f"info.json 有 {key}")
    c.expect(info.get("data_path") == V21_DATA_TEMPLATE,
             f"data_path 模板 == {V21_DATA_TEMPLATE}（当前 {info.get('data_path')!r}）")
    c.expect(info.get("video_path") == V21_VIDEO_TEMPLATE,
             f"video_path 模板 == {V21_VIDEO_TEMPLATE}（当前 {info.get('video_path')!r}）")
    fps = info.get("fps")
    c.expect(isinstance(fps, (int, float)) and fps > 0, f"fps 为正数（当前 {fps!r}）")
    c.expect(bool(info.get("robot_type")), "robot_type 非空")
    total_eps = info.get("total_episodes", -1)
    total_frames = info.get("total_frames", -1)
    splits = info.get("splits") or {}
    c.expect(splits.get("train") == f"0:{total_eps}",
             f"splits.train == \"0:{total_eps}\"（当前 {splits.get('train')!r}）")

    feats = info.get("features", {})
    video_feats = [k for k, v in feats.items() if v.get("dtype") == "video"]
    image_feats = [k for k, v in feats.items() if v.get("dtype") == "image"]
    table_feats = {k: v for k, v in feats.items() if v.get("dtype") not in ("video", "image")}
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
        if feat.get("dtype") == "video":
            c.expect(isinstance(feat.get("info", {}).get("video.fps"), (int, float)),
                     f"features[{name}].info['video.fps'] 存在")

    # meta/tasks.jsonl：task_index 从 0 连续
    tasks = _read_jsonl(root / "meta" / "tasks.jsonl")
    c.expect(bool(tasks), "meta/tasks.jsonl 存在且可解析")
    if tasks:
        idxs = [t.get("task_index") for t in tasks]
        c.expect(idxs == list(range(len(tasks))), f"task_index 从 0 连续（{idxs}）")
        c.expect(all(isinstance(t.get("task"), str) and t["task"] for t in tasks),
                 "每条 task 非空字符串")
        c.expect(len(tasks) == info.get("total_tasks", -1),
                 f"tasks 行数 == total_tasks（{len(tasks)}/{info.get('total_tasks')}）")

    # meta/episodes.jsonl：episode_index 连续、Σ length == total_frames
    eps = _read_jsonl(root / "meta" / "episodes.jsonl")
    c.expect(bool(eps), "meta/episodes.jsonl 存在且可解析")
    if eps:
        c.expect([e.get("episode_index") for e in eps] == list(range(len(eps))),
                 "episode_index 从 0 连续")
        c.expect(all(isinstance(e.get("tasks"), list) and e["tasks"] for e in eps),
                 "每集带非空 tasks 列表")
        c.expect(len(eps) == total_eps, f"episodes 行数 == total_episodes（{len(eps)}/{total_eps}）")
        c.expect(sum(e.get("length", 0) for e in eps) == total_frames,
                 f"Σ length == total_frames（{sum(e.get('length', 0) for e in eps)}/{total_frames}）")

    # meta/stats.json + meta/modality.json
    stats = _read_json(root / "meta" / "stats.json")
    c.expect(isinstance(stats, dict), "meta/stats.json 可解析")
    if isinstance(stats, dict):
        for key in ("observation.state", "action", "timestamp"):
            c.expect(key in stats, f"stats.json 有 {key}")
    modality = _read_json(root / "meta" / "modality.json")
    c.expect(isinstance(modality, dict) and isinstance(modality.get("state"), dict)
             and isinstance(modality.get("action"), dict),
             "meta/modality.json 有 state/action 分组")

    # 数据文件：episode_*.jsonl（raw）或 episode_*.parquet（转换后）
    data_jsonl = sorted((root / "data" / "chunk-000").glob("episode_*.jsonl"))
    data_pq = sorted((root / "data" / "chunk-000").glob("episode_*.parquet"))
    data_files = data_pq if data_pq else data_jsonl
    c.expect(bool(data_files), "data/chunk-000/episode_* 存在（jsonl 或转换后 parquet）")
    c.expect(len(data_files) == total_eps,
             f"数据文件数 == total_episodes（{len(data_files)}/{total_eps}）")
    need_cols = set(V21_ROW_BOOKKEEPING) | set(table_feats)
    total_rows = 0
    for f in data_files:
        got, fmt = _read_rows(f)
        if got is None:
            c.expect(False, f"{f.name}: {fmt}")
            continue
        total_rows += len(got)
        missing = need_cols - set(got[0].keys()) if got else need_cols
        c.expect(not missing, f"{f.name} 数据列齐全（{fmt}，缺 {sorted(missing) if missing else '无'}）")
        if got:
            stem_idx = int(f.stem.split("_")[1])
            c.expect(got[0].get("episode_index") == stem_idx,
                     f"{f.name} 行 episode_index == {stem_idx}")
            c.expect(got[0].get("timestamp") == 0.0, f"{f.name} 第一行 timestamp == 0")
    c.expect(total_rows == total_frames,
             f"数据行数 == total_frames（{total_rows}/{total_frames}）")

    # videos 布局：video 特性 = 帧目录（raw）或 mp4（转换后），episode
    # artifact 总数必须对上 total_videos；image（depth.*）特性 = u16 PNG
    # 帧目录（转换器不把深度转 mp4）。
    videos_root = root / "videos" / "chunk-000"
    art_total = 0
    for key in video_feats:
        # v2.1 布局：目录名 = 完整特性键（videos/chunk-000/observation.images.<name>/）
        cam_dir = videos_root / key
        if not cam_dir.is_dir():
            c.expect(info.get("total_videos", 0) == 0,
                     f"{key} 有 episode 帧目录或 mp4")
            continue
        ep_dirs = {d.name for d in cam_dir.glob("episode_*") if d.is_dir()}
        ep_mp4s = {p.stem for p in cam_dir.glob("episode_*.mp4")}
        c.expect(not (ep_dirs & ep_mp4s), f"{key} 的同一 episode 不应同时有帧目录与 mp4")
        for d in sorted(ep_dirs):
            c.expect(any((cam_dir / d).glob("frame_*.png")),
                      f"{key}/{d} 帧目录含 frame_*.png")
        art_total += len(ep_dirs) + len(ep_mp4s)
    c.expect(art_total == info.get("total_videos", -1),
             f"episode 视频 artifact 总数 == total_videos（{art_total}/{info.get('total_videos')}）")
    for key in image_feats:
        d = videos_root / key
        c.expect(d.is_dir(), f"{key} 帧目录存在（u16 PNG 序列）")
        if d.is_dir():
            subs = [x for x in d.glob("episode_*") if x.is_dir()]
            c.expect(bool(subs) and all(any(x.glob("frame_*.png")) for x in subs),
                     f"{key} 至少一个 episode 帧目录且含 frame_*.png")
    # 无幻影轨（P2-W1-D 深度-only 对称）：videos/chunk-000/ 下每个轨目录
    # 都对应已声明的 video/image 特性（深度-only 相机不再声明没有帧的
    # observation.images.<name>，RGB-only 相机本就不声明 depth.<name>）
    if videos_root.is_dir():
        declared = set(video_feats) | set(image_feats)
        undeclared = sorted(d.name for d in videos_root.iterdir() if d.is_dir() and d.name not in declared)
        c.expect(not undeclared, f"videos/chunk-000/ 无幻影轨：轨目录都对应已声明特性（未声明 {undeclared or '无'}）")

    # MANIFEST 提交点对账（DE-P1）：
    # - raw 树：逐条在位重算哈希；
    # - 转换后树（有 MANIFEST.converted.json）：行文件条目改对 raw_jsonl/
    #   归档核验；帧 PNG 条目被 mp4 取代的记为历史回执（不硬失败）。
    converted_marker = root / "MANIFEST.converted.json"
    converted = _read_json(converted_marker) if converted_marker.exists() else None
    manifest_path = root / "MANIFEST.jsonl"
    c.expect(manifest_path.exists(), "MANIFEST.jsonl 存在（引擎原子提交点）")
    entries = _read_jsonl(manifest_path) if manifest_path.exists() else None
    c.expect(entries is not None, "MANIFEST.jsonl 逐行 JSON 可解析")
    if converted_marker.exists():
        c.expect(isinstance(converted, dict) and converted.get("converted") is True,
                 "MANIFEST.converted.json 标记 converted=true（转换后提交点，DE-P1）")
        c.expect(bool(isinstance(converted, dict) and converted.get("converted_at")),
                 "MANIFEST.converted.json 带 converted_at")
        ce = (converted or {}).get("entries")
        c.expect(ce == entries,
                 "MANIFEST.converted.json 忠实复制原清单条目")
    if entries is not None:
        raw_dir = root / "raw_jsonl"
        if converted is not None:
            c.expect(raw_dir.is_dir(), "raw_jsonl/ 归档目录存在（已转换口径）")
        mism: list[str] = []
        missing: list[str] = []
        superseded = 0
        for e in entries:
            rel, want = e.get("file", ""), e.get("hash")
            # 清单里 hash 是 "016x" 十六进制字符串——对账前转回整数
            try:
                want_i = int(want, 16) if isinstance(want, str) else want
            except (TypeError, ValueError):
                mism.append(f"{rel}(hash 非十六进制: {want!r})")
                continue
            p = root / rel
            if p.is_file():
                if content_hash(p.read_bytes()) != want_i:
                    mism.append(rel)
            elif converted is not None and (raw_dir / Path(rel).name).is_file():
                arch = raw_dir / Path(rel).name
                if content_hash(arch.read_bytes()) != want_i:
                    mism.append(f"{rel} → raw_jsonl/{arch.name}")
            elif converted is not None and FRAME_PNG_RE.match(rel):
                superseded += 1  # 帧目录已被 mp4 取代（转换器 rmtree）——历史回执
            else:
                missing.append(rel)
        c.expect(not mism, f"MANIFEST 哈希对账（错 {mism or '无'}）")
        c.expect(not missing, f"MANIFEST 清单文件齐全（缺 {missing or '无'}）")
        if superseded:
            c.expect(True, f"{superseded} 条帧 PNG 条目已被 mp4 取代（条目退行为历史回执）")

    return c.report()


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("root", type=Path, help="LeRobot 数据集目录（v3.0 默认 / --v21 按 v2.1）")
    ap.add_argument("--v21", action="store_true",
                    help="按 v2.1 目录契约自检（含 MANIFEST 哈希对账）")
    args = ap.parse_args()
    sys.exit(check_v21(args.root) if args.v21 else check(args.root))
