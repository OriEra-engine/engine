//! L5 GPU 帧成本微基准 —— 快照路径 `render_scene_views` 与持久路径 `render_draw_list`。
//!
//! 这是渲染层第一个基准。此前的整机基准（`oriera-agent/benches/world_frame`）
//! 只含物理 / 记忆 / 推理 / 闸口四段，**不含渲染**——所以"OriEra 能不能
//! 跑 60fps 画面"一直没有根据。本文件补上这一段。
//!
//! ## 两根轴分开量
//!
//! | 轴 | 场景 | 量的是什么 |
//! |---|---|---|
//! | 绘制数 | `draw/n{parts}`：n 个方块平铺在视锥内，互不遮挡 | draw call、顶点上传、bind group 构造 |
//! | 填充率 | `fill/overdraw{parts}`：n 个方块叠在同一位置 | 片元着色、深度测试 |
//!
//! 持久路径（RFC 0004 步 1a）再两根轴，布局与 `draw/n` 相同（同网格、同分辨率、
//! 同覆盖率），区别只在资源是否跨帧复用、绘制是否实例化：
//!
//! | 轴 | 场景 | 量的是什么 |
//! |---|---|---|
//! | `persist/n` | `render_draw_list` + 显式 `read_target` | 与 `draw/n` **同口径**：都等到 GPU 出像素 |
//! | `psubmit/n` | 只 `render_draw_list`，不回读 | 纯 CPU 提交成本（GPU 侧未完成，不含光栅化） |
//!
//! 分辨率两档：966×544（对标演示视频）与 1920×1080。
//!
//! ## 读数前必须看适配器类型
//!
//! 本基准带 `--nocapture` 时打印适配器名称与类型。**软件光栅化
//! （llvmpipe / SwiftShader / device_type=Cpu）的读数不是 GPU 帧时间**：
//! 它反映的是 CPU 侧提交与光栅化成本，只能作同机回归对比，不能用来
//! 回答"真 GPU 上能否 60fps"。真 GPU 数字须在带硬件显卡的机器上重跑。
//!
//! ## 跑法
//!
//! ```sh
//! WGPU_BACKEND=gl cargo bench -p oriera-render --features render-wgpu --bench gpu_frame -- --nocapture
//! ```
//!
//! `render_scene_views` 每次调用都新建颜色 / 深度纹理与顶点缓冲（快照式
//! 接口，非持久帧循环），所以这里的每帧成本**包含目标重建**——这本身
//! 就是当前接口形态下的一帧，不另做摊销。
//!
//! 内部基线，未标定硬件与构建形态前不对外。

#![cfg(feature = "render-wgpu")]

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_render::camera::Camera;
use oriera_render::gpu::{
    DrawList, GpuMaterial, GpuSkeleton, GpuSkinnedMesh, HeadlessRenderer, ReadTicket, RenderTarget,
    RenderedImage, ScenePart, SkinnedDrawList, SkinnedPart,
};

const FRAME_BUDGET_US: f64 = 16_667.0;
/// 两档分辨率的宽高比相同（约 16:9），网格按它铺满。
const ASPECT: f32 = 16.0 / 9.0;
/// QY-Demo 的分辨率，以及常见的 1080p。
const RESOLUTIONS: [(u32, u32); 2] = [(966, 544), (1920, 1080)];
/// 四根轴；criterion 主体与 breakdown 共用，避免两边口径不一致。
/// fill 轴全屏 overdraw 在软件光栅化下每倍约 1–4 ms，档位压到 64 以内保证单帧可测。
#[derive(Clone, Copy)]
enum Axis {
    /// 快照路径，平铺：增长的是绘制数。
    Draw,
    /// 快照路径，全叠：增长的是 overdraw 倍数。
    Fill,
    /// 持久路径，提交 + 显式回读（与快照路径同口径）。
    Persist,
    /// 持久路径，只提交不回读。
    PersistSubmit,
    /// 提交 + 异步回读,两帧在途流水。
    PersistAsync,
    /// 快照路径的蒙皮(n 个部件,每部件各自建缓冲)。
    SkinDraw,
    /// 持久路径的蒙皮(n 个实例,各自一个骨架,面板每帧更新)。
    Skin,
}

