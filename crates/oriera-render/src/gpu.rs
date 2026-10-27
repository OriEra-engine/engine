//! L5 wgpu 最小闭环(`render-wgpu` feature,默认关)。
//!
//! 测评共识 P0「渲染层是空的」的第一刀:**真 GPU pass、真 mesh、真像素出图**。
//! 设计边界(诚实清单):
//! - 无头(headless)渲染到纹理,回读 BGRA 像素——不接窗口系统,不进帧调度;
//! - 单管线、单顶点流(POSITION f32×3)、无深度缓冲、无光照(位置渐变着色自证三角形);
//! - 输入网格在 CPU 侧归一化到裁剪空间,shader 不含矩阵运算;
//! - 位置:windows/linux 原生验证;Web(wasm)是 wgpu 的既有能力,接入排在路线图。
//!
//! 用法见 `examples/glb_headless.rs`(GLB → 解码 → 渲染 → BMP)。

use std::fmt;
use std::sync::mpsc;
use wgpu::util::DeviceExt;

/// 一帧无头渲染结果:紧密排列的 BGRA8 像素(行无填充)。
#[derive(Debug, Clone)]
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl RenderedImage {
    /// 取 (x, y) 像素的 BGRA。坐标越界返回背景哨兵。
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.bgra[i],
            self.bgra[i + 1],
            self.bgra[i + 2],
            self.bgra[i + 3],
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    /// 本机找不到可用 GPU 适配器(虚拟机/CI 常见)。
    NoAdapter,
    /// 适配器存在但设备初始化失败。
    Device(String),
    /// 回读映射失败。
    Map(String),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::NoAdapter => write!(f, "no GPU adapter available"),
            GpuError::Device(e) => write!(f, "device init failed: {e}"),
            GpuError::Map(e) => write!(f, "readback map failed: {e}"),
        }
    }
}

impl std::error::Error for GpuError {}

const SHADER: &str = r#"
struct VSOut {
    @builtin(position) pos: vec4f,
    @location(0) color: vec4f,
};

// 顶点已在裁剪空间(CPU 侧归一化),颜色同样 CPU 烘焙:
// 无材质时复刻旧位置渐变;有材质时为 baseColor × 兰伯特顶点光照。
@vertex
fn vs(@location(0) p: vec3f, @location(1) c: vec4f) -> VSOut {
    var out: VSOut;
    out.pos = vec4f(p, 1.0);
    out.color = c;
    return out;
}

@fragment
fn fs(in: VSOut) -> @location(0) vec4f {
    return in.color;
}
"#;

/// 烘焙顶点(裁剪坐标 + 颜色)。12+16=28 字节,对齐 4。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadedVertex {
    pos: [f32; 3],
    color: [f32; 4],
}

/// 固定主光方向(世界感稳定的「左上前方」灯)。
const LIGHT: [f32; 3] = [0.45, 0.55, 0.70];

/// CPU 烘焙每顶点颜色。
/// - 无材质:复刻旧版位置渐变(同公式,兼容旧出图观感);
/// - 有材质:baseColor × (0.35 环境 + 0.65 兰伯特),法线缺失时
///   以 (0,0,1) 朝向镜头(等比缩放不改变法线方向)。
fn bake_vertices(
    clip: &[[f32; 3]],
    normals: Option<&[[f32; 3]]>,
    base_color: Option<[f32; 4]>,
) -> Vec<ShadedVertex> {
    let grad = |p: [f32; 3]| {
        [
            p[0] * 0.5 + 0.5,
            p[1] * 0.5 + 0.5,
            0.3 + 0.5 * (p[2] * 0.5 + 0.5),
            1.0,
        ]
    };
    match base_color {
        None => clip
            .iter()
            .map(|p| ShadedVertex {
                pos: *p,
                color: grad(*p),
            })
            .collect(),
        Some(base) => {
            let ln = (LIGHT[0] * LIGHT[0] + LIGHT[1] * LIGHT[1] + LIGHT[2] * LIGHT[2]).sqrt();
            clip.iter()
                .enumerate()
                .map(|(i, p)| {
                    let n = normals
                        .and_then(|ns| ns.get(i).copied())
                        .unwrap_or([0.0, 0.0, 1.0]);
                    let nn = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                    let diffuse = if nn > f32::EPSILON {
                        ((n[0] * LIGHT[0] + n[1] * LIGHT[1] + n[2] * LIGHT[2]) / (nn * ln)).max(0.0)
                    } else {
                        0.0
                    };
                    let shade = 0.35 + 0.65 * diffuse;
                    ShadedVertex {
                        pos: *p,
                        color: [base[0] * shade, base[1] * shade, base[2] * shade, base[3]],
                    }
                })
                .collect()
        }
    }
}

/// 无头渲染器:持有一套 device/queue/pipeline,可反复渲染网格。
pub struct HeadlessRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    adapter_info: String,
}

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.05,
    g: 0.08,
    b: 0.13,
    a: 1.0,
};

