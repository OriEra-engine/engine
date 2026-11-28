# 3DGS 办公室重建管线

将真实办公室照片变成 OriEra 可渲染的高斯泼溅场景。

## 管线概览

```
📱 拍照 → 🧮 COLMAP 位姿 → 🔥 gsplat 训练 → 📦 PLY → 🖥️ OriEra 渲染
```

## 依赖安装（一次性）

| 工具 | 许可 | 安装 |
|---|---|---|
| [COLMAP](https://github.com/colmap/colmap) | BSD | [最新 release](https://github.com/colmap/colmap/releases) .zip 解压加 PATH |
| [PyTorch](https://pytorch.org) ≥2.0 CUDA | BSD | `pip install torch --index-url https://download.pytorch.org/whl/cu121` |
| [gsplat](https://github.com/nerfstudio-project/gsplat) | Apache-2.0 | `pip install gsplat` |
| [pycolmap](https://github.com/colmap/colmap/blob/main/pycolmap) | BSD | `pip install pycolmap` |
| [imageio-ffmpeg](https://github.com/imageio/imageio-ffmpeg) | BSD | `pip install imageio-ffmpeg`（动画合成用） |

## 拍照建议

- 手机环绕房间 2-3 圈（整体/中距/特写），重叠度 ≥60%；
- 避免玻璃/镜面/纯白墙面（SfM 特征点不足）；
- 每张照片间移动 ~15-20° 视角，总 80-200 张足够；
- 光照均匀，避免强逆光。

## 一键运行

```bash
python scripts/3dgs_pipeline.py --photos ./office_photos --out ./office_3dgs
```

## 分步运行

```bash
# 仅 COLMAP（相机位姿估计）
python scripts/3dgs_pipeline.py --photos ./office_photos --out ./office_3dgs --step colmap

# 仅格式转换（COLMAP BIN → transforms.json）
python scripts/3dgs_pipeline.py --photos ./office_photos --out ./office_3dgs --step convert

# 仅训练（需要先完成 colmap + convert）
python scripts/3dgs_pipeline.py --photos ./office_photos --out ./office_3dgs --step train

# 快速预览（7K 迭代，~3 分钟 GPU）
python scripts/3dgs_pipeline.py --photos ./office_photos --out ./office_3dgs --iterations 7000
```

## OriEra 加载

```rust
use oriera_asset::ply::decode_splat_ply;

let bytes = std::fs::read("office_3dgs/output/scene.ply")?;
let cloud = decode_splat_ply(&bytes)?;
println!("{} splats loaded", cloud.len());
```

## 性能预期

| 泼溅数 | PLY 大小 | 解码耗时 | 渲染 FPS（GPU） |
|---|---|---|---|
| 10K | 0.7MB | <10ms | >100 |
| 100K | 7MB | ~50ms | >60 |
| 500K | 34MB | ~200ms | >30 |
| 1M | 68MB | ~400ms | >20 |

（RTX 3050+ / Intel Arc / Apple M1+ 可达上表 FPS；核显需 DX12 后端）

## 许可说明

| 工具 | 许可 | 商用 |
|---|---|---|
| COLMAP | BSD-3 | ✅ |
| gsplat | Apache-2.0 | ✅ |
| INRIA 原版 3DGS | **非商业** | ❌ 不要用 |
| SOGS 压缩 | 开源（Fraunhofer HHI） | ✅ |
