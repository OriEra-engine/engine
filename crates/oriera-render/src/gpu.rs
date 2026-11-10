//! L5 wgpu 最小闭环(`render-wgpu` feature,默认关)。
//!
//! 测评共识 P0「渲染层是空的」的第一刀:**真 GPU pass、真 mesh、真像素出图**。
//! 设计边界(诚实清单):
//! - 无头(headless)渲染到纹理,回读 BGRA 像素——不接窗口系统,不进帧调度;
//! - 旧管线([`HeadlessRenderer::render_mesh`]/`render_mesh_shaded`):顶点在
//!   CPU 侧归一化到裁剪空间,无深度缓冲,行为冻结(既有测试锁定);
//! - #152 首刀新增 [`HeadlessRenderer::render_mesh_view`]:**MVP uniform
//!   ([`crate::camera::Camera`],WebGPU NDC z∈[0,1])+ Depth32Float 深度缓冲
//!   (Less+write,近面遮蔽远面与绘制顺序无关)**,世界坐标网格直接渲染;
//! - 位置:windows/linux 原生验证;Web(wasm)是 wgpu 的既有能力,接入排在路线图。
//!
//! 用法见 `examples/glb_headless.rs`(GLB → 解码 → 渲染 → BMP)。

use crate::camera::Camera;
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

/// #152 首刀:世界坐标顶点经 MVP uniform(camera.view_proj)投影。
const SHADER_VIEW: &str = r#"
struct VSOut {
    @builtin(position) pos: vec4f,
    @location(0) color: vec4f,
};

@group(0) @binding(0) var<uniform> mvp: mat4x4f;

@vertex
fn vs(@location(0) p: vec3f, @location(1) c: vec4f) -> VSOut {
    var out: VSOut;
    out.pos = mvp * vec4f(p, 1.0);
    out.color = c;
    return out;
}

@fragment
fn fs(in: VSOut) -> @location(0) vec4f {
    return in.color;
}
"#;

/// #152 第四刀:PBR 最小着色(Cook-Torrance GGX + Smith-Schlick 遮蔽 + Schlick
/// 菲涅尔 + 半球环境替身)。MR 打包纹理 G=粗糙度 B=金属度与顶点因子相乘;
/// **无 MR 纹理时顶点 metallic 烘 0**(无 IBL 的金属黑兜底,行业通行)。
/// 单方向光,光强打包 light_dir.w。世界坐标顶点,UV,顶点色=材质 baseColorFactor。
const SHADER_VIEW_PBR: &str = r#"
struct VSOut {
    @builtin(position) pos: vec4f,
    @location(0) normal: vec3f,
    @location(1) uv: vec2f,
    @location(2) color: vec4f,
    @location(3) mr: vec2f,
    @location(4) world: vec3f,
};

struct Cam {
    mvp: mat4x4f,
    cam_pos: vec4f,
    light_dir: vec4f,
};

@group(0) @binding(0) var<uniform> cam: Cam;
@group(1) @binding(0) var base_tex: texture_2d<f32>;
@group(1) @binding(1) var mr_tex: texture_2d<f32>;
@group(1) @binding(2) var base_samp: sampler;

@vertex
fn vs(@location(0) p: vec3f, @location(1) n: vec3f, @location(2) uv: vec2f,
      @location(3) color: vec4f, @location(4) mr: vec2f) -> VSOut {
    var out: VSOut;
    out.pos = cam.mvp * vec4f(p, 1.0);
    out.world = p;
    out.normal = n;
    out.uv = uv;
    out.color = color;
    out.mr = mr;
    return out;
}

