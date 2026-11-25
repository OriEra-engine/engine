//! 持久渲染路径(RFC 0004 步 1a):资源跨帧复用 + 实例化绘制 + 显式回读。
//!
//! 快照路径([`crate::gpu::HeadlessRenderer::render_scene_views`])每次调用都重建
//! 顶点/索引缓冲、绑定组与渲染目标,并在返回前阻塞等 GPU 出像素。它适合
//! "渲染一张图",不适合进帧循环。本模块提供与之并存的第二条路:
//!
//! - [`GpuMesh`] / [`GpuMaterial`]:上传一次,跨帧复用,由调用方持有
//!   (与 [`crate::gpu::GpuTexture`] 同款惯例)。引擎不做隐式缓存——数据变了而
//!   缓存不知道,是这类设计最常见的静默错误来源。
//! - [`Instance`] 带模型矩阵(快照路径的 `ScenePart` 是世界坐标,物件要移动
//!   只能重传顶点);同一网格同一材质的实例走一次实例化绘制。
//! - [`RenderTarget`] 持久;`render_draw_list` 提交后立即返回,不同步等待;
//!   要像素的调用方自己调 `read_target`。
//!
//! 稳态下每帧不新建任何 GPU 资源,由 [`FrameStats`] 自报并可被测试断言。
//! 实例缓冲容量显式管理:超出时 `render_draw_list` 返回错误,不在帧内重建。
//!
//! 只保证不透明物件的正确性:管线无混合,实例按网格与材质排序,会打乱
//! 调用方给的绘制顺序。

use super::{
    mvp_col_major, Camera, GpuError, GpuTexture, HeadlessRenderer, RenderedImage, CLEAR,
    SCENE_LIGHT_DIR,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use wgpu::util::DeviceExt;

/// 实例缓冲的初始容量(实例数)。超出时调 `reserve_instances` 显式扩容。
pub const DEFAULT_INSTANCE_CAPACITY: u32 = 1024;

/// 每实例模型矩阵:4 列,每列 4 个浮点。
const INSTANCE_FLOATS: usize = 16;
const INSTANCE_BYTES: u64 = (INSTANCE_FLOATS * 4) as u64;
/// 相机 uniform:mvp(列主序)+ camPos + lightDir,与快照路径同布局。
const CAMERA_UNIFORM_BYTES: u64 = 96;
/// 材质 uniform:baseColor 因子 + 金属度/粗糙度。
const MATERIAL_UNIFORM_BYTES: u64 = 32;

/// 持久路径顶点:pos3 + normal3 + uv2。材质因子不再烘进顶点,
/// 同一网格换材质共用同一份顶点数据。
type Vertex = [f32; 8];

/// 已上传的网格:顶点缓冲 + 索引缓冲。调用方持有该值即持有资源。
#[derive(Debug)]
pub struct GpuMesh {
    id: u64,
    vertex: wgpu::Buffer,
    index: wgpu::Buffer,
    index_count: u32,
}

/// 已建好的材质:因子 uniform + 纹理绑定组。调用方持有该值即持有资源。
#[derive(Debug)]
pub struct GpuMaterial {
    id: u64,
    /// 保活句柄:材质存续期间 uniform 缓冲必须一直在。
    #[expect(dead_code, reason = "持有即保活,不读取")]
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

/// 一个绘制实例:哪个网格、哪种材质、摆在哪。
///
/// `model` 是行主序模型矩阵。只支持刚体变换与均匀缩放:法线按模型矩阵的
/// 旋转部分变换,均匀缩放带来的模长在片元里被归一化消掉;非均匀缩放
/// 不保证正确。
#[derive(Clone, Copy)]
pub struct Instance<'a> {
    pub mesh: &'a GpuMesh,
    pub material: &'a GpuMaterial,
    pub model: [[f32; 4]; 4],
}

/// 一帧要画的东西。由调用方(应用壳,或显式拿到世界的 System)构造:
/// L5 不持有世界,也不自己遍历组件。
#[derive(Default)]
pub struct DrawList<'a> {
    pub instances: Vec<Instance<'a>>,
}

impl<'a> DrawList<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, mesh: &'a GpuMesh, material: &'a GpuMaterial, model: [[f32; 4]; 4]) {
        self.instances.push(Instance {
            mesh,
            material,
            model,
        });
    }

    pub fn len(&self) -> usize {
        self.instances.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    pub fn clear(&mut self) {
        self.instances.clear();
    }
}

