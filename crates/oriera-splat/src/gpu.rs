//! PLAN-splat v2 M1（`splat-wgpu` feature，默认关）：GPU 泼溅光栅化。
//!
//! 与 CPU 参考（[`crate::raster`]）的分工：**确定性数学全部留在 CPU**——
//! EWA 投影（[`crate::project::project`]）与深度排序（[`crate::sort::depth_order`]）
//! 原样复用；GPU 只做两件事：把投影后的 2D 高斯展开成实例化四边形、按
//! **后到前 over**（SrcAlpha / OneMinusSrcAlpha）混合。α 钳 [`crate::raster::ALPHA_MAX`]、
//! α < [`crate::project::ALPHA_MIN`] 丢弃，与 CPU 同规则——输出是「同数学、
//! 不同执行器」，差异只有浮点运算顺序的舍入。互证门槛（评审定）：≥99.9% 像素
//! 每通道差 ≤2/255 且全图 RMS ≤1/255（见 `gpu_matches_cpu_reference_within_review_gate`）。
//!
//! 相机消费 #152 共享底座：[`oriera_render::camera::Camera`]（GL 约定）经
//! [`SplatCamera::from_view`](crate::camera::SplatCamera::from_view) 换算。
//! 无深度缓冲参与（排序+混合已保证遮挡语义）；不读也不置 `NEURAL_RENDER_3DGS`
//! （经典泼溅 = ADR 0002 passthrough 侧）。无头设备/回读惯例与 oriera-render
//! 一致：windows-gnu 钳 GL、`WGPU_BACKEND` 可覆盖、256 对齐回读。

use crate::camera::SplatCamera;
use crate::project;
use crate::raster::SplatImage;
use crate::sort::depth_order;
use oriera_asset::ply::SplatCloud;
use std::fmt;
use std::sync::mpsc;
use wgpu::util::DeviceExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    /// 本机找不到可用 GPU 适配器(虚拟机/CI 常见)。
    NoAdapter,
    /// 设备初始化失败或非法输入。
    Device(String),
    /// 回读映射失败。
    Map(String),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::NoAdapter => write!(f, "no GPU adapter available"),
            GpuError::Device(e) => write!(f, "device/init failed: {e}"),
            GpuError::Map(e) => write!(f, "readback map failed: {e}"),
        }
    }
}

impl std::error::Error for GpuError {}

const SHADER: &str = r#"
struct VSOut {
    @builtin(position) pos: vec4f,
    @location(0) local: vec2f,
    @location(1) conic: vec3f,
    @location(2) color: vec3f,
    @location(3) opacity: f32,
    @location(4) extent_px: vec2f,
};

// corner ∈ [-1,1]²(四角),center_ndc/extent_ndc 来自实例。
@vertex
fn vs(
    @location(0) corner: vec2f,
    @location(1) center_ndc: vec2f,
    @location(2) extent_ndc: vec2f,
    @location(3) extent_px: vec2f,
    @location(4) conic: vec3f,
    @location(5) color: vec3f,
    @location(6) opacity: f32,
) -> VSOut {
    var out: VSOut;
    out.pos = vec4f(center_ndc + corner * extent_ndc, 0.5, 1.0);
    out.local = corner;
    out.conic = conic;
    out.color = color;
    out.opacity = opacity;
    out.extent_px = extent_px;
    return out;
}

// 与 CPU 参考同一公式:power = -½(a·dx² + c·dy²) - b·dx·dy,d 以像素计。
// local.y 是 NDC 上行坐标,conic 公式的 dy 是像素下行坐标——**取反对齐**;
// 倾斜 splat(交叉项 b≠0)对这个符号敏感,轴对齐探针(b=0)对 ±dy 对称测不出。
@fragment
fn fs(in: VSOut) -> @location(0) vec4f {
    let d = vec2f(in.local.x, -in.local.y) * in.extent_px;
    let power =
        -0.5 * (in.conic.x * d.x * d.x + in.conic.z * d.y * d.y) - in.conic.y * d.x * d.y;
    let a = min(0.99, in.opacity * exp(power));
    if (a < 0.003921569) { // 1/255:8 位输出不可见,与 CPU 参考同规则
        discard;
    }
    return vec4f(in.color, a);
}
"#;

/// 顶点四角(局部坐标),索引 0,1,2,0,2,3 拼两枚三角形。
const CORNERS: [[f32; 2]; 4] = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
const CORNER_INDICES: [u32; 6] = [0, 1, 2, 0, 2, 3];

/// 每 splat 实例数据(13 个 f32 = 52 字节)。center/extent_ndc 供顶点展开;
/// extent_px/conic/color/opacity 供片元求值。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    center_ndc: [f32; 2],
    extent_ndc: [f32; 2],
    extent_px: [f32; 2],
    conic: [f32; 3],
    color: [f32; 3],
    opacity: f32,
}