@fragment
fn fs(in: VSOut) -> @location(0) vec4f {
    let base = textureSample(base_tex, base_samp, in.uv);
    let mrs = textureSample(mr_tex, base_samp, in.uv);
    let albedo = in.color.rgb * base.rgb;
    let alpha = in.color.a * base.a;
    if (alpha < 0.003) { discard; }
    let metallic = clamp(in.mr.x * mrs.b, 0.0, 1.0);
    let roughness = clamp(in.mr.y * mrs.g, 0.05, 1.0);
    let n = normalize(in.normal);
    let v = normalize(cam.cam_pos.xyz - in.world);
    let l = normalize(cam.light_dir.xyz);
    let h = normalize(v + l);
    let ndl = clamp(dot(n, l), 0.0, 1.0);
    let ndv = clamp(dot(n, v), 0.0, 1.0) + 1e-4;
    let ndh = clamp(dot(n, h), 0.0, 1.0);
    let vdh = clamp(dot(v, h), 0.0, 1.0);
    // D:GGX/Trowbridge-Reitz
    let a2 = roughness * roughness;
    let dd = ndh * ndh * (a2 - 1.0) + 1.0;
    let dist = a2 / (3.14159265 * dd * dd);
    // G:Smith-Schlick(直接光 k=(r+1)^2/8)
    let k = (roughness + 1.0) * (roughness + 1.0) / 8.0;
    let gv = ndv / (ndv * (1.0 - k) + k);
    let gl = ndl / (ndl * (1.0 - k) + k);
    let vis = gv * gl;
    // F:Schlick
    let f0 = mix(vec3f(0.04), albedo, metallic);
    let fres = f0 + (1.0 - f0) * pow(1.0 - vdh, 5.0);
    let intensity = cam.light_dir.w;
    let spec = dist * vis * fres * ndl * intensity;
    let diff = (vec3f(1.0) - fres) * (1.0 - metallic) * albedo / 3.14159265 * ndl * intensity;
    // 半球环境替身(无 IBL;金属按 albedo·环境 0.4 兜底避免纯黑)
    let hemi = mix(vec3f(0.10, 0.11, 0.13), vec3f(0.22, 0.26, 0.32), n.y * 0.5 + 0.5);
    let ambient = albedo * (1.0 - metallic) * hemi + albedo * metallic * hemi * 0.4;
    let rgb = diff + spec + ambient;
    return vec4f(rgb, alpha);
}
"#;

/// 场景渲染主光方向(#152 第四刀;与 LIGHT 同向,存 w 分量作强度)。
const SCENE_LIGHT_DIR: [f32; 4] = [-0.45, 0.55, -0.70, 1.0];

/// 固定主光方向(世界感稳定的「左上前方」灯)。
const LIGHT: [f32; 3] = [0.45, 0.55, 0.70];

/// CPU 烘焙每顶点**颜色**(输入裁剪坐标;无材质渐变复刻旧公式)。
fn vertex_colors(
    clip: &[[f32; 3]],
    normals: Option<&[[f32; 3]]>,
    base_color: Option<[f32; 4]>,
) -> Vec<[f32; 4]> {
    let grad = |p: [f32; 3]| {
        [
            p[0] * 0.5 + 0.5,
            p[1] * 0.5 + 0.5,
            0.3 + 0.5 * (p[2] * 0.5 + 0.5),
            1.0,
        ]
    };
    match base_color {
        None => clip.iter().map(|p| grad(*p)).collect(),
        Some(base) => {
            let ln = (LIGHT[0] * LIGHT[0] + LIGHT[1] * LIGHT[1] + LIGHT[2] * LIGHT[2]).sqrt();
            clip.iter()
                .enumerate()
                .map(|(i, _)| {
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
                    [base[0] * shade, base[1] * shade, base[2] * shade, base[3]]
                })
                .collect()
        }
    }
}

/// 烘焙顶点(裁剪坐标 + 颜色)。12+16=28 字节,对齐 4。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadedVertex {
    pos: [f32; 3],
    color: [f32; 4],
}

/// CPU 烘焙每顶点颜色。
/// - 无材质:复刻旧版位置渐变(同公式,兼容旧出图观感);
/// - 有材质:baseColor × (0.35 环境 + 0.65 兰伯特),法线缺失时
///   以 (0,0,1) 朝向镜头(等比缩放不改变法线方向)。
fn bake_vertices(
    clip: &[[f32; 3]],
    normals: Option<&[[f32; 3]]>,
    base_color: Option<[f32; 4]>,
) -> Vec<ShadedVertex> {
    vertex_colors(clip, normals, base_color)
        .into_iter()
        .zip(clip.iter())
        .map(|(color, p)| ShadedVertex { pos: *p, color })
        .collect()
}