/// 持久渲染目标:颜色 + 深度纹理,建一次反复写。
#[derive(Debug)]
pub struct RenderTarget {
    width: u32,
    height: u32,
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    /// 保活句柄:视图在使用期间深度纹理必须一直在。
    #[expect(dead_code, reason = "持有即保活,不读取")]
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
}

impl RenderTarget {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

/// 一帧的自报数据。`created_*` 是相对上一帧新建的 GPU 资源个数,只统计
/// 持久路径内部的创建;稳态下应全为 0。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameStats {
    pub draw_calls: u32,
    pub instances: u32,
    pub created_buffers: u32,
    pub created_bind_groups: u32,
    pub created_textures: u32,
}

/// 渲染器内部状态。缓冲与绑定组在构造时建好,帧内只 `write_buffer`。
pub(crate) struct PersistState {
    pipeline: wgpu::RenderPipeline,
    material_bgl: wgpu::BindGroupLayout,
    camera_buffer: wgpu::Buffer,
    camera_bind: wgpu::BindGroup,
    /// 实例缓冲与容量(实例数)。
    instances: Mutex<(wgpu::Buffer, u32)>,
    /// 每帧复用的 CPU 侧暂存:排序下标与实例矩阵字节,避免帧内堆分配。
    scratch: Mutex<Scratch>,
    /// 上次上报时的累计计数,用于算每帧增量。
    last: Mutex<Totals>,
    next_id: AtomicU64,
    created_buffers: AtomicU32,
    created_bind_groups: AtomicU32,
    created_textures: AtomicU32,
    /// 异步回读的簿记:在途、待取与空闲缓冲。
    reads: Mutex<ReadState>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Totals {
    buffers: u32,
    bind_groups: u32,
    textures: u32,
}

#[derive(Default)]
struct Scratch {
    /// 实例的绘制顺序:按网格 id、材质 id 稳定排序后的下标。
    order: Vec<u32>,
    /// 实例矩阵,按列排布,直接写进实例缓冲。
    bytes: Vec<u8>,
}

/// 相机 uniform,布局与快照路径的 `SceneUniform` 相同。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    mvp_col: [f32; 16],
    cam_pos: [f32; 4],
    light_dir: [f32; 4],
}

/// 材质 uniform:baseColor 因子 + 金属度/粗糙度(补齐到 vec4)。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialUniform {
    color: [f32; 4],
    mr: [f32; 4],
}

macro_rules! persist_vertex {
    () => {
        r#"struct VSOut {
    @builtin(position) pos: vec4f,
    @location(0) normal: vec3f,
    @location(1) uv: vec2f,
    @location(2) color: vec4f,
    @location(3) mr: vec2f,
    @location(4) world: vec3f,
};

struct Material {
    color: vec4f,
    mr: vec4f,
};

@group(0) @binding(0) var<uniform> cam: Cam;
@group(1) @binding(0) var base_tex: texture_2d<f32>;
@group(1) @binding(1) var mr_tex: texture_2d<f32>;
@group(1) @binding(2) var base_samp: sampler;
@group(1) @binding(3) var<uniform> material: Material;

// 实例化顶点:网格顶点在 slot 0,模型矩阵四列在 slot 1(每实例步进)。
@vertex
fn vs_inst(@location(0) p: vec3f, @location(1) n: vec3f, @location(2) uv: vec2f,
      @location(5) c0: vec4f, @location(6) c1: vec4f,
      @location(7) c2: vec4f, @location(8) c3: vec4f) -> VSOut {
    let world = mat4x4f(c0, c1, c2, c3) * vec4f(p, 1.0);
    var out: VSOut;
    out.pos = cam.mvp * world;
    out.world = world.xyz;
    // 取模型矩阵的旋转部分;均匀缩放的模长由片元里的 normalize 消掉。
    out.normal = mat3x3f(c0.xyz, c1.xyz, c2.xyz) * n;
    out.uv = uv;
    out.color = material.color;
    out.mr = material.mr.xy;
    return out;
}
"#
    };
}
/// 完整着色器:相机结构 + 持久顶点 + 与快照路径共用的 PBR 片元。
const SHADER_PERSIST: &str = concat!(
    r#"
struct Cam {
    mvp: mat4x4f,
    cam_pos: vec4f,
    light_dir: vec4f,
};
"#,
    persist_vertex!(),
    pbr_fragment!(),
);