/// 投影结果 → GPU 实例:像素 → NDC 用与 CPU 光栅同一约定(像素 i 覆盖
/// 连续区间 [i, i+1),中心 i+0.5;NDC ±1 对应视口两外缘),保证片元里的
/// `d = local * extent_px` 与 CPU 的 `dx = px + 0.5 - mean` 逐采样一致。
fn build_instances(
    projected: &[project::ProjectedSplat],
    order: &[u32],
    camera: &SplatCamera,
) -> Vec<Instance> {
    let (w, h) = (camera.width as f32, camera.height as f32);
    order
        .iter()
        .rev() // 前到背序 → 后到前绘制(over 混合)
        .map(|&slot| {
            let p = &projected[slot as usize];
            Instance {
                center_ndc: [p.mean[0] / w * 2.0 - 1.0, 1.0 - p.mean[1] / h * 2.0],
                extent_ndc: [p.extent[0] * 2.0 / w, p.extent[1] * 2.0 / h],
                extent_px: p.extent,
                conic: p.conic,
                color: p.color,
                opacity: p.opacity,
            }
        })
        .collect()
}

/// 无头泼溅渲染器:持有一套 device/queue/pipeline,可反复渲染。
pub struct SplatRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    adapter_info: String,
}

/// 默认后端:与 oriera-render 同一策略(windows-gnu 实测 DX12/Vulkan 枚举崩溃 → GL)。
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

