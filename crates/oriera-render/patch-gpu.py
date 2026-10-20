import io

p = 'src/gpu.rs'
s = io.open(p, encoding='utf-8').read()

# ---- 1. SHADER:顶点色直通(颜色由 CPU 烘焙 —— 渐变或兰伯特) ----
old_shader = '''const SHADER: &str = r#"
struct VSOut {
    @builtin(position) pos: vec4f,
    @location(0) color: vec3f,
};

@vertex
fn vs(@location(0) p: vec3f) -> VSOut {
    // 顶点已在裁剪空间(CPU 侧归一化);z 折进颜色做深度示意
    var out: VSOut;
    out.pos = vec4f(p, 1.0);
    out.color = vec3f(p.x * 0.5 + 0.5, p.y * 0.5 + 0.5, 0.3 + 0.5 * (p.z * 0.5 + 0.5));
    return out;
}

@fragment
fn fs(in: VSOut) -> @location(0) vec4f {
    return vec4f(in.color, 1.0);
}
"#;'''
new_shader = '''const SHADER: &str = r#"
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
"#";

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
/// - 无材质:复刻旧版位置渐变(字节级同公式,兼容旧出图观感);
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
        None => clip.iter().map(|p| ShadedVertex { pos: *p, color: grad(*p) }).collect(),
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
                        ((n[0] * LIGHT[0] + n[1] * LIGHT[1] + n[2] * LIGHT[2]) / (nn * ln))
                            .max(0.0)
                    } else {
                        0.0
                    };
                    let shade = 0.35 + 0.65 * diffuse;
                    ShadedVertex {
                        pos: *p,
                        color: [
                            base[0] * shade,
                            base[1] * shade,
                            base[2] * shade,
                            base[3],
                        ],
                    }
                })
                .collect()
        }
    }
}'''
assert old_shader in s, 'shader anchor'
s = s.replace(old_shader, new_shader, 1)

# ---- 2. pipeline 顶点布局:pos@0 + color@12, stride 28 ----
old_layout = '''                    buffers: &[Some(wgpu::VertexBufferLayout {'''
# 找到该行及其后 attributes 数组,整段替换
import re
m = re.search(
    r'buffers: &\[Some\(wgpu::VertexBufferLayout \{\s*step_mode: wgpu::VertexStepMode::Vertex,\s*attributes: &\[wgpu::VertexAttribute \{[^}]*\}\],\s*\}\)\],',
    s,
)
assert m, 'layout anchor'
new_layout = '''buffers: &[Some(wgpu::VertexBufferLayout {
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x3,
                                offset: 0,
                                shader_location: 0,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 12,
                                shader_location: 1,
                            },
                        ],
                    })],'''
s = s.replace(m.group(0), new_layout, 1)

# array_stride 12 -> 28
old_stride = 'array_stride: 12,'
assert old_stride in s
s = s.replace(old_stride, 'array_stride: 28,', 1)

# ---- 3. render_mesh 拆出 shaded 版 ----
old_sig = '''    pub fn render_mesh(
        &self,
        positions: &[[f32; 3]],
        indices: &[u32],
        width: u32,
        height: u32,
    ) -> Result<RenderedImage, GpuError> {
        if positions.is_empty() || width == 0 || height == 0 {
            return Err(GpuError::Device("empty mesh or zero-sized target".into()));
        }
        let clip = normalize_to_clip(positions);

        let vertex_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vertices"),
                contents: bytemuck::cast_slice(&clip),
                usage: wgpu::BufferUsages::VERTEX,
            });'''
new_sig = '''    pub fn render_mesh(
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
            });'''
assert old_sig in s, 'render_mesh anchor'
s = s.replace(old_sig, new_sig, 1)

io.open(p, 'w', encoding='utf-8', newline=chr(10)).write(s)
print('gpu shading ok')