/// 持锁期间不会 panic;万一中毒也取出内部状态,不把渲染线程带崩。
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn texture_binding(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn matrix_column(location: u32, offset: u64) -> wgpu::VertexAttribute {
    wgpu::VertexAttribute {
        offset,
        shader_location: location,
        format: wgpu::VertexFormat::Float32x4,
    }
}

impl PersistState {
    /// 建管线、材质绑定组布局、常驻相机缓冲与初始实例缓冲。
    /// 构造期的创建不计入 `created_*`:那些计数描述的是帧内行为。
    pub(crate) fn new(device: &wgpu::Device, camera_bgl: &wgpu::BindGroupLayout) -> Self {
        let material_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("oriera-persist-material-bgl"),
            entries: &[
                texture_binding(0),
                texture_binding(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(MATERIAL_UNIFORM_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("oriera-persist-layout"),
            bind_group_layouts: &[Some(camera_bgl), Some(&material_bgl)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("oriera-persist"),
            source: wgpu::ShaderSource::Wgsl(SHADER_PERSIST.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("oriera-persist-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_inst"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 32, // pos3 + normal3 + uv2
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
                        ],
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 64, // 模型矩阵四列
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            matrix_column(5, 0),
                            matrix_column(6, 16),
                            matrix_column(7, 32),
                            matrix_column(8, 48),
                        ],
                    }),
                ],
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

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oriera-persist-camera"),
            size: CAMERA_UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("oriera-persist-camera-bind"),
            layout: camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &camera_buffer,
                    offset: 0,
                    size: None,
                }),
            }],
        });
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oriera-persist-instances"),
            size: INSTANCE_BYTES * u64::from(DEFAULT_INSTANCE_CAPACITY),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            material_bgl,
            camera_buffer,
            camera_bind,
            instances: Mutex::new((instances, DEFAULT_INSTANCE_CAPACITY)),
            scratch: Mutex::new(Scratch::default()),
            last: Mutex::new(Totals::default()),
            next_id: AtomicU64::new(1),
            created_buffers: AtomicU32::new(0),
            created_bind_groups: AtomicU32::new(0),
            created_textures: AtomicU32::new(0),
            reads: Mutex::new(ReadState::default()),
        }
    }
}

/// 计数约定:本文件里每一次 `create_buffer*` / `create_bind_group` /
/// `create_texture` 之后紧跟一句对应的 `count_new_*`,没有例外。
/// `FrameStats.created_*` 的可信度全靠这条,评审时按它卡。
impl HeadlessRenderer {
    fn count_new_buffer(&self) {
        self.persist.created_buffers.fetch_add(1, Ordering::Relaxed);
    }

    fn count_new_bind_group(&self) {
        self.persist
            .created_bind_groups
            .fetch_add(1, Ordering::Relaxed);
    }

