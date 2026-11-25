//! L5 GPU 帧成本微基准 —— `HeadlessRenderer::render_scene_views`。
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
use oriera_render::gpu::{HeadlessRenderer, ScenePart};

const FRAME_BUDGET_US: f64 = 16_667.0;
/// 两档分辨率的宽高比相同（约 16:9），网格按它铺满。
const ASPECT: f32 = 16.0 / 9.0;
/// QY-Demo 的分辨率，以及常见的 1080p。
const RESOLUTIONS: [(u32, u32); 2] = [(966, 544), (1920, 1080)];
/// 两根轴与各自的档位；criterion 主体与 breakdown 共用，避免两边口径不一致。
/// fill 轴全屏 overdraw 在软件光栅化下每倍约 1–4 ms，档位压到 64 以内保证单帧可测。
const AXES: [(&str, bool, &[usize]); 2] = [
    ("draw", true, &[1, 64, 512, 2048]),
    ("fill", false, &[4, 16, 64]),
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

/// 非背景像素占比：确认场景真的被画出来了，而不是整张空图。
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
    let px = |i: usize| -> [u8; 4] {
        [
            img.bgra[i],
            img.bgra[i + 1],
            img.bgra[i + 2],
            img.bgra[i + 3],
        ]
    };
    let bg = px(0);
    let lit = (0..img.bgra.len() / 4).filter(|&i| px(i * 4) != bg).count();
    lit as f64 / (img.bgra.len() / 4) as f64
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
        for &(label, spread, ns) in &AXES {
            for &n in ns {
                g.bench_with_input(
                    BenchmarkId::new(format!("{label}/{w}x{h}"), n),
                    &n,
                    |b, &n| b.iter(|| black_box(frame_us(&renderer, &mut meshes, n, spread, &cam))),
                );
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
        "{:<10} {:>6} {:>12} {:>8} {:>10} {:>9}",
        "场景", "parts", "µs/帧", "占预算", "边际µs/单位", "覆盖率"
    );
    let mut meshes = Vec::new();
    let med = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    for (w, h) in RESOLUTIONS {
        let cam = camera(w, h);
        for &(label, spread, ns) in &AXES {
            // 与上一档的差值：draw 轴是每个绘制单元的边际成本，fill 轴是每倍 overdraw 的。
            // 首档没有上一档，固定底噪（建目标 + 清屏 + 回读）无法单独量，见第23刀。
            let mut prev: Option<(usize, f64)> = None;
            for &n in ns {
                for _ in 0..5 {
                    frame_us(r, &mut meshes, n, spread, &cam);
                }
                let m = med((0..25)
                    .map(|_| frame_us(r, &mut meshes, n, spread, &cam))
                    .collect());
                let cov = coverage(r, &mut meshes, n, spread, &cam);
                let marginal = match prev {
                    None => "-".to_string(),
                    Some((pn, pm)) => format!("{:.1}", (m - pm) / (n - pn) as f64),
                };
                prev = Some((n, m));
                println!(
                    "{:<10} {:>6} {:>12.0} {:>7.1}% {:>10} {:>8.1}%",
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