const AXES: [(&str, Axis, &[usize]); 7] = [
    ("draw", Axis::Draw, &[1, 64, 512, 2048]),
    ("fill", Axis::Fill, &[4, 16, 64]),
    ("persist", Axis::Persist, &[1, 64, 512, 2048]),
    ("psubmit", Axis::PersistSubmit, &[1, 64, 512, 2048]),
    ("pasync", Axis::PersistAsync, &[1, 64, 512, 2048]),
    ("skindraw", Axis::SkinDraw, &[1, 16, 64]),
    ("skin", Axis::Skin, &[1, 16, 64]),
];

struct Mesh {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

/// 24 顶点方块（每面独立法线），36 索引。
fn unit_cube() -> Mesh {
    const FACES: [([f32; 3], [[f32; 3]; 4]); 6] = [
        (
            [0.0, 0.0, 1.0],
            [[-1., -1., 1.], [1., -1., 1.], [1., 1., 1.], [-1., 1., 1.]],
        ),
        (
            [0.0, 0.0, -1.0],
            [
                [1., -1., -1.],
                [-1., -1., -1.],
                [-1., 1., -1.],
                [1., 1., -1.],
            ],
        ),
        (
            [1.0, 0.0, 0.0],
            [[1., -1., 1.], [1., -1., -1.], [1., 1., -1.], [1., 1., 1.]],
        ),
        (
            [-1.0, 0.0, 0.0],
            [
                [-1., -1., -1.],
                [-1., -1., 1.],
                [-1., 1., 1.],
                [-1., 1., -1.],
            ],
        ),
        (
            [0.0, 1.0, 0.0],
            [[-1., 1., 1.], [1., 1., 1.], [1., 1., -1.], [-1., 1., -1.]],
        ),
        (
            [0.0, -1.0, 0.0],
            [
                [-1., -1., -1.],
                [1., -1., -1.],
                [1., -1., 1.],
                [-1., -1., 1.],
            ],
        ),
    ];
    let mut m = Mesh {
        positions: Vec::with_capacity(24),
        normals: Vec::with_capacity(24),
        indices: Vec::with_capacity(36),
    };
    for (fi, (n, quad)) in FACES.iter().enumerate() {
        let base = (fi * 4) as u32;
        for p in quad {
            m.positions.push(*p);
            m.normals.push(*n);
        }
        m.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    m
}

/// n 个方块，**网格始终铺满视口**（覆盖率与 n 基本无关）：
/// - `spread=true` 平铺互不遮挡 → 增长的是绘制数；
/// - `spread=false` 全叠在同一位置 → 增长的是 overdraw 倍数。
///
/// 两轴的唯一差别是遮挡关系，所以 `fill(n) - draw(1)` 就是 n 倍
/// overdraw 的边际成本，`draw(n)/n` 是每个绘制单元的成本。
fn scene(n: usize, spread: bool, meshes: &mut Vec<Mesh>) -> Vec<ScenePart<'_>> {
    meshes.clear();
    let cube = unit_cube();
    meshes.reserve(n);
    let side = (n as f32).sqrt().ceil().max(1.0);
    // 全叠时只按 1 格铺满，方块大小不随 n 变化。
    let side = if spread { side } else { 1.0 };
    // 相机在 z=90、fov 60°：半高 = 90·tan30° ≈ 52，半宽 = 半高 × 宽高比。
    let half_h = 90.0 * 30f32.to_radians().tan() * 0.95;
    // 每格边长使 side 列正好铺满宽度；方块占格子的 90%。
    let cell = 2.0 * half_h * ASPECT / side;
    let half = cell * 0.45;
    for i in 0..n {
        let mut m = Mesh {
            positions: cube.positions.clone(),
            normals: cube.normals.clone(),
            indices: cube.indices.clone(),
        };
        let (dx, dy) = if spread {
            (
                ((i as f32 % side) + 0.5 - side / 2.0) * cell,
                ((i as f32 / side) + 0.5 - side / 2.0) * cell,
            )
        } else {
            (0.0, 0.0)
        };
        for p in &mut m.positions {
            p[0] = p[0] * half + dx;
            p[1] = p[1] * half + dy;
        }
        meshes.push(m);
    }
    meshes
        .iter()
        .map(|m| ScenePart {
            positions: &m.positions,
            normals: Some(&m.normals),
            uvs: None,
            indices: &m.indices,
            base_color: [0.6, 0.6, 0.62, 1.0],
            metallic: 0.0,
            roughness: 0.8,
            base_texture: None,
            mr_texture: None,
        })
        .collect()
}

fn camera(w: u32, h: u32) -> Camera {
    // 相机在网格前上方看向原点；far 留足容纳最大的平铺。
    Camera::look_at(
        [0.0, 0.0, 90.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        60.0,
        w,
        h,
        0.1,
        500.0,
    )
}

fn frame_us(
    r: &HeadlessRenderer,
    meshes: &mut Vec<Mesh>,
    n: usize,
    spread: bool,
    cam: &Camera,
) -> f64 {
    let parts = scene(n, spread, meshes);
    let t = std::time::Instant::now();
    let img = r
        .render_scene_views(&parts, cam)
        .expect("render must succeed");
    black_box(&img.bgra);
    t.elapsed().as_secs_f64() * 1e6
}

/// 持久路径的一帧。`readback=true` 时连同显式回读一起计时，
/// 与快照路径同口径（都等到 GPU 出像素）。
fn persist_us(
    r: &HeadlessRenderer,
    list: &DrawList<'_>,
    cam: &Camera,
    target: &oriera_render::gpu::RenderTarget,
    readback: bool,
) -> f64 {
    let t = std::time::Instant::now();
    let stats = r
        .render_draw_list(list, cam, target)
        .expect("persist render must succeed");
    if readback {
        let img = r.read_target(target).expect("readback must succeed");
        black_box(&img.bgra);
    }
    black_box(stats.draw_calls);
    t.elapsed().as_secs_f64() * 1e6
}

/// 异步回读的流水帧(吞吐口径,不是单帧延迟):两帧在途——第 i 帧提交并登记
/// 回读后,先去取两帧之前那张回执,再提交下一帧。8 帧一批,返回均摊到每帧的 µs。
/// GPU 在画第 N 帧时 CPU 已经在提交第 N+1 帧,这是同步回读做不到的。
fn pipelined_us(
    r: &HeadlessRenderer,
    list: &DrawList<'_>,
    cam: &Camera,
    target: &oriera_render::gpu::RenderTarget,
) -> f64 {
    const DEPTH: usize = 2;
    const FRAMES: usize = 8;
    let mut slots: Vec<Option<ReadTicket>> = (0..DEPTH).map(|_| None).collect();
    let t = std::time::Instant::now();
    for i in 0..FRAMES + DEPTH {
        if i >= DEPTH {
            let ticket = slots[i % DEPTH].take().expect("a ticket must be in flight");
            black_box(r.wait_read(ticket).expect("wait_read").bgra);
        }
        if i < FRAMES {
            r.render_draw_list(list, cam, target)
                .expect("persist render must succeed");
            slots[i % DEPTH] = Some(r.read_target_async(target).expect("async readback"));
        }
    }
    t.elapsed().as_secs_f64() * 1e6 / FRAMES as f64
}

/// 持久路径的场景：单位方块只上传一次，位置与大小走模型矩阵。
/// 布局与 `scene(n, true, ..)` 逐格相同，所以覆盖率可比。注意 z **不缩放**——
/// 快照路径只缩放 x、y（方块深度保持 ±1），这里必须一致，否则正面离镜头更近、
/// 投影变大，两轴的覆盖率就不是一回事了。
fn with_persist<T>(r: &HeadlessRenderer, n: usize, f: impl FnOnce(&DrawList<'_>) -> T) -> T {
    let cube = unit_cube();
    let mesh = r
        .upload_mesh(&cube.positions, Some(&cube.normals), None, &cube.indices)
        .expect("upload_mesh must succeed");
    let mat = r.create_material([0.6, 0.6, 0.62, 1.0], 0.0, 0.8, None, None);
    // 容量在计时之外显式给足：帧内不重建。
    r.reserve_instances(n.max(1) as u32);
    let side = (n as f32).sqrt().ceil().max(1.0);
    let half_h = 90.0 * 30f32.to_radians().tan() * 0.95;
    let cell = 2.0 * half_h * ASPECT / side;
    let half = cell * 0.45;
    let mut list = DrawList::new();
    for i in 0..n {
        let dx = ((i as f32 % side) + 0.5 - side / 2.0) * cell;
        let dy = ((i as f32 / side) + 0.5 - side / 2.0) * cell;
        list.push(
            &mesh,
            &mat,
            [
                [half, 0.0, 0.0, dx],
                [0.0, half, 0.0, dy],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        );
    }
    f(&list)
}

/// 清屏色的 BGRA 取整值（CLEAR = 0.05 / 0.08 / 0.13）。
const BACKGROUND: [u8; 4] = [33, 20, 13, 255];

/// 非背景像素占比：确认场景真的被画出来了，而不是整张空图。
/// 背景色取固定值而不是"左上角像素"——满屏场景里左上角会被盖住，
/// 那样会把整片正面当成背景，报出一个接近 0 的假覆盖率。
fn lit_ratio(img: &RenderedImage) -> f64 {
    let px = |i: usize| -> [u8; 4] {
        [
            img.bgra[i],
            img.bgra[i + 1],
            img.bgra[i + 2],
            img.bgra[i + 3],
        ]
    };
    let lit = (0..img.bgra.len() / 4)
        .filter(|&i| (0..3).any(|k| px(i * 4)[k].abs_diff(BACKGROUND[k]) > 1))
        .count();
    lit as f64 / (img.bgra.len() / 4) as f64
}

fn coverage(
    r: &HeadlessRenderer,
    meshes: &mut Vec<Mesh>,
    n: usize,
    spread: bool,
    cam: &Camera,
) -> f64 {
    let parts = scene(n, spread, meshes);
    let img = r
        .render_scene_views(&parts, cam)
        .expect("render must succeed");
    lit_ratio(&img)
}

/// 一档的测量：预热 5 帧、取 25 个样本的中位数，再单独量一次覆盖率。
fn measure(
    r: &HeadlessRenderer,
    axis: Axis,
    meshes: &mut Vec<Mesh>,
    n: usize,
    cam: &Camera,
    med: &dyn Fn(Vec<f64>) -> f64,
) -> (f64, f64) {
    match axis {
        Axis::Draw | Axis::Fill => {
            let spread = matches!(axis, Axis::Draw);
            for _ in 0..5 {
                frame_us(r, meshes, n, spread, cam);
            }
            let m = med((0..25)
                .map(|_| frame_us(r, meshes, n, spread, cam))
                .collect());
            (m, coverage(r, meshes, n, spread, cam))
        }
        Axis::SkinDraw => {
            let sb = SkinBench::new(n);
            for _ in 0..5 {
                sb.snapshot_us(r, n, cam);
            }
            let m = med((0..25).map(|_| sb.snapshot_us(r, n, cam)).collect());
            (m, lit_ratio(&sb.snapshot_once(r, n, cam)))
        }
        Axis::Skin => {
            let target = r.create_target(cam.width, cam.height).expect("target");
            let sb = SkinBench::new(n);
            let mesh = r
                .upload_skinned_mesh(
                    &sb.positions,
                    Some(&sb.normals),
                    None,
                    &sb.joints,
                    &sb.weights,
                    &sb.indices,
                )
                .expect("upload_skinned_mesh");
            let mat = r.create_material([0.6, 0.6, 0.62, 1.0], 0.0, 0.8, None, None);
            let skeletons: Vec<GpuSkeleton> = (0..n).map(|_| r.create_skeleton()).collect();
            let palettes = &sb.palettes;
            for _ in 0..5 {
                skinned_persist_us(r, &mesh, &mat, &skeletons, palettes, cam, &target);
            }
            let m = med((0..25)
                .map(|_| skinned_persist_us(r, &mesh, &mat, &skeletons, palettes, cam, &target))
                .collect());
            let cov = lit_ratio(&skinned_persist_once(
                r, &mesh, &mat, &skeletons, palettes, cam, &target,
            ));
            (m, cov)
        }
        Axis::Persist | Axis::PersistSubmit | Axis::PersistAsync => {
            let target = r.create_target(cam.width, cam.height).expect("target");
            with_persist(r, n, |list| {
                let one = || match axis {
                    Axis::Persist => persist_us(r, list, cam, &target, true),
                    Axis::PersistSubmit => persist_us(r, list, cam, &target, false),
                    _ => pipelined_us(r, list, cam, &target),
                };
                for _ in 0..5 {
                    one();
                }
                let m = med((0..25).map(|_| one()).collect());
                r.render_draw_list(list, cam, &target)
                    .expect("persist render must succeed");
                (
                    m,
                    lit_ratio(&r.read_target(&target).expect("readback must succeed")),
                )
            })
        }
    }
}

/// 与 `gpu.rs::new` 相同的实例构造与适配器选择，保证标注的就是被测的那个。
fn adapter_label() -> String {
    let backends = match std::env::var("WGPU_BACKEND").as_deref().map(str::trim) {
        Ok("vulkan") => wgpu::Backends::VULKAN,
        Ok("gl") | Ok("opengl") => wgpu::Backends::GL,
        _ => wgpu::Backends::PRIMARY,
    };
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        match wgpu::util::initialize_adapter_from_env_or_default(&instance, None).await {
            Ok(adapter) => {
                let info = adapter.get_info();
                format!(
                    "{} / {:?} / backend {:?}",
                    info.name, info.device_type, info.backend
                )
            }
            Err(_) => "<no adapter>".into(),
        }
    })
}

fn bench_gpu_frame(c: &mut Criterion) {
    let Ok(renderer) = HeadlessRenderer::new() else {
        eprintln!("gpu_frame: no usable adapter; skipping (set WGPU_BACKEND=gl for mesa)");
        return;
    };

    if std::env::args().any(|a| a == "breakdown") {
        breakdown(&renderer);
    }

    let mut g = c.benchmark_group("gpu_frame");
    g.sample_size(10);
    for (w, h) in RESOLUTIONS {
        let cam = camera(w, h);
        let mut meshes = Vec::new();
        for &(label, axis, ns) in &AXES {
            for &n in ns {
                let id = BenchmarkId::new(format!("{label}/{w}x{h}"), n);
                match axis {
                    Axis::Draw | Axis::Fill => {
                        let spread = matches!(axis, Axis::Draw);
                        g.bench_with_input(id, &n, |b, _| {
                            b.iter(|| black_box(frame_us(&renderer, &mut meshes, n, spread, &cam)))
                        });
                    }
                    Axis::SkinDraw => {
                        let sb = SkinBench::new(n);
                        g.bench_with_input(id, &n, |b, _| {
                            b.iter(|| black_box(sb.snapshot_us(&renderer, n, &cam)))
                        });
                    }
                    Axis::Skin => {
                        let target = renderer.create_target(w, h).expect("target");
                        let sb = SkinBench::new(n);
                        let mesh = renderer
                            .upload_skinned_mesh(
                                &sb.positions,
                                Some(&sb.normals),
                                None,
                                &sb.joints,
                                &sb.weights,
                                &sb.indices,
                            )
                            .expect("upload_skinned_mesh");
                        let mat =
                            renderer.create_material([0.6, 0.6, 0.62, 1.0], 0.0, 0.8, None, None);
                        let skeletons: Vec<GpuSkeleton> =
                            (0..n).map(|_| renderer.create_skeleton()).collect();
                        let palettes = &sb.palettes;
                        g.bench_with_input(id, &n, |b, _| {
                            b.iter(|| {
                                black_box(skinned_persist_us(
                                    &renderer, &mesh, &mat, &skeletons, palettes, &cam, &target,
                                ))
                            })
                        });
                    }
                    Axis::Persist | Axis::PersistSubmit | Axis::PersistAsync => {
                        let target = renderer.create_target(w, h).expect("target");
                        with_persist(&renderer, n, |list| {
                            g.bench_with_input(id, &n, |b, _| {
                                b.iter(|| {
                                    black_box(match axis {
                                        Axis::Persist => {
                                            persist_us(&renderer, list, &cam, &target, true)
                                        }
                                        Axis::PersistSubmit => {
                                            persist_us(&renderer, list, &cam, &target, false)
                                        }
                                        _ => pipelined_us(&renderer, list, &cam, &target),
                                    })
                                })
                            });
                        });
                    }
                }
            }
        }
    }
    g.finish();
}

/// 逐档打印：每帧 µs、占预算、相对上一档的边际 µs/单位、像素覆盖率。
fn breakdown(r: &HeadlessRenderer) {
    println!("\n══════ GPU 帧成本（中位数，µs）══════");
    println!("适配器：{}", adapter_label());
    println!("帧预算 60fps = {FRAME_BUDGET_US:.0} µs\n");
    println!(
        "{:<18} {:>6} {:>12} {:>8} {:>10} {:>9}",
        "场景", "parts", "µs/帧", "占预算", "边际µs/单位", "覆盖率"
    );
    let mut meshes = Vec::new();
    let med = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    for (w, h) in RESOLUTIONS {
        let cam = camera(w, h);
        for &(label, axis, ns) in &AXES {
            // 与上一档的差值：draw / persist 轴是每个绘制单元的边际成本，
            // fill 轴是每倍 overdraw 的。首档没有上一档；固定底噪（清屏 + 回读）
            // 无法单独量，见第23刀。
            let mut prev: Option<(usize, f64)> = None;
            for &n in ns {
                let (m, cov) = measure(r, axis, &mut meshes, n, &cam, &med);
                let marginal = match prev {
                    None => "-".to_string(),
                    Some((pn, pm)) => format!("{:.1}", (m - pm) / (n - pn) as f64),
                };
                prev = Some((n, m));
                println!(
                    "{:<18} {:>6} {:>12.0} {:>7.1}% {:>10} {:>8.1}%",
                    format!("{label}/{w}x{h}"),
                    n,
                    m,
                    m / FRAME_BUDGET_US * 100.0,
                    marginal,
                    cov * 100.0
                );
            }
        }
    }
    println!(
        "\npersist = render_draw_list + 显式回读，与 draw 同口径（都等到 GPU 出像素）；\n\
         psubmit = 只提交不回读，GPU 侧尚未完成，不含光栅化时间；\n\
         pasync  = 提交 + 异步回读，两帧在途流水、8 帧均摊。与 persist 同为\n\
                   “产出一帧带像素的画面要多久”，差值就是流水的吞吐差；代价是结果晚\n\
                   两帧到手。"
    );
    // 设备类型取自适配器信息；软件光栅化的读数不能当 GPU 帧时间对外引用。
    if adapter_label().contains("/ Cpu /") {
        println!(
            "\n注：本机适配器为软件光栅化（device_type=Cpu）。以上读数**不是 GPU 帧时间**，\n\
             只作同机回归；换真卡后需重跑。"
        );
    }
}

criterion_group!(benches, bench_gpu_frame);
criterion_main!(benches);

// ─── 蒙皮两轴 ────────────────────────────────────────────────────────────────
//
// skindraw = 快照路径 `render_skinned_views`:每帧每个部件现建顶点/索引/关节
//            缓冲并阻塞回读。QY-Demo 里那些人形现在走的就是这条。
// skin     = 持久路径:网格上传一次、每个实例一个骨架、面板每帧更新。
//
// 两边都是"24 顶点方块全绑关节 0",面板按实例平移铺开,差异只在提交路径。

/// 蒙皮基准的共享数据:一套顶点 + 每个实例一个单关节面板。
struct SkinBench {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
    joints: Vec<[u32; 4]>,
    weights: Vec<[f32; 4]>,
    /// 每个实例的面板(单关节,平移铺开)。
    palettes: Vec<[[f32; 4]; 4]>,
}

impl SkinBench {
    fn new(n: usize) -> Self {
        let cube = unit_cube();
        let side = (n as f32).sqrt().ceil().max(1.0);
        let half_h = 90.0 * 30f32.to_radians().tan() * 0.95;
        let cell = 2.0 * half_h * ASPECT / side;
        let half = cell * 0.45;
        let joints = vec![[0u32, 0, 0, 0]; cube.positions.len()];
        let weights = vec![[1.0f32, 0.0, 0.0, 0.0]; cube.positions.len()];
        let mut palettes = Vec::with_capacity(n);
        for i in 0..n {
            let dx = ((i as f32 % side) + 0.5 - side / 2.0) * cell;
            let dy = ((i as f32 / side) + 0.5 - side / 2.0) * cell;
            palettes.push([
                [half, 0.0, 0.0, dx],
                [0.0, half, 0.0, dy],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]);
        }
        Self {
            positions: cube.positions,
            normals: cube.normals,
            indices: cube.indices,
            joints,
            weights,
            palettes,
        }
    }

    /// 快照蒙皮路径渲染一次。每个部件现拼 `SkinnedPart`,调用方看不到的
    /// 缓冲在函数内建好又销毁——这就是这条路径的真实形态。
    fn snapshot_once(&self, r: &HeadlessRenderer, n: usize, cam: &Camera) -> RenderedImage {
        let parts: Vec<SkinnedPart<'_>> = (0..n)
            .map(|i| SkinnedPart {
                positions: &self.positions,
                normals: Some(&self.normals),
                uvs: None,
                indices: &self.indices,
                base_color: [0.6, 0.6, 0.62, 1.0],
                metallic: 0.0,
                roughness: 0.8,
                base_texture: None,
                mr_texture: None,
                joints: &self.joints,
                weights: &self.weights,
                palette: &self.palettes[i..i + 1],
            })
            .collect();
        r.render_skinned_views(&parts, cam)
            .expect("snapshot skinned render must succeed")
    }

    fn snapshot_us(&self, r: &HeadlessRenderer, n: usize, cam: &Camera) -> f64 {
        let t = std::time::Instant::now();
        let img = self.snapshot_once(r, n, cam);
        black_box(&img.bgra);
        t.elapsed().as_secs_f64() * 1e6
    }
}

/// 持久蒙皮路径渲染一次:面板每帧更新(与真实帧循环同形),提交后显式回读。
fn skinned_persist_once(
    r: &HeadlessRenderer,
    mesh: &GpuSkinnedMesh,
    mat: &GpuMaterial,
    skeletons: &[GpuSkeleton],
    palettes: &[[[f32; 4]; 4]],
    cam: &Camera,
    target: &RenderTarget,
) -> RenderedImage {
    let mut list = SkinnedDrawList::new();
    for (i, skel) in skeletons.iter().enumerate() {
        r.update_skeleton(skel, &palettes[i..i + 1])
            .expect("update_skeleton must succeed");
        list.push(mesh, mat, skel);
    }
    let stats = r
        .render_frame(&DrawList::new(), &list, cam, target)
        .expect("skinned persist render must succeed");
    black_box(stats.skinned_draws);
    r.read_target(target).expect("readback must succeed")
}

fn skinned_persist_us(
    r: &HeadlessRenderer,
    mesh: &GpuSkinnedMesh,
    mat: &GpuMaterial,
    skeletons: &[GpuSkeleton],
    palettes: &[[[f32; 4]; 4]],
    cam: &Camera,
    target: &RenderTarget,
) -> f64 {
    let t = std::time::Instant::now();
    let img = skinned_persist_once(r, mesh, mat, skeletons, palettes, cam, target);
    black_box(&img.bgra);
    t.elapsed().as_secs_f64() * 1e6
}