/// 默认后端:MinGW(windows-gnu) 下 DX12/Vulkan 的适配器枚举在
/// wgpu-hal 实测崩溃(30.0.1,2026-10 本机验证),GL 稳定;MSVC 与其他
/// 平台用 PRIMARY。可用环境变量 `WGPU_BACKEND` 覆盖(vulkan/dx12/gl/metal/all)。
#[cfg(all(windows, target_env = "gnu"))]
const DEFAULT_BACKENDS: wgpu::Backends = wgpu::Backends::GL;
#[cfg(not(all(windows, target_env = "gnu")))]
const DEFAULT_BACKENDS: wgpu::Backends = wgpu::Backends::PRIMARY;

fn backends_from_env() -> wgpu::Backends {
    match std::env::var("WGPU_BACKEND").as_deref().map(str::trim) {
        Ok("vulkan") => wgpu::Backends::VULKAN,
        Ok("dx12") | Ok("dx12_native") => wgpu::Backends::DX12,
        Ok("gl") | Ok("opengl") => wgpu::Backends::GL,
        Ok("metal") => wgpu::Backends::METAL,
        Ok("all") => wgpu::Backends::all(),
        _ => DEFAULT_BACKENDS,
    }
}

impl HeadlessRenderer {
    /// 初始化 wgpu 设备(阻塞式;内部异步由 pollster 驱动)。
    pub fn new() -> Result<Self, GpuError> {
        pollster::block_on(async {
            // wgpu 30:InstanceDescriptor 无 Default,字段全量显式;后端按策略选取
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: backends_from_env(),
                flags: wgpu::InstanceFlags::default(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                backend_options: wgpu::BackendOptions::default(),
                display: None,
            });
            // 官方助手:支持 WGPU_ADAPTER_NAME 环境变量覆盖,返回 Result
            let adapter = wgpu::util::initialize_adapter_from_env_or_default(&instance, None)
                .await
                .map_err(|e| GpuError::Device(format!("no usable adapter: {e}")))?;
            let info = adapter.get_info();
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .map_err(|e| GpuError::Device(format!("{e}")))?;

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("oriera-minimal"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("oriera-minimal-pipeline"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 28,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x3,
                            },
                            wgpu::VertexAttribute {
                                offset: 12,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                        ],
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Bgra8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

            Ok(HeadlessRenderer {
                device,
                queue,
                pipeline,
                adapter_info: format!("{} ({})", info.name, info.backend),
            })
        })
    }

    pub fn adapter_info(&self) -> &str {
        &self.adapter_info
    }

    /// 渲染一个网格并回读像素。顶点在 CPU 侧归一化到裁剪空间(保纵横比、居中、0.9 缩放)。
    /// `indices` 为空则按顶点序直接绘制。
    pub fn render_mesh(
        &self,
        positions: &[[f32; 3]],
        indices: &[u32],
        width: u32,
        height: u32,
    ) -> Result<RenderedImage, GpuError> {
        self.render_mesh_shaded(positions, None, None, indices, width, height)
    }

    /// 带材质的渲染:法线 + 基础色走 CPU 烘焙的兰伯特顶点光照;
    /// 两者皆 None 时与 [`HeadlessRenderer::render_mesh`] 完全一致。
    pub fn render_mesh_shaded(
        &self,
        positions: &[[f32; 3]],
        normals: Option<&[[f32; 3]]>,
        base_color: Option<[f32; 4]>,
        indices: &[u32],
        width: u32,
        height: u32,
    ) -> Result<RenderedImage, GpuError> {
        if positions.is_empty() || width == 0 || height == 0 {
            return Err(GpuError::Device("empty mesh or zero-sized target".into()));
        }
        let clip = normalize_to_clip(positions);
        let verts = bake_vertices(&clip, normals, base_color);

        let vertex_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vertices"),
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("oriera-minimal-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, vertex_buf.slice(..));
            match indices.split_first() {
                Some(_) => {
                    let index_buf =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("indices"),
                                contents: bytemuck::cast_slice(indices),
                                usage: wgpu::BufferUsages::INDEX,
                            });
                    pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
                }
                None => pass.draw(0..positions.len() as u32, 0..1),
            }
        }

        // 回读:wgpu 要求 bytes_per_row 对齐 256,读出后去填充
        let stride = (width * 4) as usize;
        let padded = stride.div_ceil(256) * 256;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * height as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let (tx, rx) = mpsc::channel();
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| GpuError::Map(format!("{e}")))?;
        rx.recv()
            .map_err(|e| GpuError::Map(format!("map callback lost: {e}")))?
            .map_err(|e| GpuError::Map(format!("{e}")))?;

        let data = slice
            .get_mapped_range()
            .map_err(|e| GpuError::Map(format!("{e}")))?;
        let mut bgra = Vec::with_capacity(stride * height as usize);
        for row in 0..height as usize {
            let start = row * padded;
            bgra.extend_from_slice(&data[start..start + stride]);
        }
        drop(data);
        readback.unmap();

        Ok(RenderedImage {
            width,
            height,
            bgra,
        })
    }
}