    fn count_new_texture(&self) {
        self.persist
            .created_textures
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 上传网格,跨帧复用。索引必须非空;给了法线或 UV 时长度必须与顶点数
    /// 一致;索引越界在 CPU 侧显式拒绝,不赌 GPU 行为。
    /// 缺省语义与快照路径相同:无法线按朝向镜头 (0,0,1),无 UV 取 (0,0)。
    pub fn upload_mesh(
        &self,
        positions: &[[f32; 3]],
        normals: Option<&[[f32; 3]]>,
        uvs: Option<&[[f32; 2]]>,
        indices: &[u32],
    ) -> Result<GpuMesh, GpuError> {
        if positions.is_empty() {
            return Err(GpuError::Device("mesh has no vertices".into()));
        }
        if indices.is_empty() {
            return Err(GpuError::Device("mesh has no indices".into()));
        }
        if normals.is_some_and(|n| n.len() != positions.len()) {
            return Err(GpuError::Device(
                "normals length differs from positions".into(),
            ));
        }
        if uvs.is_some_and(|u| u.len() != positions.len()) {
            return Err(GpuError::Device("uvs length differs from positions".into()));
        }
        if indices.iter().any(|&i| i as usize >= positions.len()) {
            return Err(GpuError::Device("index out of range".into()));
        }
        let mut verts: Vec<Vertex> = Vec::with_capacity(positions.len());
        for (i, p) in positions.iter().enumerate() {
            let n = normals
                .and_then(|n| n.get(i).copied())
                .unwrap_or([0.0, 0.0, 1.0]);
            let uv = uvs.and_then(|u| u.get(i).copied()).unwrap_or([0.0, 0.0]);
            verts.push([p[0], p[1], p[2], n[0], n[1], n[2], uv[0], uv[1]]);
        }
        let vertex = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("oriera-mesh-vertices"),
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
        self.count_new_buffer();
        let index = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("oriera-mesh-indices"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        self.count_new_buffer();
        Ok(GpuMesh {
            id: self.persist.next_id.fetch_add(1, Ordering::Relaxed),
            vertex,
            index,
            index_count: indices.len() as u32,
        })
    }

    /// 建材质:因子进 uniform,纹理与采样器进绑定组。无纹理走白模兜底,
    /// 语义同快照路径。
    pub fn create_material(
        &self,
        base_color: [f32; 4],
        metallic: f32,
        roughness: f32,
        base_texture: Option<&GpuTexture>,
        mr_texture: Option<&GpuTexture>,
    ) -> GpuMaterial {
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("oriera-material"),
                contents: bytemuck::cast_slice(&[MaterialUniform {
                    color: base_color,
                    mr: [metallic, roughness, 0.0, 0.0],
                }]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        self.count_new_buffer();
        let base_view = base_texture
            .map(|t| &t.view)
            .unwrap_or(&self.dummy_base_view);
        let mr_view = mr_texture.map(|t| &t.view).unwrap_or(&self.dummy_mr_view);
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("oriera-material-bind"),
            layout: &self.persist.material_bgl,
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
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniform,
                        offset: 0,
                        size: None,
                    }),
                },
            ],
        });
        self.count_new_bind_group();
        GpuMaterial {
            id: self.persist.next_id.fetch_add(1, Ordering::Relaxed),
            uniform,
            bind,
        }
    }

    /// 建持久渲染目标:颜色(BGRA8,可回读)+ 深度。
    pub fn create_target(&self, width: u32, height: u32) -> Result<RenderTarget, GpuError> {
        if width == 0 || height == 0 {
            return Err(GpuError::Device("zero-sized target".into()));
        }
        Ok(self.build_target(width, height))
    }

    fn build_target(&self, width: u32, height: u32) -> RenderTarget {
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let color = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("oriera-target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.count_new_texture();
        let depth = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("oriera-target-depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        self.count_new_texture();
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        RenderTarget {
            width,
            height,
            color,
            color_view,
            depth,
            depth_view,
        }
    }

    /// 显式改尺寸:尺寸没变则不动,否则重建颜色与深度纹理。
    /// 不在 `render_draw_list` 里悄悄做。
    pub fn resize_target(
        &self,
        target: &mut RenderTarget,
        width: u32,
        height: u32,
    ) -> Result<(), GpuError> {
        if width == 0 || height == 0 {
            return Err(GpuError::Device("zero-sized target".into()));
        }
        if target.width != width || target.height != height {
            *target = self.build_target(width, height);
        }
        Ok(())
    }

    /// 实例缓冲当前容量(实例数)。
    pub fn instance_capacity(&self) -> u32 {
        lock(&self.persist.instances).1
    }

    /// 把实例缓冲扩到至少 `capacity` 个实例。只增不减;容量已够则不动。
    pub fn reserve_instances(&self, capacity: u32) {
        let mut slot = lock(&self.persist.instances);
        if slot.1 >= capacity {
            return;
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oriera-persist-instances"),
            size: INSTANCE_BYTES * u64::from(capacity),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.count_new_buffer();
        *slot = (buffer, capacity);
    }
}

impl HeadlessRenderer {
    /// 提交一帧:**提交后立即返回**,不等待 GPU,也不返回像素。空列表是合法
    /// 的一帧——只清屏。相机尺寸必须与渲染目标一致,否则报错而不是画出一张
    /// 被拉伸的图。
    ///
    /// 实例按(网格 id,材质 id)稳定排序后归段,每段一次实例化绘制。排序会
    /// 打乱调用方给的顺序,因此只保证不透明物件的正确性。
    pub fn render_draw_list(
        &self,
        list: &DrawList<'_>,
        camera: &Camera,
        target: &RenderTarget,
    ) -> Result<FrameStats, GpuError> {
        if camera.width != target.width || camera.height != target.height {
            return Err(GpuError::Device(
                "camera size differs from render target".into(),
            ));
        }
        let count = list.instances.len();

        // 排序与实例矩阵写入复用的暂存区:帧内不做堆分配。
        let mut scratch = lock(&self.persist.scratch);
        let Scratch { order, bytes } = &mut *scratch;
        order.clear();
        order.extend(0..count as u32);
        order.sort_by_key(|&i| {
            let x = &list.instances[i as usize];
            (x.mesh.id, x.material.id)
        });
        bytes.clear();
        bytes.reserve(count * INSTANCE_FLOATS * 4);
        for &i in order.iter() {
            // 行主序 → 按列上传(WGSL mat4x4f 以列为参数)。
            let m = &list.instances[i as usize].model;
            let mut cols = [[0f32; 4]; 4];
            for (r, row) in m.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    cols[c][r] = *v;
                }
            }
            for c in &cols {
                bytes.extend_from_slice(bytemuck::cast_slice(c));
            }
        }

        let instances = lock(&self.persist.instances);
        let (instance_buf, capacity) = (&instances.0, instances.1);
        if count as u64 > u64::from(capacity) {
            return Err(GpuError::Device(format!(
                "draw list has {count} instances, capacity is {capacity}; call reserve_instances"
            )));
        }

        let eye = camera.eye_pos();
        let cam = CameraUniform {
            mvp_col: mvp_col_major(&camera.view_proj()),
            cam_pos: [eye[0], eye[1], eye[2], 0.0],
            light_dir: SCENE_LIGHT_DIR,
        };
        self.queue
            .write_buffer(&self.persist.camera_buffer, 0, bytemuck::cast_slice(&[cam]));
        if count > 0 {
            self.queue.write_buffer(instance_buf, 0, bytes);
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("oriera-persist-frame"),
            });
        let mut draw_calls = 0u32;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("oriera-persist-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth_view,
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
            pass.set_pipeline(&self.persist.pipeline);
            pass.set_bind_group(0, &self.persist.camera_bind, &[]);

            // 连续相同(网格,材质)的实例归一段,一次 draw_indexed。
            let mut start = 0usize;
            while start < order.len() {
                let first = &list.instances[order[start] as usize];
                let key = (first.mesh.id, first.material.id);
                let mut end = start + 1;
                while end < order.len() {
                    let x = &list.instances[order[end] as usize];
                    if (x.mesh.id, x.material.id) != key {
                        break;
                    }
                    end += 1;
                }
                pass.set_bind_group(1, &first.material.bind, &[]);
                pass.set_vertex_buffer(0, first.mesh.vertex.slice(..));
                pass.set_vertex_buffer(
                    1,
                    instance_buf.slice(start as u64 * INSTANCE_BYTES..end as u64 * INSTANCE_BYTES),
                );
                pass.set_index_buffer(first.mesh.index.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..first.mesh.index_count, 0, 0..(end - start) as u32);
                draw_calls += 1;
                start = end;
            }
        }
        // 只提交,不轮询:GPU 与 CPU 可以流水起来。
        self.queue.submit(Some(encoder.finish()));
        drop(scratch);
        drop(instances);

        let now = Totals {
            buffers: self.persist.created_buffers.load(Ordering::Relaxed),
            bind_groups: self.persist.created_bind_groups.load(Ordering::Relaxed),
            textures: self.persist.created_textures.load(Ordering::Relaxed),
        };
        let mut last = lock(&self.persist.last);
        let stats = FrameStats {
            draw_calls,
            instances: count as u32,
            created_buffers: now.buffers - last.buffers,
            created_bind_groups: now.bind_groups - last.bind_groups,
            created_textures: now.textures - last.textures,
        };
        *last = now;
        Ok(stats)
    }

    /// 把渲染目标读回成像素。这是**显式**动作,也是持久路径里唯一会阻塞
    /// 等 GPU 完成的地方;它新建的回读缓冲不计入 `FrameStats.created_*`。
    pub fn read_target(&self, target: &RenderTarget) -> Result<RenderedImage, GpuError> {
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("oriera-persist-readback"),
            });
        self.readback_pixels(encoder, &target.color, target.width, target.height)
    }
}