/// 无头渲染器:持有一套 device/queue/pipeline,可反复渲染网格。
pub struct HeadlessRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    /// #152 首刀:MVP uniform + 深度缓冲的世界坐标管线。
    pipeline_view: wgpu::RenderPipeline,
    /// 相机 uniform bind group layout(group0,96B:mvp+camPos+lightDir)。
    camera_bgl: wgpu::BindGroupLayout,
    /// 纹理 bind group layout(group1:base/mr/sampler)。
    tex_bgl: wgpu::BindGroupLayout,
    /// #152 第四刀:PBR 管线。
    pipeline_view_pbr: wgpu::RenderPipeline,
    /// base 白模视图(无纹理 part 的 albedo=因子兜底)。
    dummy_base_view: wgpu::TextureView,
    /// MR 白模视图(无 MR part 的因子直通兜底)。
    dummy_mr_view: wgpu::TextureView,
    /// 纹理采样器(LINEAR/CLAMP,PBR 与纹理路径共用)。
    scene_sampler: wgpu::Sampler,
    adapter_info: String,
}

/// 已上传的 sRGB 纹理及视图(与纹理管线配套)。
pub struct GpuTexture {
    view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

/// 场景渲染的一个绘制单元(世界坐标;[`HeadlessRenderer::render_scene_views`])。
pub struct ScenePart<'a> {
    pub positions: &'a [[f32; 3]],
    pub normals: Option<&'a [[f32; 3]]>,
    pub uvs: Option<&'a [[f32; 2]]>,
    pub indices: &'a [u32],
    /// 材质 baseColorFactor(PBR albedo;缺省语义由调用方给 [1,1,1,1])。
    pub base_color: [f32; 4],
    /// metallicFactor(PBR;无 MR 纹理时调用方应烘 0——无 IBL 金属黑兜底)。
    pub metallic: f32,
    /// roughnessFactor(PBR)。
    pub roughness: f32,
    /// baseColor 纹理(None = 白模兜底,albedo=因子)。
    pub base_texture: Option<&'a GpuTexture>,
    /// MR 打包纹理 G=粗糙度 B=金属度(None = 白模兜底,因子直通)。
    pub mr_texture: Option<&'a GpuTexture>,
}

