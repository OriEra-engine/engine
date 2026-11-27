# 第 26 刀：蒙皮网格持久路径（RFC 0004 步 1c）

> **工作记录（2026-11）。** 文中"本机/开发机"指当时跑基准的那台没有 GPU 的机器（llvmpipe 软件光栅化），毫秒数不是 GPU 帧时间，只作同机回归。文中的"补丁"是当时的交付方式，对应关系见[索引](../README.md)。

补丁：`026-skinned-persistent-path-step1c.patch`（基于 `0387718`，依赖 023、024、025）
状态：已落地，正确性验过，**性能只有本机软件光栅化的量级读数**

## 一句话

NPC 角色不用再每帧重建顶点缓冲和绑定组了：一个骨架一份关节 uniform，每帧只更新面板，实例引用骨架即可出画。和快照路径逐像素一致，本机 64 个蒙皮实例 544p 9.2 ms，比快照路径便宜约两成。

## 一、为什么是这一步

这一刀属于"不需要真卡就能先做"的那一批：蒙皮持久路径、回读、像素等价测试全在 CPU 侧。

它在 RFC 0004 里原本被明确划在范围外，第 6 节写的理由是"每实例一份关节面板，涉及 uniform 容量上限，复杂度高于静态网格，等 1a 有数据再议"。既然做了，就一并登记进 RFC，第 6 节改写成"已落地"，第 2 个开放问题从"要不要提前"改成"NPC 上限是多少"。

## 二、语义：和快照路径对齐

顶点在 bind 空间，面板是 bind→世界的 4×4，模型的刚体变换由调用方**预乘进面板**。所以蒙皮实例**没有** model 矩阵，这是和静态 `Instance` 最大的不同。

一个骨架一份 16KB uniform（`MAX_JOINTS = 256`），每帧 `update_skeleton` 写面板。共用同一骨架的多个实例会同步改姿势——这是有意的取舍，换来的是不必给每个实例单独拷贝面板。

## 三、做了什么

| 接口 | 行为 |
|---|---|
| `upload_skinned_mesh(positions, normals, uvs, joints, weights, indices)` | 一次上传；校验非空、长度一致、索引在范围内、关节号 < 256；记下网格用到的最大关节号 |
| `GpuSkinnedMesh` | 顶点 64B：pos/normal/uv + JOINTS Uint32×4 + WEIGHTS Float32×4；无 `id` |
| `create_skeleton()` / `update_skeleton(&GpuSkeleton, palette)` | 16KB uniform；面板 1..=256 个，行主序输入、按列写，同时记 `joint_count()` |
| `SkinnedInstance { mesh, material, skeleton }` / `SkinnedDrawList` | 与静态 `DrawList` 平行的独立列表 |
| `render_frame(&DrawList, &SkinnedDrawList, camera, target)` | 一次清屏、一个 pass：先静态归段绘制，再逐个蒙皮实例切管线、绑 camera/material/joints 三组、`draw_indexed` |
| `FrameStats::skinned_draws` | 本帧蒙皮绘制次数。旧的 `render_draw_list` 变成薄封装，行为不变 |

渲染前逐实例校验 `mesh.max_joint < skeleton.joint_count()`，不满足报 "exceeds skeleton palette length …; call update_skeleton"——没写面板、写少了都不会静默画错。

### 踩过的坑（给以后看的人）

- 蒙皮着色器和静态路径共用同一份片元着色器（`pbr_fragment!` 宏）。共用意味着名字必须对齐：顶点输出结构要叫 `VSOut`，全局绑定要叫 `cam` / `base_tex` / `mr_tex` / `base_samp` / `material`，`struct Material{color,mr}` 的字段也不能改。写成 `SkinVSOut`、`skin_cam` 之类，shader module 创建时就 validation error，二十条测试连带全红。
- `PersistVertex` 是线性布局、顶点步长 40；`SkinVertex` 是 64B（多出 JOINTS/WEIGHTS），两个管线各自描述，不能共用 `vertex_layout`。
- 管线无混合、深度 `Depth32Float` Less + write，和静态路径一致。

## 四、验收

持久路径测试从 16 条加到 20 条（S1–S4，全 `#[ignore]`，需要本机适配器），本地 20 passed。