/// 异步回读(RFC 0004 步 1b):提交后不等 GPU,拿到回执再来取。
///
/// 同步的 `read_target` 每次都要停下来等像素,帧循环里 CPU 与 GPU 轮空:CPU
/// 等 GPU 画完,GPU 再等 CPU 提交下一帧。这里把"要像素"拆成三步:
///
/// 1. `read_target_async`:把目标当前内容拷进回读缓冲并开始映射,立即返回
///    [`ReadTicket`];
/// 2. 中间随便做什么——提交后面几帧也行;
/// 3. `poll_read`(不阻塞)或 `wait_read`(等到好)取像素。
///
/// 两条硬约束,都是为了不无限占内存:
///
/// - 同一时刻最多 [`MAX_READS_IN_FLIGHT`] 张回读在 GPU 侧在途。满了的话,
///   `read_target_async` 会等到最旧的完成再往下走(背压),而不是悄悄排队。
/// - 已完成但没人取走的像素同样最多留这么多张,再多丢最旧的。所以一张回执
///   如果在之后又发起了 `MAX_READS_IN_FLIGHT` 次请求之前没被取走,它就作废了,
///   取的时候报错,不会给错图。
///
/// 回读缓冲进池子复用,稳态下不新建;池子涨出来的那几次创建会出现在之后那帧的
/// [`FrameStats::created_buffers`] 里(同步 `read_target` 是一次性的,照旧不计)。
impl HeadlessRenderer {
    /// 请求把渲染目标的当前内容读回来。本方法把拷贝命令排进队列后立即返回,
    /// **不等待 GPU**。画面取的是提交时刻之前所有已提交帧画完后的结果。
    pub fn read_target_async(&self, target: &RenderTarget) -> Result<ReadTicket, GpuError> {
        // 回读缓冲要求每行 256 字节对齐。
        let row = padded_row_bytes(target.width);
        let data_bytes = row * u64::from(target.height);

        // 腾在途名额:满了就等到至少一张完成。
        {
            let mut st = lock(&self.persist.reads);
            while st.pending.len() >= MAX_READS_IN_FLIGHT {
                self.harvest_reads(&mut st, true);
            }
            let seq = st.next_seq + 1;
            st.next_seq = seq;
        }

        // 取一块够大的空闲缓冲;都没有才新建。
        let buffer = {
            let mut st = lock(&self.persist.reads);
            match st.idle.iter().position(|&(_, cap)| cap >= data_bytes) {
                Some(i) => st.idle.swap_remove(i).0,
                None => {
                    // 比现在需要的还小的缓冲以后也用不上,不留。
                    st.idle.retain(|&(_, cap)| cap >= data_bytes);
                    let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("oriera-persist-readback"),
                        size: data_bytes,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    });
                    self.count_new_buffer();
                    buffer
                }
            }
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("oriera-persist-readback-async"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row as u32),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: target.width,
                height: target.height,
                depth_or_array_layers: 1,
            },
        );
        // 只提交不轮询:拷贝排在本帧绘制之后,由 GPU 按序执行。
        self.queue.submit(Some(encoder.finish()));

        let done = Arc::new(Mutex::new(None));
        buffer.slice(..data_bytes).map_async(wgpu::MapMode::Read, {
            let done = Arc::clone(&done);
            move |result| *lock(&done) = Some(result)
        });

        let ticket = {
            let mut st = lock(&self.persist.reads);
            let ticket = ReadTicket { seq: st.next_seq };
            st.pending.push_back(PendingRead {
                seq: ticket.seq,
                capacity: buffer.size(),
                buffer,
                width: target.width,
                height: target.height,
                row,
                data_bytes,
                done,
            });
            ticket
        };
        Ok(ticket)
    }

    /// 问一次"好了没",不阻塞。`Ok(None)` 是还没好;回执作废或从未签发过则报错。
    pub fn poll_read(&self, ticket: ReadTicket) -> Result<Option<RenderedImage>, GpuError> {
        let mut st = lock(&self.persist.reads);
        self.harvest_reads(&mut st, false);
        st.take(ticket.seq)
    }

    /// 等到这张回执就绪并取走像素。设备挂了或回执作废则报错。
    pub fn wait_read(&self, ticket: ReadTicket) -> Result<RenderedImage, GpuError> {
        let mut st = lock(&self.persist.reads);
        loop {
            self.harvest_reads(&mut st, true);
            match st.take(ticket.seq) {
                Ok(Some(img)) => return Ok(img),
                Ok(None) => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// 还在 GPU 侧没完成的异步回读张数(不含已完成待取的)。
    pub fn reads_in_flight(&self) -> usize {
        lock(&self.persist.reads).pending.len()
    }

    /// 推进设备并把已完成的在途回读搬到待取队列。`wait=true` 时至少推进到
    /// 有一张完成为止(没有在途的则立即返回);否则只问一次,绝不阻塞。
    /// 持锁调用是安全的:映射回调只写各自的 `done` 标记,不碰这本簿记。
    fn harvest_reads(&self, st: &mut ReadState, wait: bool) {
        loop {
            let _ = self.device.poll(if wait {
                wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                }
            } else {
                wgpu::PollType::Poll
            });
            let before = st.pending.len();
            let items = std::mem::take(&mut st.pending);
            for item in items {
                if item.is_done() {
                    self.finish_read(st, item);
                } else {
                    st.pending.push_back(item);
                }
            }
            if !wait || st.pending.len() < before || st.pending.is_empty() {
                return;
            }
        }
    }

    /// 一张在途回读完成后:去填充拷出像素,缓冲还池,结果进待取队列。
    fn finish_read(&self, st: &mut ReadState, item: PendingRead) {
        let result = match &*lock(&item.done) {
            Some(Ok(())) => {
                let slice = item.buffer.slice(..item.data_bytes);
                match slice.get_mapped_range() {
                    Ok(data) => {
                        let stride = (item.width as usize) * 4;
                        let row = item.row as usize;
                        let mut bgra = Vec::with_capacity(stride * item.height as usize);
                        for y in 0..item.height as usize {
                            let start = y * row;
                            bgra.extend_from_slice(&data[start..start + stride]);
                        }
                        drop(data);
                        item.buffer.unmap();
                        Ok(RenderedImage {
                            width: item.width,
                            height: item.height,
                            bgra,
                        })
                    }
                    Err(e) => Err(GpuError::Map(format!("{e}"))),
                }
            }
            Some(Err(e)) => Err(GpuError::Map(format!("{e}"))),
            None => Err(GpuError::Map("readback finished without a result".into())),
        };
        // 映射失败或没拿到映射范围的缓冲不回收:状态不明,直接丢掉。
        if result.is_ok() {
            st.idle.push((item.buffer, item.capacity));
        }
        st.ready.push_back((item.seq, result));
        while st.ready.len() > MAX_READS_IN_FLIGHT {
            st.ready.pop_front();
        }
    }
}

/// 同一时刻最多在途的异步回读张数,也是待取像素的上限。
pub const MAX_READS_IN_FLIGHT: usize = 4;

/// 一次异步回读的回执。按签发顺序编号,只在本渲染器内有效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadTicket {
    seq: u64,
}