/// 行主序 Mat4 → WGSL 列主序上传布局(与 render_mesh_view 同一约定)。
fn mvp_col_major(m: &[[f32; 4]; 4]) -> [f32; 16] {
    let mut out = [0f32; 16];
    for (r, row) in m.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            out[c * 4 + r] = *v;
        }
    }
    out
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

            // #152 首刀:同顶点流,uniform MVP + 深度测试(Less + write)。
            let shader_view = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("oriera-camera"),
                source: wgpu::ShaderSource::Wgsl(SHADER_VIEW.into()),
            });
            let pipeline_view = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("oriera-view-pipeline"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader_view,
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
                    module: &shader_view,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Bgra8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    // wgpu 30:两字段是 Option,None = 默认行为;显式写出语义。
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

            // #152 第二刀:纹理采样管线(pos+uv+shade;sRGB 纹理 × 顶点色)。
            // 显式布局:group0 = 相机 MVP(group 内 binding0),group1 = 纹理+采样器。
            let shader_view_pbr = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("oriera-camera-textured"),
                source: wgpu::ShaderSource::Wgsl(SHADER_VIEW_PBR.into()),
            });
            let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("oriera-camera-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    // mvp(顶点)+ camPos/lightDir(片元)同缓冲
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(96),
                    },
                    count: None,
                }],
            });
            let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("oriera-tex-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
            let tex_pipeline_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("oriera-view-tex-layout"),
                    bind_group_layouts: &[Some(&camera_bgl), Some(&tex_bgl)],
                    immediate_size: 0,
                });
            let pipeline_view_pbr =
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("oriera-view-textured-pipeline"),
                    layout: Some(&tex_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader_view_pbr,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[Some(wgpu::VertexBufferLayout {
                            array_stride: 56, // pos3 + normal3 + uv2 + color4 + mr2
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
                                    format: wgpu::VertexFormat::Float32x3,
                                },
                                wgpu::VertexAttribute {
                                    offset: 24,
                                    shader_location: 2,
                                    format: wgpu::VertexFormat::Float32x2,
                                },
                                wgpu::VertexAttribute {
                                    offset: 32,
                                    shader_location: 3,
                                    format: wgpu::VertexFormat::Float32x4,
                                },
                                wgpu::VertexAttribute {
                                    offset: 48,
                                    shader_location: 4,
                                    format: wgpu::VertexFormat::Float32x2,
                                },
                            ],
                        })],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader_view_pbr,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Bgra8Unorm,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: wgpu::TextureFormat::Depth32Float,
                        depth_write_enabled: Some(true),
                        depth_compare: Some(wgpu::CompareFunction::Less),
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    }),
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                });

            // PBR 白模兜底:base=纯白(albedo=因子),mr=白(G=B=1 → 因子直通)
            let mk_dummy = |px: [u8; 4]| {
                let t = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("oriera-dummy-tex"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &t,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &px,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d::default(),
                );
                t.create_view(&wgpu::TextureViewDescriptor::default())
            };
            let dummy_base_view = mk_dummy([255, 255, 255, 255]);
            let dummy_mr_view = mk_dummy([255, 255, 255, 255]);
            let scene_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("oriera-scene-sampler"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                ..Default::default()
            });

            Ok(HeadlessRenderer {
                device,
                queue,
                pipeline,
                pipeline_view,
                camera_bgl,
                tex_bgl,
                pipeline_view_pbr,
                dummy_base_view,
                dummy_mr_view,
                scene_sampler,
                adapter_info: format!("{} ({})", info.name, info.backend),
            })
        })
    }

    pub fn adapter_info(&self) -> &str {
        &self.adapter_info
    }

    /// 上传 RGBA8 像素为 sRGB 纹理(baseColorTexture 的色彩空间),LINEAR/CLAMP,
    /// 并生成与纹理管线配套的 bind group。调用方持有 [`GpuTexture`] 即持有资源。
    pub fn upload_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<GpuTexture, GpuError> {
        if rgba.len() != (width as usize) * (height as usize) * 4 || width == 0 || height == 0 {
            return Err(GpuError::Device("texture size/pixel mismatch".into()));
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("oriera-base-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // baseColorTexture 按规范是 sRGB 编码——采样硬件负责线性化。
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(GpuTexture {
            view,
            width,
            height,
        })
    }

    /// #152 第二刀:整场景渲染——同一 pass 内逐 part 绘制(共享相机与深度)。
    /// part 顺序即绘制顺序(深度测试保证遮挡正确,与顺序无关);带纹理的 part
    /// 走纹理管线,其余走 [`HeadlessRenderer::render_mesh_view`] 同族管线。
    /// 法线参与兰伯特烘焙(缺省按 (0,0,1) 朝镜头);UV 仅在带纹理时消费。
    pub fn render_scene_views(
        &self,
        parts: &[ScenePart<'_>],
        camera: &Camera,
    ) -> Result<RenderedImage, GpuError> {
        let (width, height) = (camera.width, camera.height);
        if width == 0 || height == 0 || parts.is_empty() {
            return Err(GpuError::Device("empty scene or zero-sized target".into()));
        }
        // 96B 场景 uniform:mvp(列主序)+ camPos + lightDir(光强打包 w)
        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct SceneUniform {
            mvp_col: [f32; 16],
            cam_pos: [f32; 4],
            light_dir: [f32; 4],
        }
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("scene-camera"),
                contents: bytemuck::cast_slice(&[SceneUniform {
                    mvp_col: mvp_col_major(&camera.view_proj()),
                    cam_pos: [
                        camera.eye_pos()[0],
                        camera.eye_pos()[1],
                        camera.eye_pos()[2],
                        0.0,
                    ],
                    light_dir: SCENE_LIGHT_DIR,
                }]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let cam_bind_pbr = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bind-pbr"),
            layout: &self.camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform,
                    offset: 0,
                    size: None,
                }),
            }],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gltf-scene-frame"),
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
        let depth = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("oriera-scene-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for part in parts {
                if part.positions.is_empty() {
                    continue;
                }
                // PBR 顶点:pos3 normal3 uv2 color4(因子) mr2(因子);法线缺省朝镜头
                let textured = part.base_texture.is_some() && part.uvs.is_some();
                let mut verts: Vec<[f32; 14]> = Vec::with_capacity(part.positions.len());
                for (i, p) in part.positions.iter().enumerate() {
                    let uv = if textured {
                        part.uvs
                            .and_then(|u| u.get(i).copied())
                            .unwrap_or([0.0, 0.0])
                    } else {
                        [0.0, 0.0]
                    };
                    let n = part
                        .normals
                        .and_then(|n| n.get(i).copied())
                        .unwrap_or([0.0, 0.0, 1.0]);
                    verts.push([
                        p[0],
                        p[1],
                        p[2],
                        n[0],
                        n[1],
                        n[2],
                        uv[0],
                        uv[1],
                        part.base_color[0],
                        part.base_color[1],
                        part.base_color[2],
                        part.base_color[3],
                        part.metallic,
                        part.roughness,
                    ]);
                }
                let vertex_buf =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("part-vertices"),
                            contents: bytemuck::cast_slice(&verts),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buf = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("part-indices"),
                        contents: bytemuck::cast_slice(part.indices),
                        usage: wgpu::BufferUsages::INDEX,
                    });
                // PBR 统一管线:白模兜底让无纹理/无 MR part 走因子路径
                pass.set_pipeline(&self.pipeline_view_pbr);
                pass.set_bind_group(0, &cam_bind_pbr, &[]);
                let base_view = part
                    .base_texture
                    .map(|t| &t.view)
                    .unwrap_or(&self.dummy_base_view);
                let mr_view = part
                    .mr_texture
                    .map(|t| &t.view)
                    .unwrap_or(&self.dummy_mr_view);
                pass.set_bind_group(
                    1,
                    &self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("part-tex-bind"),
                        layout: &self.tex_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(base_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(mr_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::Sampler(&self.scene_sampler),
                            },
                        ],
                    }),
                    &[],
                );
                pass.set_vertex_buffer(0, vertex_buf.slice(..));
                match part.indices.split_first() {
                    Some(_) => {
                        pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..part.indices.len() as u32, 0, 0..1);
                    }
                    None => pass.draw(0..part.positions.len() as u32, 0..1),
                }
            }
        }

        self.readback_pixels(encoder, &texture, width, height)
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

        self.readback_pixels(encoder, &texture, width, height)
    }

    /// 提交 encoder、把目标纹理拷进 256 对齐的 readback buffer、阻塞回读。
    /// 去填充后返回紧密排列的 BGRA 行。
    fn readback_pixels(
        &self,
        encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<RenderedImage, GpuError> {
        // 回读:wgpu 要求 bytes_per_row 对齐 256,读出后去填充
        let stride = (width * 4) as usize;
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

    /// #152 首刀:用相机渲染**世界坐标**网格(MVP uniform + Depth32Float
    /// 深度缓冲)。渲染目标尺寸取自相机的 `width`/`height`(纵横比一致)。
    /// 颜色规则与 [`HeadlessRenderer::render_mesh_shaded`] 相同——无材质:
    /// 旧版位置渐变(基于全网格归一化坐标);有材质:兰伯特(世界法线)。
    /// 深度测试 Less + write:近面遮蔽远面,与绘制顺序无关。
    pub fn render_mesh_view(
        &self,
        positions: &[[f32; 3]],
        normals: Option<&[[f32; 3]]>,
        base_color: Option<[f32; 4]>,
        indices: &[u32],
        camera: &Camera,
    ) -> Result<RenderedImage, GpuError> {
        if positions.is_empty() {
            return Err(GpuError::Device("empty mesh".into()));
        }
        let (width, height) = (camera.width, camera.height);
        let clip = normalize_to_clip(positions);
        let colors = vertex_colors(&clip, normals, base_color);
        let verts: Vec<ShadedVertex> = positions
            .iter()
            .zip(colors)
            .map(|(p, color)| ShadedVertex { pos: *p, color })
            .collect();

        let mvp = camera.view_proj();
        // WGSL 的 mat4x4f 是**列主序**:Mat4 行主序,上传时转置
        // (column_major[c*4+r] = mvp[r][c])——不转置等于乘了逆矩阵,全部被裁掉。
        let mut column_major = [0f32; 16];
        for (r, row) in mvp.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                column_major[c * 4 + r] = *v;
            }
        }
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("camera-mvp"),
                contents: bytemuck::cast_slice(&column_major),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bind"),
            layout: &self.pipeline_view.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform,
                    offset: 0,
                    size: None,
                }),
            }],
        });

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
        let depth = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("oriera-view-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline_view);
            pass.set_bind_group(0, &bind, &[]);
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

        self.readback_pixels(encoder, &texture, width, height)
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
            for (c, w) in want.iter().enumerate() {
                assert!((v[i].color[c] - w).abs() < 1e-6);
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

    /// #152 首刀实测:世界坐标三角形落在投影计算的位置,颜色与
    /// bake 公式一致(兰伯特,法线缺省按 (0,0,1) 朝镜头)。
    #[test]
    #[ignore = "requires a local GPU adapter (none in CI)"]
    fn camera_path_lands_the_triangle_where_projected() {
        let r = HeadlessRenderer::new().expect("adapter");
        let cam = Camera::look_at(
            [0.0; 3],
            [0.0, 0.0, -1.0],
            [0.0, 1.0, 0.0],
            90.0,
            64,
            64,
            0.1,
            100.0,
        );
        let img = r
            .render_mesh_view(
                &[[-1.0, -1.0, -4.0], [1.0, -1.0, -4.0], [0.0, 1.0, -4.0]],
                None,
                Some([0.9, 0.2, 0.2, 1.0]),
                &[0, 1, 2],
                &cam,
            )
            .expect("render");
        let bg = [33, 20, 13, 255];
        assert_eq!(img.pixel(2, 2), bg, "corner stays background");
        // 顶点 (0,-⅓,-4) 投影到 NDC (0, -1/12) → 像素 (32, 34/35);取 (32,32)
        // 在三角形内部(底边 y_ndc=-0.25 → 像素 40,顶点 y_ndc=+0.25 → 像素 24)。
        let mid = img.pixel(32, 32);
        assert_ne!(mid, bg, "projected triangle must be rasterized: {mid:?}");
        let ln = 0.45f32.hypot(0.55).hypot(0.70);
        let shade = 0.35 + 0.65 * (0.70f32 / ln).max(0.0);
        let want = |v: f32| (v * shade * 255.0).round() as i32;
        // BGRA:B=base.b×shade,G=base.g×shade,R=base.r×shade(±2 容差吸收舍入)。
        for (i, base) in [(0usize, 0.2f32), (1, 0.2), (2, 0.9)] {
            assert!(
                (mid[i] as i32 - want(base)).abs() <= 2,
                "channel {i}: {mid:?} vs shade {shade:.4}"
            );
        }
        assert_eq!(mid[3], 255);
    }

    /// #152 首刀实测(深度语义的核心证据):**近面先画、远面后画**——
    /// 没有深度缓冲时后画的远面会覆盖近面;有了 Depth32Float(Less+write),
    /// 近面必须胜出,与绘制顺序无关。
    #[test]
    #[ignore = "requires a local GPU adapter (none in CI)"]
    fn depth_buffer_lets_the_nearer_surface_win() {
        let r = HeadlessRenderer::new().expect("adapter");
        let cam = Camera::look_at(
            [0.0; 3],
            [0.0, 0.0, -1.0],
            [0.0, 1.0, 0.0],
            60.0,
            64,
            64,
            0.1,
            100.0,
        );
        // 两块平行四边形:近面 z=-2 先画,远面 z=-5 后画(fov60 深度 2 处
        // 视锥半宽 ≈1.155,半宽 1 的四边形完整在视锥内)。
        let mut positions = vec![
            [-1.0, -1.0, -2.0],
            [1.0, -1.0, -2.0],
            [1.0, 1.0, -2.0],
            [-1.0, 1.0, -2.0],
        ];
        positions.extend([
            [-1.0, -1.0, -5.0],
            [1.0, -1.0, -5.0],
            [1.0, 1.0, -5.0],
            [-1.0, 1.0, -5.0],
        ]);
        let indices = [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7];
        let img = r
            .render_mesh_view(&positions, None, None, &indices, &cam)
            .expect("render");
        // 全网格归一化:包围盒 z∈[-5,-2],近面 z'=+0.9、远面 z'=-0.9,
        // 渐变 B = 0.3+0.5×(z'×0.5+0.5) → 近面 ≈198、远面 ≈83。
        // 无深度时后画的远面会把它压到 ≈83;有深度必须是近面的 ≈198。
        let mid = img.pixel(32, 32);
        assert!(
            (170..=215).contains(&mid[0]),
            "near quad (B≈198) must win via depth, got {mid:?} (far quad would be ≈83)"
        );
        // 中心像素是近面四角的插值(R/G 均 ≈127.5),进一步确认是近面而非背景。
        assert!(
            (110..=145).contains(&mid[1]) && (110..=145).contains(&mid[2]),
            "center must interpolate the near quad corners: {mid:?}"
        );
    }
}
