# 3DGS 办公室重建管线

将真实办公室照片变成 OriEra 可渲染的高斯泼溅场景。

## 管线概览

```
📱 拍照 → 🧮 COLMAP 位姿 → 🔥 gsplat 训练 → 📦 PLY → 🖥️ OriEra 渲染
```

## 依赖安装（一次性）

| 工具 | 许可 | 安装 | 状态 |
|---|---|---|---|
| [COLMAP](https://github.com/colmap/colmap) | BSD | [最新 release](https://github.com/colmap/colmap/releases) .zip 解压加 PATH | ⬜ 待装 |
| [PyTorch](https://pytorch.org) ≥2.0 CUDA | BSD | Python 3.13 venv + `pip install torch --index-url https://download.pytorch.org/whl/cu124` | ✅ 已装(2.6.0+cu124) |
| [gsplat](https://github.com/nerfstudio-project/gsplat) | Apache-2.0 | `pip install gsplat` | ✅ 已装(1.5.3) |
| [pycolmap](https://github.com/colmap/colmap/blob/main/pycolmap) | BSD | `pip install pycolmap` | ✅ 已装(4.2.1) |
| **CUDA Toolkit**（nvcc 编译器） | NVIDIA | [developer.nvidia.com/cuda-downloads](https://developer.nvidia.com/cuda-downloads) | 🔴 **待装（关键缺口）** |
| [imageio-ffmpeg](https://github.com/imageio/imageio-ffmpeg) | BSD | `pip install imageio-ffmpeg` | ✅ 已装 |

### Python 3.14 兼容性注意事项

PyTorch CUDA wheel 尚未发布 Python 3.14 版本——**须用 Python 3.13 venv**：

```bash
py -3.13 -m venv D:/OriEra/3dgs-env
D:/OriEra/3dgs-env/Scripts/pip install torch --index-url https://download.pytorch.org/whl/cu124 --no-deps
D:/OriEra/3dgs-env/Scripts/pip install typing_extensions jinja2 fsspec sympy networkx filelock
D:/OriEra/3dgs-env/Scripts/pip install gsplat pycolmap numpy pillow
```

已验证（GTX 1650 Ti · CUDA 12.4 · PyTorch 2.6.0）：
- `torch.cuda.is_available()` = True ✓
- `gsplat.rasterization()` 前向 = ✓（CUDA kernel 编译须 nvcc，见下）

### CUDA Toolkit 安装（最后一个系统级缺口）

gsplat 的 CUDA kernel 需 nvcc 编译器（CUDA **runtime** 已随 PyTorch cu124 就位，但 **compiler** 需单独安装）。

1. 下载：https://developer.nvidia.com/cuda-downloads（选 Windows ×86_64 → 12.4）
2. 安装：`cuda_12.4.x_windows.exe`（~3GB，需管理员权限，约 10 分钟）
3. 安装后 nvcc 位于 `C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.4\bin\nvcc.exe`
4. 确认：`nvcc --version` 输出版本号
5. 重跑 `python scripts/3dgs_pipeline.py` — gsplat 会自动用 nvcc JIT 编译 CUDA kernel（首次 ~3-5 分钟，后续缓存）

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