/// 回读缓冲每行的字节数(256 对齐)。
fn padded_row_bytes(width: u32) -> u64 {
    (u64::from(width) * 4).div_ceil(256) * 256
}

/// 还在 GPU 侧的一张回读。
struct PendingRead {
    seq: u64,
    buffer: wgpu::Buffer,
    /// 缓冲的完整容量,还池时按它登记。
    capacity: u64,
    width: u32,
    height: u32,
    /// 对齐后的行宽。
    row: u64,
    /// 实际用到的字节数。
    data_bytes: u64,
    /// 映射回调的结果;`None` 表示还没完成。
    done: Arc<Mutex<Option<Result<(), wgpu::BufferAsyncError>>>>,
}

impl PendingRead {
    fn is_done(&self) -> bool {
        lock(&self.done).is_some()
    }
}

/// 异步回读的簿记。
#[derive(Default)]
struct ReadState {
    /// 最近一张回执的编号,从 1 起。
    next_seq: u64,
    /// 已提交、GPU 侧尚未完成的回读,按提交顺序。
    pending: VecDeque<PendingRead>,
    /// 已完成、尚未被取走的像素,按回执顺序。
    ready: VecDeque<(u64, Result<RenderedImage, GpuError>)>,
    /// 空闲回读缓冲(缓冲, 容量字节)。
    idle: Vec<(wgpu::Buffer, u64)>,
}

impl ReadState {
    /// 取走一张回执的结果。还没好返回 `Ok(None)`;不在途也不在待取队列里
    /// (作废、已取走或从未签发)返回 `Err`。
    fn take(&mut self, seq: u64) -> Result<Option<RenderedImage>, GpuError> {
        if let Some(i) = self.ready.iter().position(|&(s, _)| s == seq) {
            return self
                .ready
                .remove(i)
                .expect("position just found")
                .1
                .map(Some);
        }
        if self.pending.iter().any(|p| p.seq == seq) {
            return Ok(None);
        }
        Err(GpuError::Device(format!(
            "read ticket {seq} is unknown, already taken or was dropped"
        )))
    }
}
