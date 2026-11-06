#!/usr/bin/env python3
"""LeRobot loader smoke for an OriEra LeRobotDataset v3.0 export (P2-W1-D).

The question this answers is not "is the tree well-formed" (that is
`lerobot_v3_selfcheck.py`) but "will `LeRobotDataset` actually eat it".
Two backends, one entrypoint, one verdict format:

    python scripts/lerobot_load_smoke.py <converted v3 dir>                 # a 层：emulate（默认）
    python scripts/lerobot_load_smoke.py --backend lerobot <converted v3 dir>  # b 层：真 lerobot

**emulate**（零 lerobot / torch 依赖；pyarrow 必需，numpy/Pillow/imageio
有则加深检查）——逐条复刻 lerobot ≥ 0.4 v3 读路径会踩的契约（对照
lerobot 0.7.0 源码 datasets/{dataset_metadata,dataset_reader,io_utils,
feature_utils}.py、utils/feature_utils.py）：

1. `check_version_compatibility`：codebase_version 主版本 == 3；
2. `DatasetInfo.from_dict`：fps / chunks_size / data|video_files_size_in_mb > 0；
3. `get_hf_features_from_features` → `Dataset.from_parquet(features=…)`：
   data parquet 列集合 == 非 video 特性（HF cast 要求列名一致），类型
   逐列对上（Value / Sequence(length=n) / Image() = struct<bytes,path>）；
4. `load_tasks` = `pd.read_parquet` + `task_name` = `iloc[i].name`：task
   字符串必须是 pandas 索引、行序 == task_index；
5. `load_episodes`：必需列 + 每个 video 键四列指针；`load_stats`：每个
   叶子能 cast 成规则 numpy 数组；
6. `_check_cached_episodes_sufficient`：每集每个 video 键的 mp4 都在
   （否则 lerobot 转去 Hub 下载）；
7. `dataset_to_policy_features`（lerobot-train 入口）：image/video 特性
   三维 shape + `names[2]`（null 即 TypeError）；
8. 迭代 N 帧（`get_items` 口径）：`index` 唯一且 == 行号、episode/frame
   映射、相邻 timestamp 差 == 1/fps（±1e-4，旧版 check_timestamps_sync）、
   video 查询时刻 from_timestamp + timestamp 落在该集区间且 < mp4 帧数
   （有 imageio 时实读帧数）、image 列 PNG 可解且形状 == shape（有 Pillow
   时实解码；深度 = 16-bit 灰度）、task 可取。

**lerobot**（b 层；需 `pip install "lerobot[dataset]==<pin>"`）：真
`LeRobotDataset(repo_id, root=…, video_backend="pyav")` 加载 + 均匀抽 N
帧 `ds[i]` 解码（video → (3,H,W) ∈[0,1]，深度 image → (1,H,W) 毫米）+
`dataset_to_policy_features` + 一个 DataLoader batch。`HF_HUB_OFFLINE=1`
（缺省即设）：本地加载失败就地报错，不去 Hub 兜底。

Exit 0 = 全部通过；1 = 有失败；2 = 环境缺依赖（emulate 缺 pyarrow /
lerobot 后端缺 lerobot）。
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

BOOKKEEPING = ("timestamp", "frame_index", "episode_index", "index", "task_index")
REQUIRED_EPISODE_COLS = ("episode_index", "tasks", "length", "dataset_from_index",
                         "dataset_to_index", "data/chunk_index", "data/file_index")
POINTER_SUFFIXES = ("chunk_index", "file_index", "from_timestamp", "to_timestamp")
TOLERANCE_S = 1e-4  # LeRobotDataset(tolerance_s=1e-4) default
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


class Check:
    def __init__(self) -> None:
        self.ok: list[str] = []
        self.bad: list[str] = []

    def expect(self, cond: bool, msg: str) -> bool:
        (self.ok if cond else self.bad).append(msg)
        return bool(cond)

    def report(self, backend: str) -> int:
        for line in self.ok:
            print(f"  ✓ {line}")
        for line in self.bad:
            print(f"  ✗ {line}")
        print(f"\n[{backend}] {len(self.ok)} 项通过，{len(self.bad)} 项失败。")
        return 1 if self.bad else 0


def sample_indices(total: int, n: int) -> list[int]:
    """N evenly spaced row indices incl. first and last (deterministic)."""
    if total <= 0:
        return []
    n = max(1, min(n, total))
    if n == 1:
        return [0]
    return sorted({round(i * (total - 1) / (n - 1)) for i in range(n)})


# ----------------------------------------------------------------- emulate

def _arrow_matches(pa, got, feat: dict) -> bool:
    """Is parquet field type `got` what lerobot's HF feature for `feat`
    loads as (get_hf_features_from_features)?"""
    dtype, shape = feat.get("dtype"), list(feat.get("shape") or [])
    if dtype == "image":  # datasets.Image() storage: struct<bytes: binary, path: string>
        if not pa.types.is_struct(got):
            return False
        if {got.field(i).name for i in range(got.num_fields)} != {"bytes", "path"}:
            return False
        bt = got.field("bytes").type
        return pa.types.is_binary(bt) or pa.types.is_large_binary(bt)
    try:
        want = pa.type_for_alias(dtype)
    except (ValueError, TypeError):
        return False
    if shape == [1]:
        return got == want
    if len(shape) == 1:
        is_list = pa.types.is_fixed_size_list(got) or pa.types.is_list(got) or pa.types.is_large_list(got)
        if not is_list or got.value_type != want:
            return False
        return not pa.types.is_fixed_size_list(got) or got.list_size == shape[0]
    return False  # 2-D+ numeric features: OriEra exports none


def emulate(root: Path, n_frames: int) -> int:
    try:
        import pyarrow as pa
        import pyarrow.parquet as pq
    except ImportError:
        print("错误：emulate 后端需要 pyarrow——pip install pyarrow")
        return 2
    c = Check()
    info_path = root / "meta" / "info.json"
    if not c.expect(info_path.is_file(), f"{info_path} 存在"):
        return c.report("emulate")
    info = json.loads(info_path.read_text(encoding="utf-8"))

    # 1-2. version gate + DatasetInfo.__post_init__
    ver = str(info.get("codebase_version", ""))
    try:
        major = int(ver.lstrip("v").split(".")[0])
    except ValueError:
        major = -1
    c.expect(major == 3, f"codebase_version 主版本 == 3（check_version_compatibility；当前 {ver!r}）")
    fps = info.get("fps")
    c.expect(all(isinstance(info.get(k), (int, float)) and info.get(k) > 0
                 for k in ("fps", "chunks_size", "data_files_size_in_mb", "video_files_size_in_mb")),
             "DatasetInfo 正数字段：fps / chunks_size / data|video_files_size_in_mb > 0")
    feats: dict = info.get("features") or {}
    video_keys = [k for k, v in feats.items() if v.get("dtype") == "video"]
    image_keys = [k for k, v in feats.items() if v.get("dtype") == "image"]
    depth_keys = [k for k in feats if (feats[k].get("info") or {}).get("is_depth_map")]
    table_feats = {k: v for k, v in feats.items() if v.get("dtype") != "video"}
    c.expect(not video_keys or isinstance(info.get("video_path"), str),
             "有 video 特性时 video_path 是模板字符串")

    # 7. dataset_to_policy_features (lerobot-train / make_policy)
    for k in video_keys + image_keys:
        shape, names = feats[k].get("shape") or [], feats[k].get("names")
        c.expect(len(shape) == 3 and isinstance(names, list) and len(names) == 3
                 and names[2] in ("channel", "channels"),
                 f"dataset_to_policy_features 可过 {k}（三维 shape + names[2] ∈ channel(s)；当前 {shape} / {names!r}）")

    # 3. data parquet ↔ HF features
    data_paths = sorted(root.glob("data/*/*.parquet"))
    if not c.expect(bool(data_paths), "data/*/*.parquet 存在（load_nested_dataset）"):
        return c.report("emulate")
    tables = []
    for p in data_paths:
        rel = p.relative_to(root).as_posix()
        schema = pq.read_schema(p)
        names = set(schema.names)
        c.expect(names == set(table_feats),
                 f"{rel} 列集合 == 非 video 特性（HF cast 要求；多 {sorted(names - set(table_feats)) or '无'}，"
                 f"缺 {sorted(set(table_feats) - names) or '无'}）")
        bad = [k for k in table_feats if k in names and not _arrow_matches(pa, schema.field(k).type, table_feats[k])]
        c.expect(not bad, f"{rel} 列类型 == HF 特性（Value / Sequence(length) / Image struct；不符 {bad or '无'}）")
        tables.append(pq.read_table(p))
    data = pa.concat_tables(tables) if len(tables) > 1 else tables[0]
    n_rows = data.num_rows
    c.expect(n_rows == info.get("total_frames"), f"数据行数 == total_frames（{n_rows}/{info.get('total_frames')}）")

    # 4. tasks.parquet (pd.read_parquet + task_name = iloc[i].name)
    tasks_p = root / "meta" / "tasks.parquet"
    task_strings: list[str] = []
    if c.expect(tasks_p.is_file(), "meta/tasks.parquet 存在（load_tasks）"):
        tschema = pq.read_schema(tasks_p)
        pmeta = json.loads((tschema.metadata or {}).get(b"pandas", b"{}") or b"{}")
        idx_cols = pmeta.get("index_columns") or []
        ttab = pq.read_table(tasks_p)
        if idx_cols and isinstance(idx_cols[0], str) and idx_cols[0] in ttab.column_names \
                and "task_index" in ttab.column_names:
            task_strings = [str(x) for x in ttab.column(idx_cols[0]).to_pylist()]
            order = ttab.column("task_index").to_pylist()
            c.expect(order == list(range(len(order))) and all(task_strings),
                     f"tasks.parquet：task 字符串为 pandas 索引、行序 == task_index（task_name 取 iloc[i].name；{len(order)} 条）")
        else:
            c.expect(False, f"tasks.parquet：task 字符串为 pandas 索引（当前 index_columns={idx_cols!r}——"
                            "lerobot 会把 task 读成 '0'/'1'…）")
        c.expect(len(task_strings) == info.get("total_tasks"),
                 f"tasks 条数 == total_tasks（{len(task_strings)}/{info.get('total_tasks')}）")

    # 5. episodes + stats
    ep_paths = sorted(root.glob("meta/episodes/*/*.parquet"))
    episodes: list[dict] = []
    if c.expect(bool(ep_paths), "meta/episodes/*/*.parquet 存在（load_episodes）"):
        for p in ep_paths:
            t = pq.read_table(p)
            cols = [n for n in t.column_names if not n.startswith("stats/")]
            episodes += t.select(cols).to_pylist()
        need = list(REQUIRED_EPISODE_COLS) + [f"videos/{k}/{s}" for k in video_keys for s in POINTER_SUFFIXES]
        missing = sorted({col for e in episodes for col in need if col not in e})
        c.expect(not missing, f"episodes 列齐全（含每个 video 键四列指针；缺 {missing or '无'}）")
        c.expect([e.get("episode_index") for e in episodes] == list(range(len(episodes)))
                 and len(episodes) == info.get("total_episodes"),
                 f"episode_index 从 0 连续且 == total_episodes（{len(episodes)}/{info.get('total_episodes')}）")
    stats_p = root / "meta" / "stats.json"
    if stats_p.is_file():
        stats = json.loads(stats_p.read_text(encoding="utf-8"))
        ragged = []
        try:
            import numpy as np

            for key, sub in stats.items():
                for stat, v in (sub or {}).items():
                    try:
                        a = np.atleast_1d(np.array(v))
                        if a.dtype == object:
                            ragged.append(f"{key}/{stat}")
                    except ValueError:
                        ragged.append(f"{key}/{stat}")
        except ImportError:
            pass
        c.expect(not ragged, f"stats.json 每个叶子可 cast 成规则 numpy 数组（cast_stats_to_numpy；不规则 {ragged or '无'}）")
        c.expect(all(k in stats for k in depth_keys),
                 f"深度特性有 stats（{depth_keys or '无深度特性'}）")

    # 6. video files per episode × key
    vtemplate = info.get("video_path") or ""
    mp4_frames: dict[str, int | None] = {}
    absent = []
    for k in video_keys:
        for e in episodes:
            try:
                rel = vtemplate.format(video_key=k, chunk_index=int(e[f"videos/{k}/chunk_index"]),
                                       file_index=int(e[f"videos/{k}/file_index"]))
            except (KeyError, ValueError, IndexError):
                absent.append(f"{k}#ep{e.get('episode_index')}")
                continue
            if not (root / rel).is_file():
                absent.append(rel)
            elif rel not in mp4_frames:
                mp4_frames[rel] = _mp4_frame_count(root / rel)
    c.expect(not absent, f"每集每个 video 键的 mp4 都在（_check_cached_episodes_sufficient；缺 {sorted(set(absent)) or '无'}）")

    # 8. iterate N frames (get_items)
    idx_col = data.column("index").to_pylist()
    c.expect(idx_col == list(range(n_rows)), "index 列唯一且 == 行号（absolute→relative 映射）")
    ep_by = {e["episode_index"]: e for e in episodes}
    ts_all = data.column("timestamp").to_pylist()
    ep_all = data.column("episode_index").to_pylist()
    jumps = [i for i in range(1, n_rows)
             if ep_all[i] == ep_all[i - 1] and abs(ts_all[i] - ts_all[i - 1] - 1.0 / fps) > TOLERANCE_S]
    c.expect(not jumps, f"相邻 timestamp 差 == 1/fps ±{TOLERANCE_S}（check_timestamps_sync；违例 {len(jumps)} 处）")
    picks = sample_indices(n_rows, n_frames)
    rows = data.take(picks).to_pylist() if picks else []
    problems: list[str] = []
    decoded = 0
    for i, row in zip(picks, rows):
        e = ep_by.get(row["episode_index"])
        if e is None:
            problems.append(f"#{i} episode {row['episode_index']} 不在 episodes 元数据")
            continue
        if row["frame_index"] != i - e["dataset_from_index"] or not (
                e["dataset_from_index"] <= i < e["dataset_to_index"]):
            problems.append(f"#{i} frame_index/区间不符")
        if abs(row["timestamp"] - row["frame_index"] / fps) > TOLERANCE_S:
            problems.append(f"#{i} timestamp {row['timestamp']} ≠ frame_index/fps")
        if not (0 <= row["task_index"] < max(len(task_strings), 1)):
            problems.append(f"#{i} task_index {row['task_index']} 越界")
        for k in video_keys:
            q = e[f"videos/{k}/from_timestamp"] + row["timestamp"]
            if not (e[f"videos/{k}/from_timestamp"] - TOLERANCE_S <= q < e[f"videos/{k}/to_timestamp"] + TOLERANCE_S):
                problems.append(f"#{i} {k} 查询时刻 {q:.6f} 不在本集视频区间")
            vfps = (feats[k].get("info") or {}).get("video.fps") or fps
            pos = round(q * vfps)
            if abs(q - pos / vfps) > TOLERANCE_S:
                problems.append(f"#{i} {k} 查询时刻偏离帧格 > tolerance_s")
            rel = vtemplate.format(video_key=k, chunk_index=int(e[f"videos/{k}/chunk_index"]),
                                   file_index=int(e[f"videos/{k}/file_index"]))
            nf = mp4_frames.get(rel)
            if nf is not None and pos >= nf:
                problems.append(f"#{i} {k} 帧位 {pos} ≥ mp4 帧数 {nf}")
        for k in image_keys:
            blob = (row.get(k) or {}).get("bytes")
            ok, why = _image_ok(blob, feats[k], k in depth_keys)
            decoded += ok
            if not ok:
                problems.append(f"#{i} {k}: {why}")
    c.expect(not problems, f"迭代 {len(picks)} 帧（get_items 口径：映射/时间戳/视频帧位/图像解码/任务）"
             + (f"——问题 {problems[:4]}" if problems else ""))
    if image_keys:
        c.expect(decoded == len(picks) * len(image_keys),
                 f"image 帧逐个解码通过 {decoded}/{len(picks) * len(image_keys)}（深度 = 16-bit 灰度，形状 == shape）")
    if video_keys:
        counted = sum(v is not None for v in mp4_frames.values())
        print(f"  · mp4 帧数实读 {counted}/{len(mp4_frames)} 个文件"
              + ("" if counted == len(mp4_frames) else "（无 imageio[ffmpeg] 的文件只核存在性）"))
    return c.report("emulate")


def _mp4_frame_count(path: Path) -> int | None:
    try:
        import imageio.v2 as iio
    except ImportError:
        return None
    try:
        r = iio.get_reader(path)
        try:
            return r.count_frames()
        finally:
            r.close()
    except Exception:  # noqa: BLE001 — unreadable mp4 = report 0 frames
        return 0


def _image_ok(blob, feat: dict, depth: bool) -> tuple[bool, str]:
    if not isinstance(blob, (bytes, bytearray)) or blob[:8] != PNG_MAGIC:
        return False, "不是内嵌 PNG"
    h, w, ch = (list(feat.get("shape") or []) + [0, 0, 0])[:3]
    try:
        import io

        import numpy as np
        from PIL import Image

        with Image.open(io.BytesIO(blob)) as img:
            mode = img.mode
            arr = np.asarray(img)
        if arr.shape[:2] != (h, w):
            return False, f"解码尺寸 {arr.shape[:2]} ≠ ({h},{w})"
        if depth and (mode not in ("I;16", "I;16B", "I;16L", "I") or arr.dtype.kind not in "ui"):
            return False, f"深度应为 16-bit 灰度，PIL mode={mode}"
        return True, ""
    except ImportError:
        ihdr_w, ihdr_h = int.from_bytes(blob[16:20], "big"), int.from_bytes(blob[20:24], "big")
        bit_depth, colour = blob[24], blob[25]
        if (ihdr_h, ihdr_w) != (h, w):
            return False, f"IHDR 尺寸 ({ihdr_h},{ihdr_w}) ≠ ({h},{w})"
        if depth and (bit_depth, colour) != (16, 0):
            return False, "深度应为 16-bit 灰度"
        return True, ""


# ----------------------------------------------------------------- lerobot

def real_lerobot(root: Path, n_frames: int) -> int:
    os.environ.setdefault("HF_HUB_OFFLINE", "1")
    # `datasets` 把 parquet 物化成 arrow 缓存（默认 ~/.cache/huggingface/
    # datasets）——冒烟用一次性目录，跑完即删，不污染用户缓存。
    if "HF_DATASETS_CACHE" not in os.environ:
        import atexit
        import shutil
        import tempfile

        tmp = tempfile.mkdtemp(prefix="oriera-lerobot-smoke-")
        os.environ["HF_DATASETS_CACHE"] = tmp
        atexit.register(shutil.rmtree, tmp, ignore_errors=True)
    try:
        import lerobot
        import torch
        from lerobot.datasets.lerobot_dataset import LeRobotDataset
    except ImportError as e:
        print(f"错误：lerobot 后端需要 lerobot（pip install \"lerobot[dataset]==<pin>\"）：{e}")
        return 2
    try:
        from lerobot.utils.feature_utils import dataset_to_policy_features
    except ImportError:  # lerobot 0.4.x
        from lerobot.datasets.utils import dataset_to_policy_features
    c = Check()
    print(f"  lerobot {getattr(lerobot, '__version__', '?')} · torch {torch.__version__}")
    info = json.loads((root / "meta" / "info.json").read_text(encoding="utf-8"))
    feats = info.get("features") or {}
    try:
        ds = LeRobotDataset("local/oriera-smoke", root=root, video_backend="pyav")
    except Exception as e:  # noqa: BLE001 — the verdict IS whether it loads
        c.expect(False, f"LeRobotDataset 加载：{type(e).__name__}: {e}")
        return c.report("lerobot")
    c.expect(True, f"LeRobotDataset 加载（{ds.num_episodes} 集 / {ds.num_frames} 帧）")
    c.expect(ds.num_frames == info.get("total_frames") and ds.num_episodes == info.get("total_episodes"),
             "num_frames / num_episodes == info.json")
    meta = ds.meta
    c.expect(sorted(meta.video_keys) == sorted(k for k, v in feats.items() if v.get("dtype") == "video"),
             f"meta.video_keys == video 特性（{meta.video_keys}）")
    depth_keys = list(getattr(meta, "depth_keys", []))
    want_depth = [k for k, v in feats.items() if (v.get("info") or {}).get("is_depth_map")]
    c.expect(sorted(depth_keys) == sorted(want_depth) or not hasattr(meta, "depth_keys"),
             f"meta.depth_keys == is_depth_map 特性（{depth_keys}）")
    try:
        pf = dataset_to_policy_features(meta.features)
        c.expect(True, f"dataset_to_policy_features（lerobot-train 入口）：{sorted(pf)}")
    except Exception as e:  # noqa: BLE001
        c.expect(False, f"dataset_to_policy_features：{type(e).__name__}: {e}")
    tasks = [json.loads(line) for line in (root / "raw_jsonl" / "tasks.jsonl").read_text(encoding="utf-8").splitlines()
             if line.strip()] if (root / "raw_jsonl" / "tasks.jsonl").is_file() else []
    task_by_index = {t["task_index"]: t["task"] for t in tasks}
    problems: list[str] = []
    picks = sample_indices(ds.num_frames, n_frames)
    for i in picks:
        try:
            item = ds[i]
        except Exception as e:  # noqa: BLE001
            problems.append(f"ds[{i}] {type(e).__name__}: {e}")
            continue
        for k, f in feats.items():
            if k in BOOKKEEPING or k not in item:
                if k not in BOOKKEEPING and k not in item:
                    problems.append(f"ds[{i}] 缺 {k}")
                continue
            t = item[k]
            shape = list(f.get("shape") or [])
            if t is None:
                # lerobot 0.7 对 parquet 缺列不报错、静默填 None（实测 caa207c
                # 转换器丢深度列的产物即如此）——这是数据丢失，判失败
                problems.append(f"ds[{i}] {k} 为 None（data parquet 缺列，lerobot 静默填空）")
                continue
            if f.get("dtype") in ("video", "image"):
                want = [shape[2], shape[0], shape[1]]
                if list(t.shape) != want:
                    problems.append(f"ds[{i}] {k} shape {list(t.shape)} ≠ {want}")
                elif k in want_depth:
                    if t.dtype != torch.float32 or float(t.max()) <= 0:
                        problems.append(f"ds[{i}] {k} 深度应为 float32 毫米且非全零（{t.dtype}, max {float(t.max())}）")
                elif f.get("dtype") == "video" and not (0.0 <= float(t.min()) and float(t.max()) <= 1.0):
                    problems.append(f"ds[{i}] {k} 视频帧应在 [0,1]")
            elif list(t.shape) != shape:
                problems.append(f"ds[{i}] {k} shape {list(t.shape)} ≠ {shape}")
        if task_by_index and item.get("task") != task_by_index.get(int(item["task_index"])):
            problems.append(f"ds[{i}] task {item.get('task')!r} ≠ {task_by_index.get(int(item['task_index']))!r}")
    c.expect(not problems, f"ds[i] 均匀抽 {len(picks)} 帧解码（形状/深度毫米/视频[0,1]/task）"
             + (f"——问题 {problems[:4]}" if problems else ""))
    try:
        loader = torch.utils.data.DataLoader(ds, batch_size=4, shuffle=False, num_workers=0)
        batch = next(iter(loader))
        c.expect(batch["index"].shape[0] == min(4, ds.num_frames), "DataLoader 取一个 batch（batch_size=4）")
    except Exception as e:  # noqa: BLE001
        c.expect(False, f"DataLoader batch：{type(e).__name__}: {e}")
    return c.report("lerobot")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("root", type=Path, help="转换后的 LeRobot v3.0 目录（jsonl_to_lerobot_v21.py 之后）")
    ap.add_argument("--backend", choices=("emulate", "lerobot"), default="emulate",
                    help="emulate = 零 lerobot 依赖的读路径复刻（PR 门禁 a 层）；lerobot = 真 LeRobotDataset（b 层）")
    ap.add_argument("--frames", type=int, default=32, help="均匀抽查的帧数（默认 32）")
    args = ap.parse_args()
    print(f"LeRobot 加载冒烟 · backend={args.backend} · {args.root}")
    if args.backend == "lerobot":
        return real_lerobot(args.root, args.frames)
    return emulate(args.root, args.frames)


if __name__ == "__main__":
    sys.exit(main())