| 断言 | 结果 |
|---|---|
| S1 与快照路径逐像素一致 | 通过。同一姿势下 `render_skinned_views` 与持久蒙皮路径差异 0；同帧 `skinned_draws=1`、`draw_calls=1`、`instances=0` |
| S2 换面板会动 | 通过。换一份质心右移的面板后，像素质心右移 > 20 px |
| S3 输入校验 | 通过。关节号 ≥ 256、长度不匹配、索引越界、空面板、257 面板、未写面板即渲染、面板只 1 关节却引用第 5 号——七种都报错，后两种走 palette 长度那条 |
| S4 静态与蒙皮同帧 | 通过。静态红、蒙皮蓝，两种前后顺序下中心像素分别取到蓝/红，说明描画顺序按"先静态后蒙皮"而非提交顺序；同帧 `draw_calls=2`、`instances=1`、`skinned_draws=1`；每帧换面板、连跑 10 帧 `created_buffers/textures/bind_groups` 全为 0 |

门禁（2026-11-25 实跑）：

- **工作区（叠加 001–026）**：fmt 干净；clippy `-D warnings` 三档零告警；测试 **656 passed / 0 failed / 4 ignored**，与叠加 025 时一致（这一步没往工作区加非 ignore 测试）；`render-wgpu` 下 18 passed / 3 ignored，持久路径 20 条 ignored；JS 130 pass。
- **干净工作树**（`0387718` + 023 + 024 + 025 + 026，独立 target）：fmt 干净、clippy 零告警、18 passed / 3 ignored、持久路径 20 passed、workspace 全绿 0 failed。
- 补丁 7 个文件、995 行新增、19 行删除，57766 字节；逐文件与工作区 `cmp` 一致。

一个操作教训：**两棵源码树不能共用一个 `CARGO_TARGET_DIR`**。这次先在同一 target 里编了只含 023–026 的干净树，再回主工作区跑，cargo 复用了那棵树的产物，报出 `oriera_infer::InferWaitMeter` 未解析、`JobDone` 没有 `elapsed_us` 字段——看起来像源码坏了，其实是 target 目录串了。换独立 target 复跑即恢复正常。另外磁盘只有 9.8G，两个 target 同时留必然写满，跑到一半 `No space left on device`。

## 五、读数

llvmpipe，中位数，ms/帧；覆盖率是非背景像素占比。`skindraw` 是快照路径 `render_skinned_views`，`skin` 是持久蒙皮，两轴都是 n 个实例各占屏幕一格、各用自己的面板。

| 场景 | n=1 | n=16 | n=64 | 覆盖率 1/16/64 |
|---|---|---|---|---|
| skindraw / 966×544 | 10.1 | 7.3 | 11.8 | 86.5 / 80.5 / 83.1 % |
| skin / 966×544 | 7.4 | 7.7 | 9.2 | 86.5 / 80.5 / 83.1 % |
| skindraw / 1920×1080 | 42.1 | 48.7 | 53.4 | 86.5 / 80.2 / 83.1 % |
| skin / 1920×1080 | 28.9 | 28.8 | 35.8 | 86.5 / 80.2 / 83.1 % |

怎么读：

- **两轴覆盖率对齐**（80–86%）说明画的确实是同一批像素。第一版基准写错成所有骨架共用 0 号面板，skin 轴 16/64 档覆盖率只有 1.3% / 0.0%，读数还随着 n 变大而下降——那是假数据，已修。
- **`skin` 比 `skindraw` 便宜约两成**（544p 64 个：9.2 对 11.8），边际约 22–31 µs/骨架，主要是每帧每骨架一次 16KB uniform 写 + 一次 bind group 切换。
- **64 个 NPC 光蒙皮就要 9 ms**，是这批读数里最贵的一档。大半仍然是软件光栅化在吃 CPU（同 1a 的结论），真卡上必须重测。
- 跨轮波动两成多的老问题还在，个位数百分点不构成结论。

## 六、没做

- 窗口与交换链（RFC 0004 步 2，需单独评审）。
- 蒙皮只支持 4 关节权重、无混合，半透明蒙皮物件仍走快照路径。
- 把真实帧时间喂给 `RenderSystem` 的自适应调速。
- 真卡读数——见《跑在主机上的任务清单》。
