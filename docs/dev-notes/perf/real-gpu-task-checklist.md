# 跑在主机上的任务清单（需要真 GPU）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

适用范围：本机（云主机 llvmpipe，`device_type=Cpu`）只能给量级，不能给结论。下面这些事必须落到一块有真显卡的机器上才能推进。整份清单按"必须先确认机器合格 → 再跑 → 再看什么"的顺序写。

## 一、先确认机器能跑

判据（两条都要满足）：

```bash
ls /dev/dri/            # 要看到 renderD128（或 renderD1xx）这类节点；只有 card0 没有 renderD* 说明没暴露渲染节点
vulkaninfo --summary    # 要列出非 Cpu 的 device（NVIDIA / Intel / AMD），deviceType 不能是 CPU
```

不满足就别往下走：这时 wgpu 要么报 "no usable adapter"，要么悄悄回落到 llvmpipe，跑出来的数字和本机一样。

机器要求：

- **Linux 无头即可**，不需要图形界面、不需要 X/Wayland。全程 headless 渲染 + 像素回读。
- 一张至少 8G 显存的卡即可覆盖现有基准（最高负载是 1920×1080、2048 实例，单帧几十 MB 量级）。
- 驱动与 Vulkan ICD 装齐：
  - NVIDIA：官方驱动（自带 Vulkan ICD），确认 `libGLX_nvidia` / `nvidia_icd.json` 在位；
  - Intel / AMD：mesa 的 `mesa-vulkan-drivers`（`anv` / `radv`）。
- 能拉 crates.io（首次编译要下载依赖）。
- **磁盘留 ~4G**（一个 `CARGO_TARGET_DIR` 全量编译约 2G，再加 bench 产物）。本机就是被 9.8G 根分区卡过：两个 target 并存直接 `No space left on device`，cargo 报错退出，很容易被误读成代码问题。
- 如果给的是容器：`--device /dev/dri` 挂进去，`--gpus all`（NVIDIA）或对应组权限（`video`/`render`）。
- 编译约十分钟（4 核以上）。

## 二、怎么把代码放上去

仓库基线 `0387718`（v0.13.0），按序应用补丁 023 → 024 → 025 → 026（026 依赖前三者）。023 只用基线，可以单独跑。

```bash
git clone <repo> && cd <repo> && git checkout 0387718
git apply patches/023-perf-bench-gpu-frame.patch
git apply patches/024-persistent-render-path-step1a.patch
git apply patches/025-async-readback-step1b.patch
git apply patches/026-skinned-persistent-path-step1c.patch
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=/tmp/wgpu-target     # 单独一个，不要和工作区共用
```

`build.rs`/feature 都不需要改：全部在 `render-wgpu` feature 内，默认关。

## 三、要跑的三件事

### 1. 七轴帧成本基准（主任务，约 3 分钟）

```bash
WGPU_BACKEND=vulkan cargo bench -p oriera-render --features render-wgpu \
  --bench gpu_frame -- --nocapture breakdown
```

七轴：`draw` / `fill` / `persist` / `psubmit` / `pasync` / `skindraw` / `skin`，两档分辨率（966×544、1920×1080），n = 1/16/64/512/2048（部分轴只取 1/16/64）。

若 Vulkan 不可用，退一步用 `WGPU_BACKEND=gl`（GL 下部分实例化路径行为可能不同，读数要标注后端）。

### 2. 持久路径 20 条测试（约 10 秒）

```bash
WGPU_BACKEND=vulkan cargo test -p oriera-render --features render-wgpu \
  --test persist_path -- --ignored --test-threads=1
```

预期 20 passed。这 20 条里有 6 条是**逐像素等价**断言（快照路径对持久路径、静态对蒙皮）。本机软件光栅化下差异为 0，换到真驱动后如果出现非零差异，那本身就是重要发现（浮点/采样精度/驱动差异），要连同差异图和数值一起记录。

### 3. 顺手确认整机门槛没退化

```bash
WGPU_BACKEND=vulkan cargo test -q --workspace          # 预期 656 passed / 0 failed / 4 ignored
```

## 四、读数怎么判

对照本机（llvmpipe）值：

| 指标 | 本机 | 真卡上要看什么 |
|---|---|---|
| `persist` 每实例边际（2048 实例） | 7–14 µs（1a 达标记录，目标是一个数量级以上） | **是否落到 1 µs 量级**。若还是十几微秒，说明瓶颈不在光栅化而在提交/绑定组，1a 的结论要重写 |
| `psubmit` vs `persist` | 两者接近 | 差值就是同步回读的占比。差得越多，1b 的异步回读越有价值 |
| `pasync` vs `persist` | 互有高低，在噪声内 | 真卡上 `pasync` 应明显低于 `persist`（同步等待占比被摊掉）。若仍无差别，1b 可以判定为不需要 |
| `skin` vs `skindraw`（n=64） | 9.2 vs 11.8 ms（便宜两成） | 真卡上每骨架 16KB 上传 + bind group 切换应该更便宜；若差距反而拉大，说明 CPU 端提交成了瓶颈 |
| 整帧绝对值 | 544p 20–27 ms（波动两成多） | 1080p 2048 实例要在 16.6 ms 以内才算 60fps 有戏 |

三条判读纪律：

- 同一档位跑三次，报中位数与极差。本机跨轮波动两成多，单次读数不算数。
- 报数必须带后端（vulkan/gl）与适配器名（`device_type`、驱动版本）。
- 性能数字只进内部基线，不进公开仓库文档。

## 五、这份清单要闭掉的三个悬空问题

1. **1a 到底差多少**：目标"一个数量级以上"，本机只到 3–6 倍。真卡读数决定是继续优化存储/绑定组，还是承认目标定错了。
2. **1b 值不值**：异步回读目前只有结构上的理由，没有测量证据。真卡上 `pasync` 与 `persist` 的差就是答案。
3. **每实例 7–14 µs 里有多少是真卡上也有的**：本机这部分是软件光栅化的顶点变换成本（折合每顶点 0.4–0.6 µs），真卡上按理不该存在。如果真卡上仍然存在，问题就在提交路径（每实例一次 bind group / 缓冲写入），那才是要改的地方。

## 六、还不需要真卡的事（对照）

已经在本机做完、不需要上主机的部分：持久路径与异步回读的实现与等价测试、蒙皮持久路径（步 1c）、输入校验、静态与蒙皮同帧顺序、缓冲复用计数、JS 侧 130 条测试、整机帧成本基准（纯 CPU 段）。

仍然要等真卡的：上表全部读数、窗口与交换链（RFC 0004 步 2，另有评审门槛，和真卡无关但要在有卡环境验 surface）。

## 七、风险与坑（都已踩过）

- **target 目录串用**：两棵源码树共用一个 `CARGO_TARGET_DIR`，cargo 会按包名+指纹复用产物，报出"某类型不存在"这种看着像源码坏了的错误。每棵树用独立 target。
- **磁盘**：一个全量 target ≈ 2G。先 `df -h`，不够就先删旧 target 再编译。
- **默认后端不可用**：本机不设 `WGPU_BACKEND=gl` 会报 "no usable adapter"；真卡上优先 `vulkan`。
- **测试要加 `--test-threads=1`**：这些 `#[ignore]` 测试共享 GPU 队列语义，并发跑会互相干扰读数与断言。