/// 包围盒归一化:中心化 + 等比缩放到 [-0.9, 0.9](xy 平面),z 保留原始相对深度供着色。
fn normalize_to_clip(positions: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in positions {
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let extent = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    // 等比缩放取「最大」非零维度映射到 ±0.9(取最小会让最宽轴溢出
    // 裁剪空间 —— Blobfish 各维近似等长时侥幸不显,扁四边形必炸)。
    // 全退化(所有点同位)→ scale 0,收缩到中心。
    let max_extent = extent
        .iter()
        .filter(|e| **e > f32::EPSILON)
        .fold(0.0f32, |a, e| a.max(*e));
    let scale = if max_extent > 0.0 {
        1.8 / max_extent
    } else {
        0.0
    };
    positions
        .iter()
        .map(|p| {
            [
                (p[0] - center[0]) * scale,
                (p[1] - center[1]) * scale,
                (p[2] - center[2]) * scale,
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bake_lambert_follows_the_light() {
        let clip = [[0.0, 0.0, 0.0]; 3];
        // 顺光面亮、背光面只剩环境光、缺法线按朝镜头算
        let normals = Some(&[[0.0f32, 0.0, 1.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]][..]);
        let base = [0.5f32, 0.5, 0.5, 1.0];
        let v = bake_vertices(&clip, normals, Some(base));
        let ln = 0.45f32.hypot(0.55).hypot(0.70);
        let facing = (0.70f32 / ln).max(0.0); // (0,0,1)·L/|L|
        let side = (0.45f32 / ln).max(0.0); // (1,0,0)·L/|L|
        assert!((v[0].color[0] - 0.5 * (0.35 + 0.65 * facing)).abs() < 1e-5);
        assert!((v[1].color[0] - 0.5 * 0.35).abs() < 1e-5, "背光只余环境光");
        assert!((v[2].color[0] - 0.5 * (0.35 + 0.65 * side)).abs() < 1e-5);
        // 无法线 → 全部按 (0,0,1)
        let vn = bake_vertices(&clip, None, Some(base));
        assert!((vn[1].color[0] - vn[0].color[0]).abs() < 1e-6);
    }

    #[test]
    fn bake_gradient_matches_legacy_formula() {
        let clip = [[-0.9f32, -0.2, 0.4], [0.3, 0.8, -0.5], [0.0, 0.0, 0.0]];
        let v = bake_vertices(&clip, None, None);
        for (i, p) in clip.iter().enumerate() {
            let want = [
                p[0] * 0.5 + 0.5,
                p[1] * 0.5 + 0.5,
                0.3 + 0.5 * (p[2] * 0.5 + 0.5),
                1.0,
            ];
            for c in 0..4 {
                assert!((v[i].color[c] - want[c]).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn clip_normalization_keeps_aspect_and_center() {
        // 宽 2、高 1 的四边形 → 归一化后 x∈[-0.9,0.9], y∈[-0.45,0.45]
        let quad = [
            [-1.0, -0.5, 0.0],
            [1.0, -0.5, 0.0],
            [1.0, 0.5, 0.0],
            [-1.0, 0.5, 0.0],
        ];
        let n = normalize_to_clip(&quad);
        let xs: Vec<f32> = n.iter().map(|p| p[0].abs()).collect();
        let ys: Vec<f32> = n.iter().map(|p| p[1].abs()).collect();
        assert!((xs.iter().cloned().fold(0.0f32, f32::max) - 0.9).abs() < 1e-4);
        assert!((ys.iter().cloned().fold(0.0f32, f32::max) - 0.45).abs() < 1e-4);
        // 全部同点(退化)不 panic,收缩到中心
        let degenerate = normalize_to_clip(&[[0.0, 0.0, 0.0]; 3]);
        assert!(degenerate.iter().all(|p| p.iter().all(|c| c.abs() < 1e-6)));
    }

    /// 需要本机 GPU;CI 无适配器故默认忽略。
    /// 本地运行:`cargo test -p oriera-render --features render-wgpu -- --ignored`
    #[test]
    #[ignore = "requires a local GPU adapter (none in CI)"]
    fn renders_triangle_headless() {
        let r = HeadlessRenderer::new().expect("adapter");
        let img = r
            .render_mesh(
                &[[-1.0, -0.5, 0.0], [1.0, -0.5, 0.0], [0.0, 0.8, 0.0]],
                &[0, 1, 2],
                64,
                64,
            )
            .expect("render");
        let bg = [33, 20, 13, 255]; // CLEAR (r=0.05,g=0.08,b=0.13) 的 BGRA 字节(四舍五入)
        assert_eq!(img.pixel(2, 2), bg, "corner stays background");
        let mid = img.pixel(32, 32); // 三角形重心附近
        assert_ne!(mid, bg, "triangle center must be rasterized: {mid:?}");
    }
}
