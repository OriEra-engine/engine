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

深度轨（v3.0，P2-W1-D）：`depth.{name}` 是 lerobot 的 **image** 特性
（`info.is_depth_map=true`、`depth_unit="mm"`）——lerobot 把 image 特性
存在 **data parquet 里**（`datasets.Image()` = struct<bytes, path>），
lerobot ≥ 0.7 把 16-bit PNG 解成 float32 毫米。所以 v3 转换把引擎的
u16 PNG 帧目录（`videos/depth.{name}/chunk-000/file-000/`，按 episodes
元数据的 `videos/depth.{name}/…` 指针定位）**原字节嵌进 data parquet**，
补算深度 stats（逐通道 (1,1,1)，毫米原值，不除 255），再删除替身帧目录
与指针列。深度从不进 mp4（旧版本会把 u16 深度有损压成 8-bit H.264 并删掉
PNG——不可逆，已修）。v2.1 树的深度轨保持 PNG 帧目录不变。

Usage:
    python scripts/jsonl_to_lerobot_v21.py out/embodied-sim/lerobot-v21

Requires: python -m pip install pyarrow （相机轨另需 imageio imageio-ffmpeg；
v3 深度轨的 stats 需 numpy + Pillow——imageio 自带的两个依赖）
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


def write_converted_manifest(root: Path, rewritten: list[str] | None = None) -> Path:
    """DE-P1：转换收尾落 `MANIFEST.converted.json`（raw_jsonl/ 同级）。

    内容 = 原 `MANIFEST.jsonl` 全部条目 + `converted`/`converted_at`
    标记。语义（README 同款说明）：转换后原 MANIFEST 指向的行文件已
    归档进 raw_jsonl/，其哈希仍可对归档逐条对账；`MANIFEST.converted.
    json` 是转换后目录的提交点。幂等：重跑只刷新 converted_at。
    `rewritten`（P2-W1-D）：转换器原地改写过的引擎文件（v3 深度轨补
    stats 的 meta/stats.json）——其原清单哈希不再对得上在位文件，如实
    列出；为空时不写该键（v2.1 转换产物形状不变）。
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
    if rewritten:
        out["rewritten"] = list(rewritten)
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


# ----- v3.0 image (depth) features: embed into the data parquet (P2-W1-D) -----

# datasets.Image() 的 arrow 存储类型：lerobot `get_hf_features_from_features`
# 把 dtype "image" 映射成 Image()，按此 struct 从 data parquet 读。
IMAGE_STRUCT = pa.struct([("bytes", pa.binary()), ("path", pa.string())])
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def _read_jsonl_rows(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def write_tasks_parquet(tasks: list[dict], dst: Path) -> None:
    """v3 `meta/tasks.parquet`: a pandas frame indexed by the task string
    with one `task_index` column — lerobot `load_tasks` = `pd.read_parquet`,
    and `task_name(tasks, i)` returns `tasks.iloc[i].name` (the index).

    Without pandas the index metadata is written by hand (P2-W1-D): the
    old fallback stored `task` as a plain column, so lerobot read a
    RangeIndex and every frame's `task` came back as "0"/"1"/… — a silent
    wrong prompt for language-conditioned policies."""
    tasks = sorted(tasks, key=lambda t: t["task_index"])  # iloc[i] ⇔ task_index i
    try:
        import pandas as pd
    except ImportError:
        pd = None
    if pd is not None:
        df = pd.DataFrame(
            {"task_index": [t["task_index"] for t in tasks]},
            index=pd.Index([t["task"] for t in tasks], name="task"),
        )
        df.to_parquet(dst)
        return
    tbl = pa.table({
        "task_index": pa.array([t["task_index"] for t in tasks], pa.int64()),
        "task": pa.array([t["task"] for t in tasks], pa.string()),
    })
    pandas_meta = {
        "index_columns": ["task"],
        "column_indexes": [{"name": None, "field_name": None, "pandas_type": "unicode",
                            "numpy_type": "object", "metadata": {"encoding": "UTF-8"}}],
        "columns": [
            {"name": "task_index", "field_name": "task_index", "pandas_type": "int64",
             "numpy_type": "int64", "metadata": None},
            {"name": "task", "field_name": "task", "pandas_type": "unicode",
             "numpy_type": "object", "metadata": None},
        ],
        "creator": {"library": "pyarrow", "version": pa.__version__},
        "pandas_version": "2.0.0",
    }
    tbl = tbl.replace_schema_metadata({b"pandas": json.dumps(pandas_meta).encode("utf-8")})
    pq.write_table(tbl, dst)


def _image_frame_locator(root: Path, key: str, episodes: list[dict], fps: float):
    """`(episode_index, frame_index) -> frame PNG path` for one v3 image
    feature's raw stand-in frame dir.

    Pointer columns `videos/<key>/{chunk_index,file_index,from_timestamp}`
    (P2-W1-D) locate each episode's first frame (k0 = round(from * fps)).
    Legacy raw trees exported before the pointers existed fall back to
    `dataset_from_index` in chunk-000/file-000 — valid only when the dir
    holds exactly one frame per row, which is checked (loud failure, not a
    silent misalignment)."""
    base = {}
    has_ptr = all(f"videos/{key}/from_timestamp" in ep for ep in episodes)
    for ep in episodes:
        i = ep["episode_index"]
        if has_ptr:
            ci = ep.get(f"videos/{key}/chunk_index", 0)
            fi = ep.get(f"videos/{key}/file_index", 0)
            k0 = round(ep[f"videos/{key}/from_timestamp"] * fps)
            k1 = round(ep[f"videos/{key}/to_timestamp"] * fps)
        else:
            ci, fi = 0, 0
            k0, k1 = ep["dataset_from_index"], ep["dataset_to_index"]
        base[i] = (root / "videos" / key / f"chunk-{ci:03d}" / f"file-{fi:03d}", k0, k1)
    if not has_ptr:
        d = root / "videos" / key / "chunk-000" / "file-000"
        n = len(list(d.glob("frame_*.png")))
        want = sum(ep["length"] for ep in episodes)
        if n != want:
            sys.exit(
                f"错误：{key} 无 episodes 指针列（旧版引擎导出）且帧目录帧数 {n} ≠ 总行数 {want}——"
                "无法按行对齐深度帧，请用新版引擎重导出"
            )

    def locate(ep_index: int, frame_index: int) -> Path:
        d, k0, k1 = base[ep_index]
        k = k0 + frame_index
        if k >= k1:
            sys.exit(f"错误：{key} 第 {ep_index} 集第 {frame_index} 帧超出指针区间 [{k0}, {k1})——深度帧缺失")
        return d / f"frame_{k:06d}.png"

    return locate


def _depth_png_stats(png: bytes):
    """(min, max, sum, sum_sq, n_px) of one 16-bit grayscale PNG frame
    (millimetres; 0 = no return counts as a value, lerobot computes image
    stats over all pixels)."""
    import io

    try:
        import numpy as np
        from PIL import Image
    except ImportError:
        sys.exit("错误：v3 深度轨 stats 需要 numpy + Pillow——pip install numpy pillow（imageio 自带）后重跑")
    with Image.open(io.BytesIO(png)) as img:
        arr = np.asarray(img).astype(np.float64)
    return float(arr.min()), float(arr.max()), float(arr.sum()), float((arr * arr).sum()), int(arr.size)


class _DepthAcc:
    """Running per-channel depth stats → lerobot image-stats shape (1,1,1)."""

    def __init__(self) -> None:
        self.lo = float("inf")
        self.hi = float("-inf")
        self.s = 0.0
        self.sq = 0.0
        self.px = 0
        self.frames = 0

    def add(self, st) -> None:
        lo, hi, s, sq, px = st
        self.lo, self.hi = min(self.lo, lo), max(self.hi, hi)
        self.s += s
        self.sq += sq
        self.px += px
        self.frames += 1

    def merge(self, o: "_DepthAcc") -> None:
        if not o.frames:
            return
        self.lo, self.hi = min(self.lo, o.lo), max(self.hi, o.hi)
        self.s += o.s
        self.sq += o.sq
        self.px += o.px
        self.frames += o.frames

    def stats(self) -> dict:
        mean = self.s / self.px if self.px else 0.0
        var = max(self.sq / self.px - mean * mean, 0.0) if self.px else 0.0
        c = lambda v: [[[v]]]  # noqa: E731 — (C,1,1) with C = 1
        return {"min": c(self.lo), "max": c(self.hi), "mean": c(mean), "std": c(var ** 0.5),
                "count": [self.frames]}


def embed_v30_images(root: Path, rows: list[dict], image_feats: dict, episodes: list[dict],
                     fps: float, accs: dict) -> dict:
    """Image-feature columns for one data file: each row's PNG bytes,
    verbatim (lossless), as `datasets.Image()` structs. Feeds the per-
    episode depth stats accumulators `accs[key][episode_index]`."""
    cols = {}
    for key in image_feats:
        locate = _image_frame_locator(root, key, episodes, fps)
        values = []
        for row in rows:
            p = locate(row["episode_index"], row["frame_index"])
            try:
                data = p.read_bytes()
            except OSError as e:
                sys.exit(f"错误：{key} 帧 {p} 读取失败：{e}")
            if not data.startswith(PNG_MAGIC):
                sys.exit(f"错误：{p} 不是 PNG")
            values.append({"bytes": data, "path": p.name})
            accs.setdefault(key, {}).setdefault(row["episode_index"], _DepthAcc()).add(
                _depth_png_stats(data))
        cols[key] = pa.array(values, IMAGE_STRUCT)
    return cols


def convert_v30(root: Path, features: dict) -> int:
    """v3.0: consolidated file-*.jsonl → parquet（data/episodes/tasks）.

    Image（depth.*）特性（P2-W1-D）：PNG 原字节嵌进 data parquet 的
    `Image()` 列（lerobot 的 image 存法），episodes 行补 `stats/<key>/*`、
    删 `videos/<key>/*` 替身指针列，stats.json 补 `<key>`，替身帧目录在
    全部嵌入核验后删除。"""
    converted = 0
    # 同 convert_v21_data：image（depth.*）不是 JSONL 行字段——单独嵌入
    table_feats = {
        k: v for k, v in features.items()
        if v.get("dtype") not in ("video", "image")
    }
    image_feats = {k: v for k, v in features.items() if v.get("dtype") == "image"}
    info = json.loads((root / "meta" / "info.json").read_text(encoding="utf-8"))
    fps = float(info.get("fps") or 0)
    ep_srcs = sorted(root.glob("meta/episodes/chunk-*/file-*.jsonl"))
    episodes = [r for src in ep_srcs for r in _read_jsonl_rows(src)]
    accs: dict = {}
    raw = root / "raw_jsonl"
    for src in sorted(root.glob("data/chunk-*/file-*.jsonl")):
        rows = _read_jsonl_rows(src)
        table = convert_rows(rows, table_feats)
        if image_feats:
            if not episodes or fps <= 0:
                sys.exit("错误：v3 image 特性需要 meta/episodes 元数据与正的 fps 才能定位帧")
            for key, col in embed_v30_images(root, rows, image_feats, episodes, fps, accs).items():
                table = table.append_column(key, col)
        dst = src.with_suffix(".parquet")
        pq.write_table(table, dst)
        _archive(src, raw)
        print(f"  {src.relative_to(root)} → {dst.relative_to(root)}  ({table.num_rows} rows"
              + (f"，嵌入 image 列 {sorted(image_feats)}" if image_feats else "") + ")")
        converted += 1
    # meta/tasks.jsonl → meta/tasks.parquet（index=task 字符串，列=task_index）
    tasks_src = root / "meta" / "tasks.jsonl"
    if tasks_src.exists():
        tasks = [json.loads(line) for line in tasks_src.read_text(encoding="utf-8").splitlines() if line.strip()]
        write_tasks_parquet(tasks, root / "meta" / "tasks.parquet")
        _archive(tasks_src, raw)
        print("  meta/tasks.jsonl → meta/tasks.parquet")
    # meta/episodes/chunk-*/file-*.jsonl → parquet（列类型从值推断：
    # stats/* 为 list<double>（count 为 int64），tasks 为 list<string>）
    for src in ep_srcs:
        rows = _read_jsonl_rows(src)
        for row in rows:
            for key in image_feats:
                # raw 替身指针在嵌入后失效（帧已在 data parquet 里）
                for suffix in ("chunk_index", "file_index", "from_timestamp", "to_timestamp"):
                    row.pop(f"videos/{key}/{suffix}", None)
                acc = accs.get(key, {}).get(row["episode_index"])
                if acc is not None:
                    for stat, value in acc.stats().items():
                        row[f"stats/{key}/{stat}"] = value
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
    if image_feats and accs:
        # stats.json：补 image 特性的数据集级 stats（lerobot 形状 (1,1,1)）
        stats_path = root / "meta" / "stats.json"
        stats = json.loads(stats_path.read_text(encoding="utf-8")) if stats_path.exists() else {}
        for key in image_feats:
            total = _DepthAcc()
            for acc in accs.get(key, {}).values():
                total.merge(acc)
            if total.frames:
                stats[key] = total.stats()
        stats_path.write_text(json.dumps(stats, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"  meta/stats.json  补 image 特性 stats {sorted(image_feats)}")
        # 替身帧目录：字节已逐帧嵌入 data parquet（无损），整轨删除
        for key in image_feats:
            d = root / "videos" / key
            if d.is_dir():
                shutil.rmtree(d)
                print(f"  videos/{key}/  已嵌入 data parquet，替身帧目录删除")
        vids = root / "videos"
        if vids.is_dir() and not any(vids.iterdir()):
            vids.rmdir()
    return converted


def convert_videos(root: Path, features: dict, version: str) -> int:
    """Frame PNG dirs → mp4 (v2.1 per-episode; v3.0 one mp4 per file)."""
    mp4_made = 0
    pending = []
    if version == "v3.0":
        # videos/<cam>/chunk-000/file-000/{frame_*.png} → ../file-000.mp4
        # P2-W1-D：只转 **video** 特性的帧目录——image（depth.*）轨绝不
        # 进 mp4（u16 毫米深度过 yuv420p H.264 = 有损 8-bit，曾被转成
        # max≈8 的废数据并删掉 PNG）；它们已由 convert_v30 嵌进 data parquet。
        video_keys = {k for k, v in features.items() if v.get("dtype") == "video"}
        for frame_dir in sorted(root.glob("videos/*/chunk-*/file-*")):
            if frame_dir.parent.parent.name not in video_keys:
                continue
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
    rewritten = (
        ["meta/stats.json"]
        if version == "v3.0" and any(v.get("dtype") == "image" for v in features.values())
        else None
    )
    write_converted_manifest(root, rewritten)
    print(
        f"done: {n_eps} 个数据文件已转换 under {root}"
        + (f"，{mp4_made} 段相机视频" if mp4_made else "")
    )


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    convert(Path(sys.argv[1]))