impl SplatRenderer {
    /// 初始化 wgpu 设备(阻塞式;内部异步由 pollster 驱动)。
    pub fn new() -> Result<Self, GpuError> {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: backends_from_env(),
                flags: wgpu::InstanceFlags::default(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                backend_options: wgpu::BackendOptions::default(),
                display: None,
            });
            let adapter = wgpu::util::initialize_adapter_from_env_or_default(&instance, None)
                .await
                .map_err(|e| GpuError::Device(format!("no usable adapter: {e}")))?;
            let info = adapter.get_info();
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .map_err(|e| GpuError::Device(format!("{e}")))?;

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("oriera-splat"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("oriera-splat-pipeline"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[
                        Some(wgpu::VertexBufferLayout {
                            array_stride: 8,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &[wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x2,
                            }],
                        }),
                        Some(wgpu::VertexBufferLayout {
                            array_stride: 52,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes: &[
                                wgpu::VertexAttribute {
                                    offset: 0,
                                    shader_location: 1,
                                    format: wgpu::VertexFormat::Float32x2,
                                },
                                wgpu::VertexAttribute {
                                    offset: 8,
                                    shader_location: 2,
                                    format: wgpu::VertexFormat::Float32x2,
                                },
                                wgpu::VertexAttribute {
                                    offset: 16,
                                    shader_location: 3,
                                    format: wgpu::VertexFormat::Float32x2,
                                },
                                wgpu::VertexAttribute {
                                    offset: 24,
                                    shader_location: 4,
                                    format: wgpu::VertexFormat::Float32x3,
                                },
                                wgpu::VertexAttribute {
                                    offset: 36,
                                    shader_location: 5,
                                    format: wgpu::VertexFormat::Float32x3,
                                },
                                wgpu::VertexAttribute {
                                    offset: 48,
                                    shader_location: 6,
                                    format: wgpu::VertexFormat::Float32,
                                },
                            ],
                        }),
                    ],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        // 与渲染附件一致:浮点目标(见 texture 处注释)
                        format: wgpu::TextureFormat::Rgba32Float,
                        // 后到前 over:color = SrcAlpha·c + (1-SrcAlpha)·dst,
                        // alpha = 1·a + (1-a)·dst_a(覆盖率的 1-Π(1-α) 累积)。
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::SrcAlpha,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

            Ok(SplatRenderer {
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

    /// 渲染一朵云:CPU 投影+排序 → GPU 实例化四边形后到前 over 合成。
    /// 输出 [`SplatImage`](crate::raster::SplatImage)(RGBA8,与 CPU 参考同型,
    /// 可直接互证/fingerprint)。
    pub fn render(
        &self,
        cloud: &SplatCloud,
        camera: &SplatCamera,
        background: [f32; 3],
    ) -> Result<SplatImage, GpuError> {
        let (width, height) = (camera.width, camera.height);
        if width == 0 || height == 0 {
            return Err(GpuError::Device("empty viewport".into()));
        }
        let projected = project::project(cloud, camera);
        let order = depth_order(&projected);
        let instances = build_instances(&projected, &order, camera);

        let corner_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("corners"),
                contents: bytemuck::cast_slice(&CORNERS),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("corner-indices"),
                contents: bytemuck::cast_slice(&CORNER_INDICES),
                usage: wgpu::BufferUsages::INDEX,
            });
        let instance_buf = (!instances.is_empty()).then(|| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("splat-instances"),
                    contents: bytemuck::cast_slice(&instances),
                    usage: wgpu::BufferUsages::VERTEX,
                })
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("splat-frame"),
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
            // 浮点目标:unorm8/fp16 的每层混合舍入会在几十层叠加的热点像素上
            // 累积突破 CPU 互证容差(实测 fp16 就差在这)——真 3DGS 渲染器同样用浮点目标。
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("oriera-splat-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(background[0]),
                            g: f64::from(background[1]),
                            b: f64::from(background[2]),
                            a: 0.0, // 覆盖率从 0 累积,与 CPU 的 1-T 对齐
                        }),
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
            pass.set_vertex_buffer(0, corner_buf.slice(..));
            // 整云被剔除(实例为空)时不建实例缓冲——零尺寸 buffer 的 slice 非法;
            // 此时 pass 只剩清屏,输出 = 纯背景,与 CPU 参考一致。
            if let Some(buf) = &instance_buf {
                pass.set_vertex_buffer(1, buf.slice(..));
                pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..CORNER_INDICES.len() as u32, 0, 0..instances.len() as u32);
            }
        }

        self.readback_pixels(encoder, &texture, width, height)
    }

    /// 提交 encoder、256 对齐回读 Rgba16Float、手写 f16→f32 解码、按 CPU 同式
    /// 量化成 [`SplatImage`](RGBA8)。
    fn readback_pixels(
        &self,
        encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<SplatImage, GpuError> {
        let stride = (width * 16) as usize; // Rgba32Float: 每像素 16 字节
        let padded = stride.div_ceil(256) * 256;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * height as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = encoder;
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
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
        let mut rgba = Vec::with_capacity(stride * height as usize / 2);
        let mut chan = [0f32; 4];
        for row in 0..height as usize {
            let start = row * padded;
            for px in data[start..start + stride].as_chunks::<16>().0 {
                for (k, word) in px.as_chunks::<4>().0.iter().enumerate() {
                    chan[k] = f32::from_le_bytes(*word);
                }
                // RGBA 直接就是通道序(非 BGRA);按 CPU 同式量化
                for v in chan {
                    let f = v.clamp(0.0, 1.0);
                    rgba.push((f * 255.0).round() as u8);
                }
            }
        }
        drop(data);
        readback.unmap();

        Ok(SplatImage {
            width,
            height,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::RasterOptions;
    use crate::scene;

    fn splat(center_px: [f32; 2], extent_px: [f32; 2]) -> project::ProjectedSplat {
        project::ProjectedSplat {
            index: 0,
            depth: 1.0,
            mean: center_px,
            conic: [1.0, 0.0, 1.0],
            extent: extent_px,
            color: [0.5; 3],
            opacity: 0.8,
        }
    }

    #[test]
    fn instance_centers_map_pixels_to_ndc_per_cpu_convention() {
        // 视口 64×48:像素 (32, 24) 是视口正中 → NDC (0,0);
        // extent 1px → 半宽 NDC = 2/64、半高 = 2/48。
        let cam = SplatCamera::look_at([0.0, 0.0, -4.0], [0.0; 3], [0.0, 1.0, 0.0], 60.0, 64, 48);
        let projected = [
            splat([32.0, 24.0], [1.0, 1.0]),
            splat([0.0, 0.0], [2.0, 4.0]),
        ];
        let order = [0u32, 1];
        let inst = build_instances(&projected, &order, &cam);
        assert_eq!(inst.len(), 2);
        // 后到前:rev 后 inst[0] 是槽位 1——像素 (0,0)(左上角外缘) → NDC (-1, +1)
        assert!((inst[0].center_ndc[0] + 1.0).abs() < 1e-6);
        assert!((inst[0].center_ndc[1] - 1.0).abs() < 1e-6);
        assert_eq!(inst[0].extent_px, [2.0, 4.0]);
        // inst[1] 是槽位 0:像素 (32, 24) 正中 → NDC (0,0),extent 1px → 2/64、2/48
        assert!((inst[1].center_ndc[0] - 0.0).abs() < 1e-6);
        assert!((inst[1].center_ndc[1] - 0.0).abs() < 1e-6);
        assert!((inst[1].extent_ndc[0] - 2.0 / 64.0).abs() < 1e-6);
        assert!((inst[1].extent_ndc[1] - 2.0 / 48.0).abs() < 1e-6);
    }

    /// 评审门槛互证(本机 GPU;CI 无适配器):同一朵云、同一视点
    /// (Camera 经 from_view 换算),CPU 参考与 GPU 出图必须达到
    /// ≥99.9% 像素每通道差 ≤2/255 且全图 RMS ≤1/255。
    #[test]
    #[ignore = "requires a local GPU adapter (none in CI)"]
    fn gpu_matches_cpu_reference_within_review_gate() {
        let cloud = scene::petal_ring(600, 2026);
        let gl = oriera_render::camera::Camera::look_at(
            [0.0, 2.4, -4.2],
            [0.0; 3],
            [0.0, 1.0, 0.0],
            45.0,
            64,
            48,
            0.1,
            100.0,
        );
        let cam = SplatCamera::from_view(&gl);
        let cpu = crate::raster::rasterize(
            &cloud,
            &cam,
            &RasterOptions {
                background: scene::BRAND_BACKGROUND,
                ..Default::default()
            },
        )
        .unwrap();
        let renderer = SplatRenderer::new().unwrap();
        let gpu = renderer
            .render(&cloud, &cam, scene::BRAND_BACKGROUND)
            .unwrap();
        assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height));

        let n = cpu.rgba.len();
        let mut viol = 0usize;
        let mut sq = 0f64;
        for (a, &b) in cpu.rgba.iter().zip(&gpu.rgba) {
            let d = i32::from(*a) - i32::from(b);
            if d.abs() > 2 {
                viol += 1;
            }
            sq += f64::from(d * d);
        }
        let rms = (sq / n as f64).sqrt();
        assert!(
            viol as f64 <= n as f64 * 0.001,
            "99.9% gate: {viol}/{n} channels exceed 2/255"
        );
        assert!(rms <= 1.0, "RMS gate: {rms:.3} > 1/255");
    }

    /// 交叉项回归(倾斜 splat,conic 交叉项 b≠0):曾因 fragment 里 local.y
    /// 的「NDC 上行 vs 像素下行」符号没对齐而差到 173/255——轴对齐 splat
    /// (b=0)对 ±dy 对称,正圆探针测不出来;倾斜的一测就现形。
    #[test]
    #[ignore = "requires a local GPU adapter (none in CI)"]
    fn tilted_splat_cross_term_matches_cpu() {
        let renderer = SplatRenderer::new().unwrap();
        let (cam, background) = stage();
        let half = std::f32::consts::PI / 8.0;
        let mut cloud = SplatCloud::default();
        cloud.positions.push([0.0, 0.0, 0.0]);
        cloud.scales.push([0.6, 0.1, 0.1]);
        cloud.rotations.push([0.0, 0.0, half.sin(), half.cos()]);
        cloud.opacities.push(0.82);
        cloud.sh_dc.push([1.4, -1.0, -1.4]);
        let (cpu, gpu) = render_both(&renderer, &cloud, &cam, background);
        assert!(worst_channel_delta(&cpu, &gpu) <= 2);
    }

    /// 单 splat 逐位一致:同一投影/排序 + fp32 混合目标下的直系推论。
    #[test]
    #[ignore = "requires a local GPU adapter (none in CI)"]
    fn single_splat_matches_cpu_exactly() {
        let renderer = SplatRenderer::new().unwrap();
        let (cam, background) = stage();
        let mut cloud = SplatCloud::default();
        cloud.positions.push([0.0, 0.0, 0.0]);
        cloud.scales.push([0.5, 0.35, 0.3]);
        cloud.rotations.push([0.0, 0.0, 0.0, 1.0]);
        cloud.opacities.push(0.82);
        cloud.sh_dc.push([1.4, -1.0, -1.4]);
        let (cpu, gpu) = render_both(&renderer, &cloud, &cam, background);
        assert_eq!(worst_channel_delta(&cpu, &gpu), 0);
    }

    /// 测试台架:与 [`scene::petal_camera`] 同视点的 #152 底座相机 + Q∞ 底色。
    fn stage() -> (SplatCamera, [f32; 3]) {
        let gl = oriera_render::camera::Camera::look_at(
            [0.0, 2.4, -4.2],
            [0.0; 3],
            [0.0, 1.0, 0.0],
            45.0,
            64,
            48,
            0.1,
            100.0,
        );
        (SplatCamera::from_view(&gl), scene::BRAND_BACKGROUND)
    }

    fn render_both(
        renderer: &SplatRenderer,
        cloud: &SplatCloud,
        cam: &SplatCamera,
        background: [f32; 3],
    ) -> (SplatImage, SplatImage) {
        let opts = RasterOptions {
            background,
            ..Default::default()
        };
        let cpu = crate::raster::rasterize(cloud, cam, &opts).unwrap();
        let gpu = renderer.render(cloud, cam, background).unwrap();
        (cpu, gpu)
    }

    fn worst_channel_delta(cpu: &SplatImage, gpu: &SplatImage) -> i32 {
        cpu.rgba
            .iter()
            .zip(&gpu.rgba)
            .map(|(a, &b)| (i32::from(*a) - i32::from(b)).abs())
            .max()
            .unwrap_or(0)
    }
}
